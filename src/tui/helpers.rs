use std::io::{self, stdout};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::{App, EmailEntry, SearchHit, SearchTarget};

use crate::config::{all_configured_mailboxes, mailbox_dir, AccountConfig, ImapConfig};
use crate::draft::parse_email_draft;
use crate::imap_client::{
    fetch_emails_on_session, open_imap_session, parse_search_query, sync_mailboxes,
    MailboxState, MessageIdIndex, SyncTarget,
};
use crate::parse::FetchedEmail;
use crate::sync::mailbox_status;

// ---------------------------------------------------------------------------
// Watcher
// ---------------------------------------------------------------------------

pub(super) enum WatchEvent {
    Changed {
        account_index: usize,
    },
    Reconnected {
        account_index: usize,
    },
    Error {
        account_index: usize,
        message: String,
    },
}

pub(super) fn watcher_loop(
    tx: mpsc::Sender<WatchEvent>,
    imap_config: ImapConfig,
    account_index: usize,
) {
    use crate::imap_client::watch_mailbox as imap_watch;

    const BASE_BACKOFF_SECS: u64 = 30;
    const MAX_BACKOFF_SECS: u64 = 300;

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => {
            let _ = tx.send(WatchEvent::Error {
                account_index,
                message: "Failed to create async runtime".into(),
            });
            return;
        }
    };

    let mut consecutive_failures: u32 = 0;

    loop {
        match rt.block_on(imap_watch(&imap_config, "INBOX", Some(300))) {
            Ok(0) => {
                if consecutive_failures > 0 {
                    consecutive_failures = 0;
                    let _ = tx.send(WatchEvent::Reconnected { account_index });
                }
                if tx.send(WatchEvent::Changed { account_index }).is_err() {
                    break;
                }
            }
            Ok(2) => {
                if consecutive_failures > 0 {
                    consecutive_failures = 0;
                    let _ = tx.send(WatchEvent::Reconnected { account_index });
                }
                continue; // timeout, re-idle
            }
            Ok(_) | Err(_) => {
                // Only notify the UI on the first failure; subsequent retries are silent.
                if consecutive_failures == 0 {
                    let _ = tx.send(WatchEvent::Error {
                        account_index,
                        message: "Watch connection lost, retrying with backoff...".into(),
                    });
                }
                consecutive_failures = consecutive_failures.saturating_add(1);
                let backoff = (BASE_BACKOFF_SECS * 2u64.saturating_pow(consecutive_failures - 1))
                    .min(MAX_BACKOFF_SECS);
                std::thread::sleep(std::time::Duration::from_secs(backoff));
            }
        }
    }
}

pub(super) fn graph_watcher_loop(
    tx: mpsc::Sender<WatchEvent>,
    graph_config: crate::config::GraphConfig,
    account_index: usize,
) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => {
            let _ = tx.send(WatchEvent::Error {
                account_index,
                message: "Failed to create async runtime".into(),
            });
            return;
        }
    };

    let mut last_count: Option<usize> = None;
    let poll_interval = std::time::Duration::from_secs(60);

    loop {
        match rt.block_on(async {
            let client = crate::graph::GraphClient::new_async(&graph_config).await?;
            let ids = client.fetch_message_ids("inbox").await?;
            Ok::<usize, anyhow::Error>(ids.len())
        }) {
            Ok(count) => {
                if let Some(prev) = last_count {
                    if count != prev {
                        if tx.send(WatchEvent::Changed { account_index }).is_err() {
                            break;
                        }
                    }
                }
                last_count = Some(count);
            }
            Err(e) => {
                log::warn!(
                    "Graph watcher poll failed for account {}: {}",
                    account_index,
                    e
                );
            }
        }
        std::thread::sleep(poll_interval);
    }
}

// ---------------------------------------------------------------------------
// Terminal helpers
// ---------------------------------------------------------------------------

pub(super) fn suspend_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show,
    )?;
    disable_raw_mode()?;
    Ok(())
}

pub(super) fn resume_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}

pub(super) fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub(super) fn restore_terminal() -> Result<()> {
    execute!(stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

pub(super) fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        original_hook(panic_info);
    }));
}

// ---------------------------------------------------------------------------
// Editor / clipboard
// ---------------------------------------------------------------------------

fn editor() -> String {
    std::env::var("EDITOR").unwrap_or_else(|_| "hx".to_string())
}

pub(super) fn edit_file(path: &Path) -> Result<()> {
    let editor = editor();
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("Failed to launch editor: {}", editor))?;
    if !status.success() {
        anyhow::bail!("Editor exited with status: {}", status);
    }
    Ok(())
}

pub(super) fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("Failed to access clipboard")?;
    clipboard
        .set_text(text)
        .context("Failed to copy to clipboard")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Library call helpers
// ---------------------------------------------------------------------------

pub(super) async fn lib_do_sync(
    account_config: &AccountConfig,
    imap_config: &ImapConfig,
    limit: usize,
    reconcile: bool,
    known_index: Option<&mut MessageIdIndex>,
    prev_states: Option<&std::collections::HashMap<String, MailboxState>>,
) -> anyhow::Result<(String, SyncResultMeta)> {
    let span_label = if limit < usize::MAX { "lib_do_sync:quick" } else { "lib_do_sync:full" };
    let _span = crate::timing::TimingSpan::with_context(span_label, account_config.name.clone());

    let targets: Vec<SyncTarget> = all_configured_mailboxes(account_config)
        .iter()
        .map(|(role, mapping)| SyncTarget {
            role: role.clone(),
            server_name: mapping.server.clone(),
            local_dir: mailbox_dir(&account_config.name, role),
            status: mailbox_status(role).to_string(),
        })
        .collect();

    let result = sync_mailboxes(imap_config, &targets, limit, reconcile, false, known_index, prev_states).await?;

    // Incremental contacts-index update (best-effort, no-op if no cache).
    crate::contacts::hooks::bump_after_sync(account_config, &result.fresh_observations);

    let mut msg = format!("Synced: {} new, {} existing", result.saved, result.skipped);
    if result.read_updated > 0 {
        msg.push_str(&format!(", {} read status updated", result.read_updated));
    }
    if result.deduped > 0 {
        msg.push_str(&format!(", {} duplicates removed", result.deduped));
    }
    if reconcile {
        if result.moved > 0 || result.removed > 0 {
            msg.push_str(&format!(
                " | Reconciled: {} moved, {} removed",
                result.moved, result.removed
            ));
        } else {
            msg.push_str(" | Already in sync");
        }
    }
    let meta = SyncResultMeta {
        mailbox_states: result.mailbox_states,
        touched_dirs: result.touched_dirs,
        new_inbox_mail: result.new_inbox_mail,
    };
    Ok((msg, meta))
}

/// Metadata returned alongside the status message from lib_do_sync.
/// Carries state that needs to be persisted back to AccountState.
pub(super) struct SyncResultMeta {
    pub mailbox_states: std::collections::HashMap<String, MailboxState>,
    /// Local mailbox directories the sync actually modified on disk.
    /// Empty when the sync was a no-op (lets the UI skip cache
    /// invalidation and mailbox reload entirely).
    pub touched_dirs: Vec<std::path::PathBuf>,
    /// Sender + subject of every genuinely new inbox email this sync
    /// saved, for the desktop notification (#0009).
    pub new_inbox_mail: Vec<crate::notify::NewMailMeta>,
}

pub(super) async fn lib_do_multi_search(
    imap_config: &ImapConfig,
    query: &str,
    targets: &[SearchTarget],
) -> anyhow::Result<Vec<SearchHit>> {
    let mut criteria = parse_search_query(query);
    criteria.in_mailbox = None;

    let mut session = open_imap_session(imap_config).await?;
    let total_limit = 50usize;
    let per_mb = (total_limit / targets.len().max(1)).max(5);
    let mut total = 0usize;

    let mut hits: Vec<SearchHit> = Vec::new();

    for target in targets {
        if total >= total_limit {
            break;
        }
        let budget = per_mb.min(total_limit - total);
        log::info!(
            "Server search: querying mailbox '{}' (label={}, local_dir={}, status={})",
            target.server_name,
            target.label,
            target.local_dir.display(),
            target.status,
        );
        match fetch_emails_on_session(&mut session, &criteria, &target.server_name, Some(budget))
            .await
        {
            Ok(emails) => {
                log::info!(
                    "Server search: '{}' returned {} result(s)",
                    target.server_name,
                    emails.len(),
                );
                total += emails.len();
                for fetched in emails {
                    let entry = fetched_to_email_entry(&fetched);
                    hits.push(SearchHit {
                        entry,
                        fetched,
                        source_label: target.label.clone(),
                        source_local_dir: target.local_dir.clone(),
                        source_status: target.status.clone(),
                    });
                }
            }
            Err(e) => {
                log::warn!("Search in {} failed: {}", target.server_name, e);
            }
        }
    }

    session.logout().await.ok();

    hits.sort_by(|a, b| b.entry.date_sort.cmp(&a.entry.date_sort));

    Ok(hits)
}

pub(super) async fn lib_do_sync_graph(
    account_config: &AccountConfig,
    graph_config: &crate::config::GraphConfig,
    limit: usize,
    reconcile: bool,
) -> anyhow::Result<(String, GraphSyncMeta)> {
    let targets: Vec<SyncTarget> = all_configured_mailboxes(account_config)
        .iter()
        .map(|(role, mapping)| SyncTarget {
            role: role.clone(),
            server_name: mapping.server.clone(),
            local_dir: mailbox_dir(&account_config.name, role),
            status: mailbox_status(role).to_string(),
        })
        .collect();

    let result =
        crate::graph::sync_mailboxes_graph(graph_config, &targets, limit, reconcile, false)
            .await?;

    crate::contacts::hooks::bump_after_sync(account_config, &result.fresh_observations);

    let mut msg = format!("Synced: {} new, {} existing", result.saved, result.skipped);
    if result.read_updated > 0 {
        msg.push_str(&format!(", {} read status updated", result.read_updated));
    }
    if result.deduped > 0 {
        msg.push_str(&format!(", {} duplicates removed", result.deduped));
    }
    if reconcile {
        if result.moved > 0 || result.removed > 0 {
            msg.push_str(&format!(
                " | Reconciled: {} moved, {} removed",
                result.moved, result.removed
            ));
        } else {
            msg.push_str(" | Already in sync");
        }
    }
    let meta = GraphSyncMeta {
        touched_dirs: result.touched_dirs,
        new_inbox_mail: result.new_inbox_mail,
    };
    Ok((msg, meta))
}

/// Metadata returned alongside the status message from `lib_do_sync_graph`.
pub(super) struct GraphSyncMeta {
    pub touched_dirs: Vec<std::path::PathBuf>,
    /// Sender + subject of every genuinely new inbox email this sync
    /// saved, for the desktop notification (#0009).
    pub new_inbox_mail: Vec<crate::notify::NewMailMeta>,
}

pub(super) async fn lib_do_multi_search_graph(
    graph_config: &crate::config::GraphConfig,
    query: &str,
    targets: &[SearchTarget],
) -> anyhow::Result<Vec<SearchHit>> {
    let mut criteria = parse_search_query(query);
    criteria.in_mailbox = None;

    let client = crate::graph::GraphClient::new_async(graph_config).await?;
    let total_limit = 50usize;
    let per_mb = (total_limit / targets.len().max(1)).max(5);
    let mut total = 0usize;
    let mut hits: Vec<SearchHit> = Vec::new();

    for target in targets {
        if total >= total_limit {
            break;
        }
        let budget = per_mb.min(total_limit - total);
        match client
            .search_messages(&criteria, Some(&target.server_name), budget)
            .await
        {
            Ok(emails) => {
                total += emails.len();
                for fetched in emails {
                    let entry = fetched_to_email_entry(&fetched);
                    hits.push(SearchHit {
                        entry,
                        fetched,
                        source_label: target.label.clone(),
                        source_local_dir: target.local_dir.clone(),
                        source_status: target.status.clone(),
                    });
                }
            }
            Err(e) => {
                log::warn!("Graph search in {} failed: {}", target.server_name, e);
            }
        }
    }

    hits.sort_by(|a, b| b.entry.date_sort.cmp(&a.entry.date_sort));
    Ok(hits)
}

fn fetched_to_email_entry(fetched: &FetchedEmail) -> EmailEntry {
    let (date_display, date_sort) =
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&fetched.date) {
            (
                dt.format("%Y-%m-%d").to_string(),
                dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
            )
        } else {
            (
                fetched.date.chars().take(10).collect(),
                fetched.date.clone(),
            )
        };

    EmailEntry {
        path: PathBuf::new(),
        from: fetched.from.clone(),
        to: fetched.to.clone(),
        cc: fetched.cc.clone(),
        subject: fetched.subject.clone(),
        status: String::new(),
        date_display,
        date_sort,
        body: fetched.body_text.clone(),
        has_attachments: fetched.has_attachments,
        read: fetched.is_read,
        event: fetched.event.clone(),
    }
}

// ---------------------------------------------------------------------------
// Account resolution for Send
// ---------------------------------------------------------------------------

pub(super) fn resolve_send_account(
    app: &App,
    draft_path: &Path,
) -> (
    usize,
    Option<crate::config::SmtpConfig>,
    Option<crate::config::ImapConfig>,
    Option<crate::config::GraphConfig>,
    AccountConfig,
    Option<String>,
    Option<PathBuf>,
) {
    if let Ok(draft) = parse_email_draft(draft_path) {
        let from = draft.frontmatter.from.unwrap_or_default().to_lowercase();
        for (i, acct) in app.accounts.iter().enumerate() {
            if from.contains(&acct.account_config.default_from.to_lowercase()) {
                return (
                    i,
                    acct.smtp_config.clone(),
                    acct.imap_config.clone(),
                    acct.graph_config.clone(),
                    acct.account_config.clone(),
                    acct.signature_content.clone(),
                    acct.sent_dir.clone(),
                );
            }
        }
    }
    let acct = &app.accounts[app.active_account];
    (
        app.active_account,
        acct.smtp_config.clone(),
        acct.imap_config.clone(),
        acct.graph_config.clone(),
        acct.account_config.clone(),
        acct.signature_content.clone(),
        acct.sent_dir.clone(),
    )
}

// ---------------------------------------------------------------------------
// Search result helpers
// ---------------------------------------------------------------------------

pub(super) fn ensure_search_result_saved(app: &mut App) -> Option<PathBuf> {
    let idx = app.server_search_index;
    let result = app.server_search_results.get(idx)?;

    if let Some(ref path) = result.saved_path {
        return Some(path.clone());
    }

    let save_dir = result.source_local_dir.clone();
    let status = result.source_status.clone();

    log::info!(
        "Search result save: source_label={}, status={}, dir={}, subject={}",
        result.source_label,
        status,
        save_dir.display(),
        result.entry.subject,
    );

    let fetched_clone = result.fetched.clone();
    let message_id = result.fetched.message_id.clone();

    let mut known_ids = crate::parse::scan_existing_message_ids(&save_dir).unwrap_or_default();
    match crate::parse::save_fetched_emails_with_known_ids(
        &[fetched_clone],
        &save_dir,
        &status,
        &mut known_ids,
    ) {
        Ok((saved, _skipped, saved_paths)) => {
            if saved > 0 {
                if let Some((_mid, path)) = saved_paths.into_iter().next() {
                    let result = app.server_search_results.get_mut(idx)?;
                    result.saved_path = Some(path.clone());
                    if let Some(cache_idx) = app.mailbox_index_for_dir(&save_dir) {
                        app.invalidate_cache_idx(cache_idx);
                    }
                    return Some(path);
                }
                if let Some(cache_idx) = app.mailbox_index_for_dir(&save_dir) {
                    app.invalidate_cache_idx(cache_idx);
                }
                None
            } else {
                // Skipped as duplicate: the email already exists on disk, so no
                // path was returned. Look it up by message_id in frontmatter
                // (never by whole-file substring, which could false-match an
                // ID quoted in another email's body).
                if let Some(ref mid) = message_id {
                    if let Ok(ids) = crate::parse::scan_mailbox_message_ids(&save_dir) {
                        if let Some(path) = ids.get(mid) {
                            let path = path.clone();
                            let result = app.server_search_results.get_mut(idx)?;
                            result.saved_path = Some(path.clone());
                            return Some(path);
                        }
                    }
                }
                None
            }
        }
        Err(_) => None,
    }
}
