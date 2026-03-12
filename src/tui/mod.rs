pub mod app;
mod event;
mod theme;
mod ui;

use std::collections::{HashMap, HashSet};
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

use app::{Action, App, BgResult, MailboxKind};

use crate::config::{
    all_configured_mailboxes, resolve_mailbox_local_path, resolve_sent_mailbox, ImapConfig,
};
use crate::draft::{
    create_reply_draft, find_drafts, mark_as_approved, parse_email_draft, update_status_to_sent,
    validate_draft,
};
use crate::imap_client::{
    append_to_sent_folder, archive_email_locally, batch_archive_emails_locally,
    batch_delete_emails_locally, delete_email_locally, fetch_emails, fetch_server_message_ids,
    watch_mailbox as imap_watch, FetchCriteria,
};
use crate::parse::save_fetched_emails;
use crate::send::send_email;
use crate::sync::{mailbox_status, reconcile_local_files};
use crate::types::EmailStatus;

enum WatchEvent {
    Changed,
    Error(String),
}

/// Entry point for the TUI. Call this when `email` is invoked with no arguments.
pub fn run() -> Result<()> {
    install_panic_hook();
    let mut terminal = init_terminal()?;
    let result = run_loop(&mut terminal);
    restore_terminal()?;
    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();

    let size = terminal.size()?;
    app.terminal_width = size.width;
    app.terminal_height = size.height;

    // Spawn background mail watcher thread
    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>();
    if let Some(watcher_imap) = app.imap_config.clone() {
        app.watcher_active = true;
        std::thread::spawn(move || {
            watcher_loop(watch_tx, watcher_imap);
        });
    }

    // Background task results channel
    let (bg_tx, bg_rx) = mpsc::channel::<BgResult>();

    while app.running {
        terminal.draw(|frame| ui::view(&app, frame))?;

        if let Some(msg) = event::poll_event()? {
            let mut current_msg = Some(msg);
            while let Some(m) = current_msg {
                current_msg = app.update(m);
            }
        } else {
            app.tick_status();
            if app.bg_count > 0 {
                app.bg_spin_tick = app.bg_spin_tick.wrapping_add(1);
            }
        }

        // Check background watcher
        match watch_rx.try_recv() {
            Ok(WatchEvent::Changed) => {
                let mut current_msg = Some(app::Message::MailboxChanged);
                while let Some(m) = current_msg {
                    current_msg = app.update(m);
                }
            }
            Ok(WatchEvent::Error(e)) => {
                app.set_status(format!("Watch: {e}"));
                app.watcher_active = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                app.watcher_active = false;
            }
        }

        // Check background task results (drain all available)
        while let Ok(result) = bg_rx.try_recv() {
            handle_bg_result(&mut app, result);
        }

        // Auto-execute queued action when all mutations complete
        if app.bg_mutations == 0 {
            if let Some(action) = app.queued_action.take() {
                app.pending_action = Some(action);
            }
        }

        // Process pending action (side-effects outside the pure update)
        if let Some(action) = app.pending_action.take() {
            handle_action(&mut app, terminal, action, &bg_tx)?;
        }
    }

    Ok(())
}

fn handle_action(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: Action,
    bg_tx: &mpsc::Sender<BgResult>,
) -> Result<()> {
    match action {
        Action::EditCurrent => {
            if let Some(path) = app.selected_email_path() {
                suspend_terminal(terminal)?;
                let result = edit_file(&path);
                resume_terminal(terminal)?;
                match result {
                    Ok(()) => app.set_status("Returned from editor".to_string()),
                    Err(e) => app.set_status(format!("Edit failed: {e}")),
                }
                app.reload_current_mailbox();
            }
        }

        Action::Reply(reply_all) => {
            if let Some(path) = app.selected_email_path() {
                let default_from = app.smtp_config.as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_default();
                let drafts_dir = app.drafts_dir.clone();
                match create_reply_draft(&path, reply_all, &default_from, drafts_dir.as_deref()) {
                    Ok(draft_path) => {
                        suspend_terminal(terminal)?;
                        let _ = edit_file(&draft_path);
                        resume_terminal(terminal)?;
                        app.set_status("Reply draft ready".to_string());
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                            app.invalidate_cache_idx(idx);
                        }
                    }
                    Err(e) => app.set_status(format!("Reply failed: {e}")),
                }
                app.reload_current_mailbox();
            }
        }

        Action::Send => {
            if let Some(path) = app.selected_email_path() {
                let smtp_config = match app.smtp_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status("SMTP not configured".to_string());
                        return Ok(());
                    }
                };
                let global_config = app.global_config.clone();
                let imap_config = app.imap_config.clone();
                let signature = app.signature_content.clone();
                let sent_dir = app.sent_dir.clone();

                app.bg_count += 1;
                app.set_status("Sending...".to_string());
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<String> {
                        let draft = parse_email_draft(&path)?;
                        validate_draft(&draft)?;

                        let rt = tokio::runtime::Runtime::new()?;
                        let (send_result, raw_message, message_id) = rt.block_on(
                            send_email(&draft, &smtp_config, &global_config, signature.as_deref()),
                        )?;

                        if send_result.all_succeeded() || send_result.any_succeeded() {
                            update_status_to_sent(
                                &draft,
                                sent_dir.as_deref(),
                                message_id.as_deref(),
                            )?;
                            if let Some(ref imap_cfg) = imap_config {
                                let sent_mailbox = resolve_sent_mailbox(&global_config);
                                let _ =
                                    append_to_sent_folder(imap_cfg, &raw_message, &sent_mailbox);
                            }
                            if send_result.all_succeeded() {
                                Ok(format!(
                                    "Sent to {} recipient(s)",
                                    send_result.results.len()
                                ))
                            } else {
                                Ok(format!(
                                    "Partial: {}/{} succeeded",
                                    send_result.succeeded().len(),
                                    send_result.results.len()
                                ))
                            }
                        } else {
                            anyhow::bail!(
                                "Failed to send to all {} recipient(s)",
                                send_result.results.len()
                            )
                        }
                    })();
                    let _ = tx.send(BgResult::Send {
                        result: result.map_err(|e| e.to_string()),
                    });
                });
            }
        }

        Action::SendApproved => {
            if let Some(dir) = app.active_dir().cloned() {
                let smtp_config = match app.smtp_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status("SMTP not configured".to_string());
                        return Ok(());
                    }
                };
                let global_config = app.global_config.clone();
                let imap_config = app.imap_config.clone();
                let signature = app.signature_content.clone();
                let sent_dir = app.sent_dir.clone();

                app.bg_count += 1;
                app.set_status("Sending approved...".to_string());
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let result = (|| -> anyhow::Result<String> {
                        let drafts = find_drafts(&dir, Some(EmailStatus::Approved))?;
                        if drafts.is_empty() {
                            return Ok("No approved emails found".to_string());
                        }

                        let rt = tokio::runtime::Runtime::new()?;
                        let mut sent = 0usize;
                        let mut failed = 0usize;

                        for draft in &drafts {
                            match rt.block_on(send_email(
                                draft,
                                &smtp_config,
                                &global_config,
                                signature.as_deref(),
                            )) {
                                Ok((send_result, raw_message, message_id)) => {
                                    if send_result.all_succeeded() || send_result.any_succeeded() {
                                        let _ = update_status_to_sent(
                                            draft,
                                            sent_dir.as_deref(),
                                            message_id.as_deref(),
                                        );
                                        if let Some(ref imap_cfg) = imap_config {
                                            let sent_mailbox =
                                                resolve_sent_mailbox(&global_config);
                                            let _ = append_to_sent_folder(
                                                imap_cfg,
                                                &raw_message,
                                                &sent_mailbox,
                                            );
                                        }
                                        sent += 1;
                                    } else {
                                        failed += 1;
                                    }
                                }
                                Err(_) => failed += 1,
                            }
                        }

                        Ok(format!("{} sent, {} failed", sent, failed))
                    })();
                    let _ = tx.send(BgResult::SendApproved {
                        result: result.map_err(|e| e.to_string()),
                    });
                });
            }
        }

        Action::NewDraft => {
            let name = chrono::Local::now().format("draft-%Y%m%d-%H%M%S").to_string();
            let file_name = format!("{name}.md");
            let dir = app
                .find_mailbox_by_kind(MailboxKind::Drafts)
                .map(|i| app.mailboxes[i].dir.clone())
                .or_else(|| app.drafts_dir.clone())
                .unwrap_or_else(|| PathBuf::from("."));
            let path = dir.join(&file_name);

            if path.exists() {
                app.set_status(format!("File already exists: {}", path.display()));
            } else {
                let skeleton = "---\nto:\ncc:\nbcc:\nsubject:\nstatus: draft\nfrom:\nreply_to:\nattachments: []\n---\n\n";
                match std::fs::write(&path, skeleton) {
                    Ok(()) => {
                        suspend_terminal(terminal)?;
                        let _ = edit_file(&path);
                        resume_terminal(terminal)?;
                        app.set_status(format!("Created: {}", file_name));
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                            app.invalidate_cache_idx(idx);
                        }
                        app.reload_current_mailbox();
                    }
                    Err(e) => app.set_status(format!("New draft failed: {e}")),
                }
            }
        }

        Action::Approve => {
            if let Some(path) = app.selected_email_path() {
                match mark_as_approved(&path) {
                    Ok(msg) => {
                        app.set_status(msg);
                        app.reload_current_mailbox();
                    }
                    Err(e) => app.set_status(format!("Approve failed: {e}")),
                }
            }
        }

        Action::Archive => {
            if let Some(path) = app.selected_email_path() {
                let imap_config = match app.imap_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status("IMAP not configured".to_string());
                        return Ok(());
                    }
                };
                let archive_dir = match app.archive_dir.clone() {
                    Some(d) => d,
                    None => {
                        app.set_status("Archive directory not configured".to_string());
                        return Ok(());
                    }
                };
                let archive_server_name = app.archive_server_name.clone();

                app.remove_selected_from_list();
                app.bg_count += 1;
                app.bg_mutations += 1;
                app.set_status("Archiving...".to_string());
                terminal.draw(|frame| ui::view(app, frame))?;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let result = archive_email_locally(
                        &imap_config,
                        &archive_dir,
                        &path,
                        &archive_server_name,
                    )
                    .map(|()| String::new())
                    .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Archive { result });
                });
            }
        }

        Action::Delete => {
            if let Some(path) = app.selected_email_path() {
                let imap_config = match app.imap_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status("IMAP not configured".to_string());
                        return Ok(());
                    }
                };

                app.remove_selected_from_list();
                app.bg_count += 1;
                app.bg_mutations += 1;
                app.set_status("Deleting...".to_string());
                terminal.draw(|frame| ui::view(app, frame))?;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let result = delete_email_locally(&imap_config, &path)
                        .map(|()| String::new())
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Delete { result });
                });
            }
        }

        Action::BatchArchive(paths) => {
            let imap_config = match app.imap_config.clone() {
                Some(c) => c,
                None => {
                    app.set_status("IMAP not configured".to_string());
                    return Ok(());
                }
            };
            let archive_dir = match app.archive_dir.clone() {
                Some(d) => d,
                None => {
                    app.set_status("Archive directory not configured".to_string());
                    return Ok(());
                }
            };
            let archive_server_name = app.archive_server_name.clone();

            let path_set: HashSet<PathBuf> = paths.iter().cloned().collect();
            app.remove_selected_from_list_batch(&path_set);

            let count = paths.len();
            app.bg_count += count;
            app.bg_mutations += count;
            app.set_status(format!("Archiving {} emails...", count));
            terminal.draw(|frame| ui::view(app, frame))?;

            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let results = batch_archive_emails_locally(
                    &imap_config,
                    &archive_dir,
                    &paths,
                    &archive_server_name,
                );
                for (_path, result) in results {
                    let _ = tx.send(BgResult::Archive {
                        result: result.map(|()| String::new()).map_err(|e| e.to_string()),
                    });
                }
            });
        }

        Action::BatchDelete(paths) => {
            let imap_config = match app.imap_config.clone() {
                Some(c) => c,
                None => {
                    app.set_status("IMAP not configured".to_string());
                    return Ok(());
                }
            };

            let path_set: HashSet<PathBuf> = paths.iter().cloned().collect();
            app.remove_selected_from_list_batch(&path_set);

            let count = paths.len();
            app.bg_count += count;
            app.bg_mutations += count;
            app.set_status(format!("Deleting {} emails...", count));
            terminal.draw(|frame| ui::view(app, frame))?;

            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let results = batch_delete_emails_locally(&imap_config, &paths);
                for (_path, result) in results {
                    let _ = tx.send(BgResult::Delete {
                        result: result.map(|()| String::new()).map_err(|e| e.to_string()),
                    });
                }
            });
        }

        Action::CopyPath => {
            if let Some(path) = app.selected_email_path() {
                match copy_to_clipboard(&path.display().to_string()) {
                    Ok(()) => app.set_status("Path copied to clipboard".to_string()),
                    Err(e) => app.set_status(format!("Copy failed: {e}")),
                }
            }
        }

        Action::Fetch => {
            if app.bg_mutations > 0 {
                app.queued_action = Some(Action::Fetch);
                app.set_status(format!(
                    "Fetch queued ({} ops pending...)",
                    app.bg_mutations
                ));
                return Ok(());
            }
            let imap_config = match app.imap_config.clone() {
                Some(c) => c,
                None => {
                    app.set_status("IMAP not configured".to_string());
                    return Ok(());
                }
            };
            let global_config = app.global_config.clone();

            app.bg_count += 1;
            app.set_status("Fetching...".to_string());
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let result =
                    lib_sync(&global_config, &imap_config, 10).map_err(|e| e.to_string());
                let _ = tx.send(BgResult::Fetch { result });
            });
        }

        Action::Sync => {
            if app.bg_mutations > 0 {
                app.queued_action = Some(Action::Sync);
                app.set_status(format!(
                    "Sync queued ({} ops pending...)",
                    app.bg_mutations
                ));
                return Ok(());
            }
            let imap_config = match app.imap_config.clone() {
                Some(c) => c,
                None => {
                    app.set_status("IMAP not configured".to_string());
                    return Ok(());
                }
            };
            let global_config = app.global_config.clone();

            app.bg_count += 1;
            app.set_status("Syncing...".to_string());
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let result = lib_sync_reconcile(&global_config, &imap_config, 50)
                    .map_err(|e| e.to_string());
                let _ = tx.send(BgResult::Sync { result });
            });
        }
    }

    Ok(())
}

fn handle_bg_result(app: &mut App, result: BgResult) {
    app.bg_count = app.bg_count.saturating_sub(1);

    match result {
        BgResult::Archive { result } => {
            app.bg_mutations = app.bg_mutations.saturating_sub(1);
            match result {
                Ok(msg) => {
                    app.set_status(if msg.is_empty() { "Email archived".into() } else { msg });
                    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Archive) {
                        app.invalidate_cache_idx(idx);
                    }
                }
                Err(e) => {
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                    app.set_persistent_error(format!(
                        "Archive failed: {e}\nEmail restored to inbox. Sync (F) to fix?"
                    ));
                }
            }
        }

        BgResult::Delete { result } => {
            app.bg_mutations = app.bg_mutations.saturating_sub(1);
            match result {
                Ok(msg) => {
                    app.set_status(if msg.is_empty() { "Email deleted".into() } else { msg });
                }
                Err(e) => {
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                    app.set_persistent_error(format!(
                        "Delete failed: {e}\nEmail restored. Sync (F) to fix?"
                    ));
                }
            }
        }

        BgResult::Send { result } => {
            match result {
                Ok(msg) => {
                    app.set_status(if msg.is_empty() { "Email sent".into() } else { msg });
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status(format!("Send failed: {e}")),
            }
        }

        BgResult::SendApproved { result } => {
            match result {
                Ok(msg) => {
                    app.set_status(if msg.is_empty() { "Approved emails sent".into() } else { msg });
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status(format!("Send-approved failed: {e}")),
            }
        }

        BgResult::Fetch { result } => {
            match result {
                Ok(msg) => {
                    app.set_status(if msg.is_empty() { "Fetch complete".into() } else { msg });
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status(format!("Fetch failed: {e}")),
            }
        }

        BgResult::Sync { result } => {
            match result {
                Ok(msg) => {
                    app.set_status(if msg.is_empty() { "Sync complete".into() } else { msg });
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status(format!("Sync failed: {e}")),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal helpers
// ---------------------------------------------------------------------------

fn suspend_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn resume_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(())
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() -> Result<()> {
    execute!(stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        original_hook(panic_info);
    }));
}

fn watcher_loop(tx: mpsc::Sender<WatchEvent>, imap_config: ImapConfig) {
    loop {
        match imap_watch(&imap_config, "INBOX", Some(300)) {
            Ok(0) => {
                if tx.send(WatchEvent::Changed).is_err() {
                    break;
                }
            }
            Ok(2) => continue, // timeout, re-idle
            Ok(_) => {
                let _ = tx.send(WatchEvent::Error("Watch connection lost".into()));
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            Err(_) => {
                let _ = tx.send(WatchEvent::Error("Watch connection lost".into()));
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Direct library call helpers
// ---------------------------------------------------------------------------

fn lib_sync(
    global_config: &crate::config::GlobalConfig,
    imap_config: &ImapConfig,
    limit: usize,
) -> anyhow::Result<String> {
    let sync_targets: Vec<(String, String, PathBuf, &str)> = all_configured_mailboxes(global_config)
        .iter()
        .map(|(role, mapping)| {
            let local_dir = resolve_mailbox_local_path(global_config, mapping);
            let status_str = mailbox_status(role);
            (role.clone(), mapping.server.clone(), local_dir, status_str)
        })
        .collect();

    let mut total_saved = 0usize;
    let mut total_skipped = 0usize;

    for (_, imap_mailbox, local_dir, status_str) in &sync_targets {
        let criteria = FetchCriteria {
            from: None,
            to: None,
            cc: None,
            subject: None,
            body: None,
            since: None,
            before: None,
        };
        let emails = fetch_emails(imap_config, &criteria, imap_mailbox, Some(limit))?;
        let (saved, skipped) = save_fetched_emails(&emails, local_dir, status_str)?;
        total_saved += saved;
        total_skipped += skipped;
    }

    Ok(format!("Synced: {} new, {} existing", total_saved, total_skipped))
}

fn lib_sync_reconcile(
    global_config: &crate::config::GlobalConfig,
    imap_config: &ImapConfig,
    limit: usize,
) -> anyhow::Result<String> {
    let sync_msg = lib_sync(global_config, imap_config, limit)?;

    let mut reconcile_pairs: Vec<(String, String, PathBuf)> = Vec::new();
    if let Some(ref m) = global_config.mailboxes.inbox {
        reconcile_pairs.push((
            "INBOX".to_string(),
            m.server.clone(),
            resolve_mailbox_local_path(global_config, m),
        ));
    }
    if let Some(ref m) = global_config.mailboxes.archive {
        reconcile_pairs.push((
            "Archive".to_string(),
            m.server.clone(),
            resolve_mailbox_local_path(global_config, m),
        ));
    }

    let mut server_ids: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut local_dirs: HashMap<String, PathBuf> = HashMap::new();

    for (role, imap_mailbox, local_dir) in &reconcile_pairs {
        local_dirs.insert(role.clone(), local_dir.clone());
        let ids = fetch_server_message_ids(imap_config, imap_mailbox)?;
        server_ids.insert(role.clone(), ids);
    }

    let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs)?;
    if moved > 0 || removed > 0 {
        Ok(format!(
            "{} | Reconciled: {} moved, {} removed",
            sync_msg, moved, removed
        ))
    } else {
        Ok(format!("{} | Already in sync", sync_msg))
    }
}

// ---------------------------------------------------------------------------
// Utilities (kept from Phase 2)
// ---------------------------------------------------------------------------

fn editor() -> String {
    std::env::var("EDITOR").unwrap_or_else(|_| "hx".to_string())
}

fn edit_file(path: &Path) -> Result<()> {
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

fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut clipboard =
        arboard::Clipboard::new().context("Failed to access clipboard")?;
    clipboard
        .set_text(text)
        .context("Failed to copy to clipboard")?;
    Ok(())
}
