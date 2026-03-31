pub mod app;
mod event;
mod theme;
mod ui;

use std::collections::HashSet;
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

use app::{Action, App, BgResult, MailboxKind, StatusLevel};

use crate::config::{
    all_configured_mailboxes, resolve_mailbox_local_path, resolve_sent_mailbox, ImapConfig,
};
use crate::draft::{
    create_forward_draft, create_reply_draft, find_drafts, mark_as_approved, parse_email_draft,
    update_status_to_sent, validate_draft,
};
use crate::imap_client::{
    append_to_sent_folder, archive_email_locally, batch_archive_emails_locally,
    batch_delete_emails_locally, delete_email_locally, parse_search_query,
    sync_mailboxes, SyncTarget, watch_mailbox as imap_watch,
};
use crate::parse::FetchedEmail;
use crate::send::send_email;
use crate::sync::mailbox_status;
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
        terminal.draw(|frame| ui::view(&mut app, frame))?;

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
                    Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
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
                    Err(e) => app.set_status_level(format!("Reply failed: {e}"), StatusLevel::Error),
                }
                app.reload_current_mailbox();
            }
        }

        Action::Forward => {
            if let Some(path) = app.selected_email_path() {
                let default_from = app.smtp_config.as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_default();
                let drafts_dir = app.drafts_dir.clone();
                match create_forward_draft(&path, &default_from, drafts_dir.as_deref()) {
                    Ok(draft_path) => {
                        suspend_terminal(terminal)?;
                        let _ = edit_file(&draft_path);
                        resume_terminal(terminal)?;
                        app.set_status("Forward draft ready".to_string());
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                            app.invalidate_cache_idx(idx);
                        }
                    }
                    Err(e) => app.set_status_level(format!("Forward failed: {e}"), StatusLevel::Error),
                }
                app.reload_current_mailbox();
            }
        }

        Action::Send => {
            if let Some(path) = app.selected_email_path() {
                let smtp_config = match app.smtp_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status_level("SMTP not configured".to_string(), StatusLevel::Error);
                        return Ok(());
                    }
                };
                let global_config = app.global_config.clone();
                let imap_config = app.imap_config.clone();
                let signature = app.signature_content.clone();
                let sent_dir = app.sent_dir.clone();

                app.bg_count += 1;
                app.set_status_level("Sending...".to_string(), StatusLevel::Progress);
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = (|| -> anyhow::Result<String> {
                        let draft = parse_email_draft(&path)?;
                        validate_draft(&draft)?;

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
                                let _ = rt.block_on(
                                    append_to_sent_folder(imap_cfg, &raw_message, &sent_mailbox),
                                );
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
                        app.set_status_level("SMTP not configured".to_string(), StatusLevel::Error);
                        return Ok(());
                    }
                };
                let global_config = app.global_config.clone();
                let imap_config = app.imap_config.clone();
                let signature = app.signature_content.clone();
                let sent_dir = app.sent_dir.clone();

                app.bg_count += 1;
                app.set_status_level("Sending approved...".to_string(), StatusLevel::Progress);
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = (|| -> anyhow::Result<String> {
                        let drafts = find_drafts(&dir, Some(EmailStatus::Approved))?;
                        if drafts.is_empty() {
                            return Ok("No approved emails found".to_string());
                        }

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
                                            let _ = rt.block_on(append_to_sent_folder(
                                                imap_cfg,
                                                &raw_message,
                                                &sent_mailbox,
                                            ));
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
                    Err(e) => app.set_status_level(format!("New draft failed: {e}"), StatusLevel::Error),
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
                    Err(e) => app.set_status_level(format!("Approve failed: {e}"), StatusLevel::Error),
                }
            }
        }

        Action::Archive => {
            if let Some(path) = app.selected_email_path() {
                let imap_config = match app.imap_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                        return Ok(());
                    }
                };
                let archive_dir = match app.archive_dir.clone() {
                    Some(d) => d,
                    None => {
                        app.set_status_level("Archive directory not configured".to_string(), StatusLevel::Error);
                        return Ok(());
                    }
                };
                let archive_server_name = app.archive_server_name.clone();

                app.remove_selected_from_list();
                app.bg_count += 1;
                app.bg_mutations += 1;
                app.set_status_level("Archiving...".to_string(), StatusLevel::Progress);
                terminal.draw(|frame| ui::view(app, frame))?;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = rt.block_on(archive_email_locally(
                        &imap_config,
                        &archive_dir,
                        &path,
                        &archive_server_name,
                    ))
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
                        app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                        return Ok(());
                    }
                };

                app.remove_selected_from_list();
                app.bg_count += 1;
                app.bg_mutations += 1;
                app.set_status_level("Deleting...".to_string(), StatusLevel::Progress);
                terminal.draw(|frame| ui::view(app, frame))?;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = rt.block_on(delete_email_locally(&imap_config, &path))
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
                    app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                    return Ok(());
                }
            };
            let archive_dir = match app.archive_dir.clone() {
                Some(d) => d,
                None => {
                    app.set_status_level("Archive directory not configured".to_string(), StatusLevel::Error);
                    return Ok(());
                }
            };
            let archive_server_name = app.archive_server_name.clone();

            let path_set: HashSet<PathBuf> = paths.iter().cloned().collect();
            app.remove_selected_from_list_batch(&path_set);

            let count = paths.len();
            app.bg_count += count;
            app.bg_mutations += count;
            app.set_status_level(format!("Archiving {} emails...", count), StatusLevel::Progress);
            terminal.draw(|frame| ui::view(app, frame))?;

            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let results = rt.block_on(batch_archive_emails_locally(
                    &imap_config,
                    &archive_dir,
                    &paths,
                    &archive_server_name,
                ));
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
                    app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                    return Ok(());
                }
            };

            let path_set: HashSet<PathBuf> = paths.iter().cloned().collect();
            app.remove_selected_from_list_batch(&path_set);

            let count = paths.len();
            app.bg_count += count;
            app.bg_mutations += count;
            app.set_status_level(format!("Deleting {} emails...", count), StatusLevel::Progress);
            terminal.draw(|frame| ui::view(app, frame))?;

            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let results = rt.block_on(batch_delete_emails_locally(&imap_config, &paths));
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
                    Err(e) => app.set_status_level(format!("Copy failed: {e}"), StatusLevel::Error),
                }
            }
        }

        Action::OpenAttachment(path) => {
            match crate::parse::open_file_with_system(&path) {
                Ok(()) => {
                    let name = path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    app.set_status(format!("Opened: {name}"));
                }
                Err(e) => {
                    app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
                }
            }
        }

        Action::OpenHtmlInBrowser(path) => {
            match crate::parse::open_file_with_system(&path) {
                Ok(()) => {
                    app.set_status("Opened in browser".to_string());
                }
                Err(e) => {
                    app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
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
                    app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                    return Ok(());
                }
            };
            let global_config = app.global_config.clone();

            app.bg_count += 1;
            app.set_status_level("Fetching...".to_string(), StatusLevel::Progress);
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result =
                    rt.block_on(lib_do_sync(&global_config, &imap_config, 10, false))
                        .map_err(|e| e.to_string());
                let _ = tx.send(BgResult::Fetch { result });
            });
        }

        Action::ServerSearch { query, targets } => {
            let imap_config = match app.imap_config.clone() {
                Some(c) => c,
                None => {
                    app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                    return Ok(());
                }
            };

            app.server_search_loading = true;
            app.server_search_status = Some("Searching...".to_string());
            app.bg_count += 1;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result =
                    rt.block_on(lib_do_multi_search(&imap_config, &query, &targets));
                let _ = tx.send(BgResult::ServerSearch {
                    result: result.map_err(|e| e.to_string()),
                });
            });
        }

        Action::SearchResultOpen
        | Action::SearchResultReply(_)
        | Action::SearchResultForward
        | Action::SearchResultArchive => {
            // Phase 5: actions on search results (handled below)
            handle_search_result_action(app, terminal, action, bg_tx)?;
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
                    app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
                    return Ok(());
                }
            };
            let global_config = app.global_config.clone();

            app.bg_count += 1;
            app.set_status_level("Syncing...".to_string(), StatusLevel::Progress);
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result = rt
                    .block_on(lib_do_sync(&global_config, &imap_config, 50, true))
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
                    let text = if msg.is_empty() { "Email archived".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Archive) {
                        app.invalidate_cache_idx(idx);
                    }
                }
                Err(e) => {
                    app.push_status(format!("Archive failed: {e}"), StatusLevel::Error);
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
                    let text = if msg.is_empty() { "Email deleted".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                }
                Err(e) => {
                    app.push_status(format!("Delete failed: {e}"), StatusLevel::Error);
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
                    let text = if msg.is_empty() { "Email sent".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status_level(format!("Send failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::SendApproved { result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Approved emails sent".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status_level(format!("Send-approved failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::Fetch { result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Fetch complete".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status_level(format!("Fetch failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::Sync { result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Sync complete".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    app.invalidate_all_caches();
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status_level(format!("Sync failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::ServerSearch { result } => {
            app.server_search_loading = false;
            match result {
                Ok(hits) => {
                    let count = hits.len();
                    app.server_search_results = hits
                        .into_iter()
                        .map(|hit| app::SearchResultEntry {
                            entry: hit.entry,
                            fetched: hit.fetched,
                            saved_path: None,
                            source_label: hit.source_label,
                            source_local_dir: hit.source_local_dir,
                            source_status: hit.source_status,
                        })
                        .collect();
                    app.server_search_index = 0;
                    app.server_search_scroll = 0;
                    app.server_search_headers_scroll = 0;
                    app.server_search_status = Some(format!(
                        "{} result{}",
                        count,
                        if count == 1 { "" } else { "s" }
                    ));
                    if count > 0 {
                        app.server_search_focus = app::SearchOverlayFocus::List;
                    }
                }
                Err(e) => {
                    app.server_search_status = Some(format!("Error: {}", e));
                }
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

fn handle_search_result_action(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: Action,
    bg_tx: &mpsc::Sender<BgResult>,
) -> Result<()> {
    match action {
        Action::SearchResultOpen => {
            if let Some(path) = ensure_search_result_saved(app) {
                suspend_terminal(terminal)?;
                let result = edit_file(&path);
                resume_terminal(terminal)?;
                match result {
                    Ok(()) => app.set_status("Returned from editor".to_string()),
                    Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
                }
            } else {
                app.set_status_level("Failed to save email locally".to_string(), StatusLevel::Error);
            }
        }

        Action::SearchResultReply(reply_all) => {
            if let Some(path) = ensure_search_result_saved(app) {
                let default_from = app
                    .smtp_config
                    .as_ref()
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
                    Err(e) => {
                        app.set_status_level(format!("Reply failed: {e}"), StatusLevel::Error)
                    }
                }
            } else {
                app.set_status_level("Failed to save email locally".to_string(), StatusLevel::Error);
            }
        }

        Action::SearchResultForward => {
            if let Some(path) = ensure_search_result_saved(app) {
                let default_from = app
                    .smtp_config
                    .as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_default();
                let drafts_dir = app.drafts_dir.clone();
                match create_forward_draft(&path, &default_from, drafts_dir.as_deref()) {
                    Ok(draft_path) => {
                        suspend_terminal(terminal)?;
                        let _ = edit_file(&draft_path);
                        resume_terminal(terminal)?;
                        app.set_status("Forward draft ready".to_string());
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                            app.invalidate_cache_idx(idx);
                        }
                    }
                    Err(e) => {
                        app.set_status_level(format!("Forward failed: {e}"), StatusLevel::Error)
                    }
                }
            } else {
                app.set_status_level("Failed to save email locally".to_string(), StatusLevel::Error);
            }
        }

        Action::SearchResultArchive => {
            if let Some(path) = ensure_search_result_saved(app) {
                let imap_config = match app.imap_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status_level(
                            "IMAP not configured".to_string(),
                            StatusLevel::Error,
                        );
                        return Ok(());
                    }
                };
                let archive_dir = match app.archive_dir.clone() {
                    Some(d) => d,
                    None => {
                        app.set_status_level(
                            "Archive dir not configured".to_string(),
                            StatusLevel::Error,
                        );
                        return Ok(());
                    }
                };
                let archive_server_name = app.archive_server_name.clone();

                // Remove from search results
                app.server_search_results.remove(app.server_search_index);
                if app.server_search_index >= app.server_search_results.len()
                    && !app.server_search_results.is_empty()
                {
                    app.server_search_index = app.server_search_results.len() - 1;
                }

                app.bg_count += 1;
                app.bg_mutations += 1;
                app.set_status_level("Archiving...".to_string(), StatusLevel::Progress);
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    let result = rt
                        .block_on(archive_email_locally(
                            &imap_config,
                            &archive_dir,
                            &path,
                            &archive_server_name,
                        ))
                        .map(|()| String::new())
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Archive { result });
                });
            } else {
                app.set_status_level("Failed to save email locally".to_string(), StatusLevel::Error);
            }
        }

        _ => {}
    }
    Ok(())
}

fn ensure_search_result_saved(app: &mut App) -> Option<PathBuf> {
    let idx = app.server_search_index;
    let result = app.server_search_results.get(idx)?;

    if let Some(ref path) = result.saved_path {
        return Some(path.clone());
    }

    // Use per-result source metadata instead of active mailbox
    let save_dir = result.source_local_dir.clone();
    let status = result.source_status.clone();

    let fetched_clone = result.fetched.clone();
    let message_id = result.fetched.message_id.clone();

    let mut known_ids = crate::parse::scan_existing_message_ids(&save_dir).unwrap_or_default();
    match crate::parse::save_fetched_emails_with_known_ids(
        &[fetched_clone],
        &save_dir,
        &status,
        &mut known_ids,
    ) {
        Ok((saved, _skipped)) => {
            if saved > 0 {
                if let Some(ref mid) = message_id {
                    if let Some(path) = find_file_by_message_id(&save_dir, mid) {
                        let result = app.server_search_results.get_mut(idx)?;
                        result.saved_path = Some(path.clone());
                        if let Some(cache_idx) = app.mailbox_index_for_dir(&save_dir) {
                            app.invalidate_cache_idx(cache_idx);
                        }
                        return Some(path);
                    }
                }
                if let Some(cache_idx) = app.mailbox_index_for_dir(&save_dir) {
                    app.invalidate_cache_idx(cache_idx);
                }
                None
            } else {
                // Already existed (skipped), find it
                if let Some(ref mid) = message_id {
                    if let Some(path) = find_file_by_message_id(&save_dir, mid) {
                        let result = app.server_search_results.get_mut(idx)?;
                        result.saved_path = Some(path.clone());
                        return Some(path);
                    }
                }
                None
            }
        }
        Err(_) => None,
    }
}

fn find_file_by_message_id(dir: &Path, message_id: &str) -> Option<PathBuf> {
    let walker = walkdir::WalkDir::new(dir).max_depth(1);
    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        if entry.path().extension().is_some_and(|ext| ext == "md") {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if content.contains(message_id) {
                    return Some(entry.path().to_path_buf());
                }
            }
        }
    }
    None
}

async fn lib_do_multi_search(
    imap_config: &ImapConfig,
    query: &str,
    targets: &[app::SearchTarget],
) -> anyhow::Result<Vec<app::SearchHit>> {
    use crate::imap_client::{open_imap_session, fetch_emails_on_session};

    let mut criteria = parse_search_query(query);
    // Clear in_mailbox -- it was already resolved to targets by the caller
    criteria.in_mailbox = None;

    let mut session = open_imap_session(imap_config).await?;
    let total_limit = 50usize;
    let per_mb = (total_limit / targets.len().max(1)).max(5);
    let mut total = 0usize;

    let mut hits: Vec<app::SearchHit> = Vec::new();

    for target in targets {
        if total >= total_limit {
            break;
        }
        let budget = per_mb.min(total_limit - total);
        match fetch_emails_on_session(
            &mut session,
            &criteria,
            &target.server_name,
            Some(budget),
        )
        .await
        {
            Ok(emails) => {
                total += emails.len();
                for fetched in emails {
                    let entry = fetched_to_email_entry(&fetched);
                    hits.push(app::SearchHit {
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

    // Sort by date descending
    hits.sort_by(|a, b| b.entry.date_sort.cmp(&a.entry.date_sort));

    Ok(hits)
}

fn fetched_to_email_entry(fetched: &FetchedEmail) -> app::EmailEntry {
    app::EmailEntry {
        path: PathBuf::new(),
        from: fetched.from.clone(),
        to: fetched.to.clone(),
        cc: fetched.cc.clone(),
        subject: fetched.subject.clone(),
        status: String::new(),
        date_display: fetched.date.chars().take(10).collect(),
        date_sort: fetched.date.clone(),
        body: fetched.body_text.clone(),
        has_attachments: fetched.has_attachments,
    }
}

fn watcher_loop(tx: mpsc::Sender<WatchEvent>, imap_config: ImapConfig) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => {
            let _ = tx.send(WatchEvent::Error("Failed to create async runtime".into()));
            return;
        }
    };
    loop {
        match rt.block_on(imap_watch(&imap_config, "INBOX", Some(300))) {
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

async fn lib_do_sync(
    global_config: &crate::config::GlobalConfig,
    imap_config: &ImapConfig,
    limit: usize,
    reconcile: bool,
) -> anyhow::Result<String> {
    let targets: Vec<SyncTarget> = all_configured_mailboxes(global_config)
        .iter()
        .map(|(role, mapping)| SyncTarget {
            role: role.clone(),
            server_name: mapping.server.clone(),
            local_dir: resolve_mailbox_local_path(global_config, mapping),
            status: mailbox_status(role).to_string(),
        })
        .collect();

    let result = sync_mailboxes(imap_config, &targets, limit, reconcile).await?;

    let mut msg = format!("Synced: {} new, {} existing", result.saved, result.skipped);
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
    Ok(msg)
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
