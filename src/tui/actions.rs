use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Result;
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::{Action, App, BgResult, MailboxKind, StatusLevel};
use super::helpers::{
    edit_file, ensure_search_result_saved, resolve_send_account, suspend_terminal, resume_terminal,
};
use super::ui;

use crate::config::resolve_sent_mailbox;
use crate::draft::{
    create_forward_draft, create_reply_draft, find_drafts, mark_as_approved, parse_email_draft,
    update_status_to_sent, validate_draft,
};
use crate::imap_client::{
    append_to_sent_folder, archive_email_locally, batch_archive_emails_locally,
    batch_delete_emails_locally, delete_email_locally,
};
use crate::send::send_email;
use crate::types::EmailStatus;

pub(super) fn handle_action(
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
                let (acct_idx, smtp_config, imap_config, account_config, signature, sent_dir) =
                    resolve_send_account(app, &path);
                let smtp_config = match smtp_config {
                    Some(c) => c,
                    None => {
                        app.set_status_level("SMTP not configured".to_string(), StatusLevel::Error);
                        return Ok(());
                    }
                };
                let email_settings = app.global_config.email.clone();

                app.bg_count += 1;
                app.set_status_level("Sending...".to_string(), StatusLevel::Progress);
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = (|| -> anyhow::Result<String> {
                        let draft = parse_email_draft(&path)?;
                        validate_draft(&draft)?;

                        let (send_result, raw_message, message_id) = rt.block_on(
                            send_email(&draft, &smtp_config, &email_settings, signature.as_deref()),
                        )?;

                        if send_result.all_succeeded() || send_result.any_succeeded() {
                            update_status_to_sent(
                                &draft,
                                sent_dir.as_deref(),
                                message_id.as_deref(),
                            )?;
                            if let Some(ref imap_cfg) = imap_config {
                                let sent_mailbox = resolve_sent_mailbox(&account_config);
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
                        account_index: acct_idx,
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
                let email_settings = app.global_config.email.clone();
                let account_config = app.account_config.clone();
                let imap_config = app.imap_config.clone();
                let signature = app.signature_content.clone();
                let sent_dir = app.sent_dir.clone();

                app.bg_count += 1;
                app.set_status_level("Sending approved...".to_string(), StatusLevel::Progress);
                let acct_idx = app.active_account;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
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
                                &email_settings,
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
                                                resolve_sent_mailbox(&account_config);
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
                        account_index: acct_idx,
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
                let now = chrono::Utc::now().to_rfc2822();
                let from = app.smtp_config.as_ref().map(|s| s.default_from.as_str()).unwrap_or("");
                let skeleton = format!("---\nto:\ncc:\nbcc:\nsubject:\nstatus: draft\nfrom: {from}\ndate: {now}\nreply_to:\nattachments: []\n---\n\n");
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
                let acct_idx = app.active_account;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt.block_on(archive_email_locally(
                        &imap_config,
                        &archive_dir,
                        &path,
                        &archive_server_name,
                    ))
                    .map(|()| String::new())
                    .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Archive { account_index: acct_idx, result });
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
                let acct_idx = app.active_account;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt.block_on(delete_email_locally(&imap_config, &path))
                        .map(|()| String::new())
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Delete { account_index: acct_idx, result });
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

            let acct_idx = app.active_account;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                let results = rt.block_on(batch_archive_emails_locally(
                    &imap_config,
                    &archive_dir,
                    &paths,
                    &archive_server_name,
                ));
                for (_path, result) in results {
                    let _ = tx.send(BgResult::Archive {
                        account_index: acct_idx,
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

            let acct_idx = app.active_account;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                let results = rt.block_on(batch_delete_emails_locally(&imap_config, &paths));
                for (_path, result) in results {
                    let _ = tx.send(BgResult::Delete {
                        account_index: acct_idx,
                        result: result.map(|()| String::new()).map_err(|e| e.to_string()),
                    });
                }
            });
        }

        Action::CopyPath => {
            if let Some(path) = app.selected_email_path() {
                match super::helpers::copy_to_clipboard(&path.display().to_string()) {
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
            if app.bg_count > 0 {
                app.queued_action = Some(Action::Fetch);
                app.set_status(format!(
                    "Fetch queued ({} ops pending...)",
                    app.bg_count
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
            let account_config = app.account_config.clone();

            app.bg_count += 1;
            app.set_status_level("Fetching...".to_string(), StatusLevel::Progress);
            let acct_idx = app.active_account;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                let result =
                    rt.block_on(super::helpers::lib_do_sync(&account_config, &imap_config, 10, false))
                        .map_err(|e| e.to_string());
                let _ = tx.send(BgResult::Fetch { account_index: acct_idx, result });
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
                let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                let result =
                    rt.block_on(super::helpers::lib_do_multi_search(&imap_config, &query, &targets));
                let _ = tx.send(BgResult::ServerSearch {
                    result: result.map_err(|e| e.to_string()),
                });
            });
        }

        Action::SearchResultOpen
        | Action::SearchResultReply(_)
        | Action::SearchResultForward
        | Action::SearchResultArchive => {
            handle_search_result_action(app, terminal, action, bg_tx)?;
        }

        Action::Sync => {
            if app.bg_count > 0 {
                app.queued_action = Some(Action::Sync);
                app.set_status(format!(
                    "Sync queued ({} ops pending...)",
                    app.bg_count
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
            let account_config = app.account_config.clone();

            app.bg_count += 1;
            app.set_status_level("Syncing...".to_string(), StatusLevel::Progress);
            let acct_idx = app.active_account;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                let result = rt
                    .block_on(super::helpers::lib_do_sync(&account_config, &imap_config, 50, true))
                    .map_err(|e| e.to_string());
                let _ = tx.send(BgResult::Sync { account_index: acct_idx, result });
            });
        }
    }

    Ok(())
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

                app.server_search_results.remove(app.server_search_index);
                if app.server_search_index >= app.server_search_results.len()
                    && !app.server_search_results.is_empty()
                {
                    app.server_search_index = app.server_search_results.len() - 1;
                }

                app.bg_count += 1;
                app.bg_mutations += 1;
                app.set_status_level("Archiving...".to_string(), StatusLevel::Progress);
                let acct_idx = app.active_account;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt
                        .block_on(archive_email_locally(
                            &imap_config,
                            &archive_dir,
                            &path,
                            &archive_server_name,
                        ))
                        .map(|()| String::new())
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Archive { account_index: acct_idx, result });
                });
            } else {
                app.set_status_level("Failed to save email locally".to_string(), StatusLevel::Error);
            }
        }

        _ => {}
    }
    Ok(())
}
