use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Result;
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::{
    Action, App, BgResult, ComposeField, ComposeMode, ComposeWizard, Focus, MailboxKind,
    StatusLevel,
};
use super::helpers::{
    edit_file, ensure_search_result_saved, lib_do_multi_search_graph, lib_do_sync_graph,
    resolve_send_account, resume_terminal, suspend_terminal,
};
use super::ui;

use crate::config::resolve_sent_mailbox;
use crate::draft::{
    create_forward_draft, create_reply_draft, find_drafts, mark_as_approved, parse_email_draft,
    update_status_to_sent, validate_draft,
};
use crate::imap_client::{
    append_to_sent_folder, archive_email_locally, batch_archive_emails_locally,
    batch_delete_emails_locally, delete_email_locally, get_message_id_from_file,
    mark_read_on_server, mark_unread_on_server, update_read_status_locally,
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
            if let Some(email) = app.selected_email() {
                let path = email.path.clone();
                let was_unread = !email.read;
                suspend_terminal(terminal)?;
                let result = edit_file(&path);
                resume_terminal(terminal)?;
                match result {
                    Ok(()) => app.set_status("Returned from editor".to_string()),
                    Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
                }
                app.reload_current_mailbox();
                // Auto-mark as read after opening in editor
                if was_unread {
                    app.push_action(Action::MarkAsRead);
                }
            }
        }

        Action::Reply(reply_all) => {
            if let Some(path) = app.selected_email_path() {
                let default_from = app
                    .smtp_config
                    .as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_else(|| app.account_config.default_from.clone());
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
                app.reload_current_mailbox();
            }
        }

        Action::Forward => {
            if let Some(path) = app.selected_email_path() {
                let default_from = app
                    .smtp_config
                    .as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_else(|| app.account_config.default_from.clone());
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
                app.reload_current_mailbox();
            }
        }

        Action::Send => {
            if let Some(path) = app.selected_email_path() {
                let (acct_idx, smtp_config, imap_config, graph_config, account_config, signature, sent_dir) =
                    resolve_send_account(app, &path);

                if graph_config.is_some()
                    && account_config.auth_method == crate::config::AuthMethod::Graph
                {
                    let graph_config = graph_config.unwrap();
                    let email_settings = app.global_config.email.clone();

                    app.bg_count += 1;
                    app.set_status_level(
                        "Sending via Graph...".to_string(),
                        StatusLevel::Progress,
                    );
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = (|| -> anyhow::Result<String> {
                            let draft = parse_email_draft(&path)?;
                            validate_draft(&draft)?;

                            let to = parse_graph_recipients(draft.frontmatter.to.as_deref());
                            let cc = parse_graph_recipients(draft.frontmatter.cc.as_deref());
                            let bcc = parse_graph_recipients(draft.frontmatter.bcc.as_deref());
                            let to_refs: Vec<(&str, &str)> =
                                to.iter().map(|(n, a)| (n.as_str(), a.as_str())).collect();
                            let cc_refs: Vec<(&str, &str)> =
                                cc.iter().map(|(n, a)| (n.as_str(), a.as_str())).collect();
                            let bcc_refs: Vec<(&str, &str)> =
                                bcc.iter().map(|(n, a)| (n.as_str(), a.as_str())).collect();

                            let quoted_html = draft.path.with_extension("html");
                            let quoted = if quoted_html.exists() {
                                std::fs::read_to_string(&quoted_html).ok()
                            } else {
                                None
                            };
                            let html_body = crate::send::markdown_to_html(
                                &draft.body_markdown,
                                &email_settings,
                                signature.as_deref(),
                                quoted.as_deref(),
                            );

                            let mut att_data: Vec<(String, Vec<u8>, String)> = Vec::new();
                            if let Some(ref attachments) = draft.frontmatter.attachments {
                                for att_path in attachments {
                                    let expanded = shellexpand::tilde(att_path);
                                    let p = std::path::Path::new(expanded.as_ref());
                                    let content = std::fs::read(p)?;
                                    let filename = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| "attachment".to_string());
                                    let content_type =
                                        mime_guess::from_path(p).first_or_octet_stream().to_string();
                                    att_data.push((filename, content, content_type));
                                }
                            }

                            let client = rt
                                .block_on(crate::graph::GraphClient::new_async(&graph_config))?;
                            rt.block_on(client.send_mail(
                                &to_refs,
                                &cc_refs,
                                &bcc_refs,
                                &draft.frontmatter.subject,
                                &html_body,
                                &att_data,
                            ))?;

                            update_status_to_sent(&draft, sent_dir.as_deref(), None)?;
                            crate::contacts::hooks::bump_after_send(&account_config, &draft);

                            Ok(format!(
                                "Sent via Graph to {} recipient(s)",
                                to.len() + cc.len() + bcc.len()
                            ))
                        })();
                        let _ = tx.send(BgResult::Send {
                            account_index: acct_idx,
                            result: result.map_err(|e| e.to_string()),
                        });
                    });
                } else {
                    let smtp_config = match smtp_config {
                        Some(c) => c,
                        None => {
                            app.set_status_level(
                                "SMTP not configured".to_string(),
                                StatusLevel::Error,
                            );
                            return Ok(());
                        }
                    };
                    let email_settings = app.global_config.email.clone();

                    app.bg_count += 1;
                    app.set_status_level("Sending...".to_string(), StatusLevel::Progress);
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = (|| -> anyhow::Result<String> {
                            let draft = parse_email_draft(&path)?;
                            validate_draft(&draft)?;

                            let (send_result, raw_message, message_id) = rt.block_on(send_email(
                                &draft,
                                &smtp_config,
                                &email_settings,
                                signature.as_deref(),
                            ))?;

                            if send_result.all_succeeded() || send_result.any_succeeded() {
                                update_status_to_sent(
                                    &draft,
                                    sent_dir.as_deref(),
                                    message_id.as_deref(),
                                )?;
                                if let Some(ref imap_cfg) = imap_config {
                                    let sent_mailbox = resolve_sent_mailbox(&account_config);
                                    let _ = rt.block_on(append_to_sent_folder(
                                        imap_cfg,
                                        &raw_message,
                                        &sent_mailbox,
                                    ));
                                }
                                crate::contacts::hooks::bump_after_send(&account_config, &draft);
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
        }

        Action::SendApproved => {
            if let Some(dir) = app.active_dir().cloned() {
                if app.is_graph() {
                    let graph_config = app.graph_config.clone().unwrap();
                    let email_settings = app.global_config.email.clone();
                    let account_config = app.account_config.clone();
                    let signature = app.signature_content.clone();
                    let sent_dir = app.sent_dir.clone();

                    app.bg_count += 1;
                    app.set_status_level(
                        "Sending approved via Graph...".to_string(),
                        StatusLevel::Progress,
                    );
                    let acct_idx = app.active_account;
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = (|| -> anyhow::Result<String> {
                            let drafts = find_drafts(&dir, Some(EmailStatus::Approved))?;
                            if drafts.is_empty() {
                                return Ok("No approved emails found".to_string());
                            }

                            let mut sent = 0usize;
                            let mut failed = 0usize;

                            for draft in &drafts {
                                let send_result = (|| -> anyhow::Result<()> {
                                    let to = parse_graph_recipients(
                                        draft.frontmatter.to.as_deref(),
                                    );
                                    let cc = parse_graph_recipients(
                                        draft.frontmatter.cc.as_deref(),
                                    );
                                    let bcc = parse_graph_recipients(
                                        draft.frontmatter.bcc.as_deref(),
                                    );
                                    let to_refs: Vec<(&str, &str)> = to
                                        .iter()
                                        .map(|(n, a)| (n.as_str(), a.as_str()))
                                        .collect();
                                    let cc_refs: Vec<(&str, &str)> = cc
                                        .iter()
                                        .map(|(n, a)| (n.as_str(), a.as_str()))
                                        .collect();
                                    let bcc_refs: Vec<(&str, &str)> = bcc
                                        .iter()
                                        .map(|(n, a)| (n.as_str(), a.as_str()))
                                        .collect();

                                    let quoted_html = draft.path.with_extension("html");
                                    let quoted = if quoted_html.exists() {
                                        std::fs::read_to_string(&quoted_html).ok()
                                    } else {
                                        None
                                    };
                                    let html_body = crate::send::markdown_to_html(
                                        &draft.body_markdown,
                                        &email_settings,
                                        signature.as_deref(),
                                        quoted.as_deref(),
                                    );

                                    let mut att_data: Vec<(String, Vec<u8>, String)> = Vec::new();
                                    if let Some(ref attachments) = draft.frontmatter.attachments {
                                        for att_path in attachments {
                                            let expanded = shellexpand::tilde(att_path);
                                            let p = std::path::Path::new(expanded.as_ref());
                                            let content = std::fs::read(p)?;
                                            let filename = p
                                                .file_name()
                                                .map(|n| n.to_string_lossy().to_string())
                                                .unwrap_or_else(|| "attachment".to_string());
                                            let content_type = mime_guess::from_path(p)
                                                .first_or_octet_stream()
                                                .to_string();
                                            att_data.push((filename, content, content_type));
                                        }
                                    }

                                    let client = rt.block_on(
                                        crate::graph::GraphClient::new_async(&graph_config),
                                    )?;
                                    rt.block_on(client.send_mail(
                                        &to_refs,
                                        &cc_refs,
                                        &bcc_refs,
                                        &draft.frontmatter.subject,
                                        &html_body,
                                        &att_data,
                                    ))?;
                                    Ok(())
                                })();

                                match send_result {
                                    Ok(()) => {
                                        let _ = update_status_to_sent(
                                            draft,
                                            sent_dir.as_deref(),
                                            None,
                                        );
                                        crate::contacts::hooks::bump_after_send(
                                            &account_config,
                                            draft,
                                        );
                                        sent += 1;
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
                } else {
                    let smtp_config = match app.smtp_config.clone() {
                        Some(c) => c,
                        None => {
                            app.set_status_level(
                                "SMTP not configured".to_string(),
                                StatusLevel::Error,
                            );
                            return Ok(());
                        }
                    };
                    let email_settings = app.global_config.email.clone();
                    let account_config = app.account_config.clone();
                    let imap_config = app.imap_config.clone();
                    let signature = app.signature_content.clone();
                    let sent_dir = app.sent_dir.clone();

                    app.bg_count += 1;
                    app.set_status_level(
                        "Sending approved...".to_string(),
                        StatusLevel::Progress,
                    );
                    let acct_idx = app.active_account;
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
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
                                        if send_result.all_succeeded()
                                            || send_result.any_succeeded()
                                        {
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
                                            crate::contacts::hooks::bump_after_send(
                                                &account_config,
                                                draft,
                                            );
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
        }

        Action::NewDraft => {
            let name = chrono::Local::now()
                .format("draft-%Y%m%d-%H%M%S")
                .to_string();
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
                let default_from = app
                    .smtp_config
                    .as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_else(|| app.account_config.default_from.clone());
                let from = default_from.as_str();
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
                    Err(e) => {
                        app.set_status_level(format!("New draft failed: {e}"), StatusLevel::Error)
                    }
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
                    Err(e) => {
                        app.set_status_level(format!("Approve failed: {e}"), StatusLevel::Error)
                    }
                }
            }
        }

        Action::BatchApprove(paths) => {
            let total = paths.len();
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            for path in &paths {
                match mark_as_approved(path) {
                    Ok(_) => succeeded += 1,
                    Err(e) => {
                        log::warn!("Approve failed for {}: {e}", path.display());
                        failed += 1;
                    }
                }
            }
            if failed == 0 {
                app.set_status(format!("Approved {} drafts", succeeded));
            } else {
                app.set_status_level(
                    format!("Approved {}/{} drafts ({} failed)", succeeded, total, failed),
                    StatusLevel::Warning,
                );
            }
            app.selection.clear();
            app.reload_current_mailbox();
        }

        Action::Archive => {
            if let Some(path) = app.selected_email_path() {
                let archive_dir = match app.archive_dir.clone() {
                    Some(d) => d,
                    None => {
                        app.set_status_level(
                            "Archive directory not configured".to_string(),
                            StatusLevel::Error,
                        );
                        return Ok(());
                    }
                };
                let archive_server_name = app.archive_server_name.clone();

                if app.is_graph() {
                    let graph_config = app.graph_config.clone().unwrap();

                    app.remove_selected_from_list();
                    app.bg_count += 1;
                    app.bg_mutations += 1;
                    app.set_status_level("Archiving...".to_string(), StatusLevel::Progress);
                    terminal.draw(|frame| ui::view(app, frame))?;
                    let acct_idx = app.active_account;
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = rt
                            .block_on(crate::graph::archive_email_graph(
                                &graph_config,
                                &archive_dir,
                                &path,
                                &archive_server_name,
                            ))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Archive {
                            account_index: acct_idx,
                            result,
                        });
                    });
                } else {
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

                    // Optimistically update in-memory index
                    if let Some(mid) = get_message_id_from_file(&path) {
                        app.remove_from_message_index(&path, &mid);
                        // The file will be moved to archive_dir by the background task.
                        // We don't know the exact filename yet, so the archive entry
                        // will be updated on the next sync or bg_result.
                    }

                    app.remove_selected_from_list();
                    app.bg_count += 1;
                    app.bg_mutations += 1;
                    app.set_status_level("Archiving...".to_string(), StatusLevel::Progress);
                    terminal.draw(|frame| ui::view(app, frame))?;
                    let acct_idx = app.active_account;
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = rt
                            .block_on(archive_email_locally(
                                &imap_config,
                                &archive_dir,
                                &path,
                                &archive_server_name,
                            ))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Archive {
                            account_index: acct_idx,
                            result,
                        });
                    });
                }
            }
        }

        Action::Delete => {
            if let Some(path) = app.selected_email_path() {
                if app.is_graph() {
                    let graph_config = app.graph_config.clone().unwrap();

                    app.remove_selected_from_list();
                    app.bg_count += 1;
                    app.bg_mutations += 1;
                    app.set_status_level("Deleting...".to_string(), StatusLevel::Progress);
                    terminal.draw(|frame| ui::view(app, frame))?;
                    let acct_idx = app.active_account;
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = rt
                            .block_on(crate::graph::delete_email_graph(&graph_config, &path))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Delete {
                            account_index: acct_idx,
                            result,
                        });
                    });
                } else {
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

                    // Optimistically update in-memory index
                    if let Some(mid) = get_message_id_from_file(&path) {
                        app.remove_from_message_index(&path, &mid);
                    }

                    app.remove_selected_from_list();
                    app.bg_count += 1;
                    app.bg_mutations += 1;
                    app.set_status_level("Deleting...".to_string(), StatusLevel::Progress);
                    terminal.draw(|frame| ui::view(app, frame))?;
                    let acct_idx = app.active_account;
                    let tx = bg_tx.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = rt
                            .block_on(delete_email_locally(&imap_config, &path))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Delete {
                            account_index: acct_idx,
                            result,
                        });
                    });
                }
            }
        }

        Action::BatchArchive(paths) => {
            let archive_dir = match app.archive_dir.clone() {
                Some(d) => d,
                None => {
                    app.set_status_level(
                        "Archive directory not configured".to_string(),
                        StatusLevel::Error,
                    );
                    return Ok(());
                }
            };
            let archive_server_name = app.archive_server_name.clone();

            let path_set: HashSet<PathBuf> = paths.iter().cloned().collect();
            app.remove_selected_from_list_batch(&path_set);

            let count = paths.len();
            app.bg_count += count;
            app.bg_mutations += count;
            app.set_status_level(
                format!("Archiving {} emails...", count),
                StatusLevel::Progress,
            );
            terminal.draw(|frame| ui::view(app, frame))?;

            let acct_idx = app.active_account;
            let tx = bg_tx.clone();

            if app.is_graph() {
                let graph_config = app.graph_config.clone().unwrap();
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    for path in &paths {
                        let result = rt
                            .block_on(crate::graph::archive_email_graph(
                                &graph_config,
                                &archive_dir,
                                path,
                                &archive_server_name,
                            ))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Archive {
                            account_index: acct_idx,
                            result,
                        });
                    }
                });
            } else {
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
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
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
        }

        Action::BatchDelete(paths) => {
            let path_set: HashSet<PathBuf> = paths.iter().cloned().collect();
            app.remove_selected_from_list_batch(&path_set);

            let count = paths.len();
            app.bg_count += count;
            app.bg_mutations += count;
            app.set_status_level(
                format!("Deleting {} emails...", count),
                StatusLevel::Progress,
            );
            terminal.draw(|frame| ui::view(app, frame))?;

            let acct_idx = app.active_account;
            let tx = bg_tx.clone();

            if app.is_graph() {
                let graph_config = app.graph_config.clone().unwrap();
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    for path in &paths {
                        let result = rt
                            .block_on(crate::graph::delete_email_graph(&graph_config, path))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Delete {
                            account_index: acct_idx,
                            result,
                        });
                    }
                });
            } else {
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
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let results =
                        rt.block_on(batch_delete_emails_locally(&imap_config, &paths));
                    for (_path, result) in results {
                        let _ = tx.send(BgResult::Delete {
                            account_index: acct_idx,
                            result: result.map(|()| String::new()).map_err(|e| e.to_string()),
                        });
                    }
                });
            }
        }

        Action::ToggleRead => {
            if let Some(email) = app.selected_email() {
                let new_read = !email.read;
                let path = email.path.clone();
                let message_id = get_message_id_from_file(&path);

                // Optimistic local update
                update_read_status_locally(&path, new_read).ok();
                if let Some(entry) = app.emails.get_mut(app.list_index) {
                    entry.read = new_read;
                }
                if let Some(Some(cached)) = app.email_cache.get_mut(app.active_mailbox) {
                    if let Some(ce) = cached.iter_mut().find(|e| e.path == path) {
                        ce.read = new_read;
                    }
                }

                let label = if new_read {
                    "Marked as read"
                } else {
                    "Marked as unread"
                };
                app.set_status(label.to_string());

                // Async server update
                if let Some(mid) = message_id {
                    if app.is_graph() {
                        let graph_cfg = app.graph_config.clone().unwrap();
                        let acct_idx = app.active_account;
                        let path_clone = path.clone();
                        app.bg_count += 1;
                        let tx = bg_tx.clone();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new()
                                .expect("failed to create tokio runtime");
                            let result =
                                rt.block_on(crate::graph::mark_read_graph(&graph_cfg, &mid, new_read));
                            let _ = tx.send(BgResult::ToggleRead {
                                account_index: acct_idx,
                                path: path_clone,
                                new_read_state: new_read,
                                result: result
                                    .map(|()| String::new())
                                    .map_err(|e| e.to_string()),
                            });
                        });
                    } else if let Some(imap_config) = app.imap_config.clone() {
                        let mailbox = app.active_server_mailbox();
                        let acct_idx = app.active_account;
                        let path_clone = path.clone();
                        app.bg_count += 1;
                        let tx = bg_tx.clone();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new()
                                .expect("failed to create tokio runtime");
                            let result = if new_read {
                                rt.block_on(mark_read_on_server(&imap_config, &mid, &mailbox))
                            } else {
                                rt.block_on(mark_unread_on_server(&imap_config, &mid, &mailbox))
                            };
                            let _ = tx.send(BgResult::ToggleRead {
                                account_index: acct_idx,
                                path: path_clone,
                                new_read_state: new_read,
                                result: result
                                    .map(|()| String::new())
                                    .map_err(|e| e.to_string()),
                            });
                        });
                    }
                }
            }
        }

        Action::MarkAsRead => {
            if let Some(email) = app.selected_email() {
                if email.read {
                    return Ok(());
                }
                let path = email.path.clone();
                let message_id = get_message_id_from_file(&path);

                // Optimistic local update (silent)
                update_read_status_locally(&path, true).ok();
                if let Some(entry) = app.emails.get_mut(app.list_index) {
                    entry.read = true;
                }
                if let Some(Some(cached)) = app.email_cache.get_mut(app.active_mailbox) {
                    if let Some(ce) = cached.iter_mut().find(|e| e.path == path) {
                        ce.read = true;
                    }
                }

                // Async server update (no status message for auto-mark)
                if let Some(mid) = message_id {
                    if app.is_graph() {
                        let graph_cfg = app.graph_config.clone().unwrap();
                        let acct_idx = app.active_account;
                        let path_clone = path.clone();
                        app.bg_count += 1;
                        let tx = bg_tx.clone();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new()
                                .expect("failed to create tokio runtime");
                            let result = rt.block_on(crate::graph::mark_read_graph(
                                &graph_cfg, &mid, true,
                            ));
                            let _ = tx.send(BgResult::ToggleRead {
                                account_index: acct_idx,
                                path: path_clone,
                                new_read_state: true,
                                result: result
                                    .map(|()| String::new())
                                    .map_err(|e| e.to_string()),
                            });
                        });
                    } else if let Some(imap_config) = app.imap_config.clone() {
                        let mailbox = app.active_server_mailbox();
                        let acct_idx = app.active_account;
                        let path_clone = path.clone();
                        app.bg_count += 1;
                        let tx = bg_tx.clone();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new()
                                .expect("failed to create tokio runtime");
                            let result =
                                rt.block_on(mark_read_on_server(&imap_config, &mid, &mailbox));
                            let _ = tx.send(BgResult::ToggleRead {
                                account_index: acct_idx,
                                path: path_clone,
                                new_read_state: true,
                                result: result
                                    .map(|()| String::new())
                                    .map_err(|e| e.to_string()),
                            });
                        });
                    }
                }
            }
        }

        Action::BatchToggleRead(paths) => {
            let any_unread = paths
                .iter()
                .any(|p| app.emails.iter().any(|e| e.path == *p && !e.read));
            let new_read = any_unread;

            // Optimistic local update
            for path in &paths {
                update_read_status_locally(path, new_read).ok();
                if let Some(entry) = app.emails.iter_mut().find(|e| &e.path == path) {
                    entry.read = new_read;
                }
                if let Some(Some(cached)) = app.email_cache.get_mut(app.active_mailbox) {
                    if let Some(ce) = cached.iter_mut().find(|e| &e.path == path) {
                        ce.read = new_read;
                    }
                }
            }
            app.selection.clear();

            let label = if new_read {
                format!("Marked {} as read", paths.len())
            } else {
                format!("Marked {} as unread", paths.len())
            };
            app.set_status(label);

            // Async server update
            if app.is_graph() {
                let graph_cfg = app.graph_config.clone().unwrap();
                let acct_idx = app.active_account;
                app.bg_count += 1;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    for path in &paths {
                        if let Some(mid) = get_message_id_from_file(path) {
                            let result = rt.block_on(crate::graph::mark_read_graph(
                                &graph_cfg, &mid, new_read,
                            ));
                            if let Err(e) = result {
                                log::warn!("Failed to toggle read for {}: {}", mid, e);
                            }
                        }
                    }
                    let _ = tx.send(BgResult::ToggleRead {
                        account_index: acct_idx,
                        path: paths.first().cloned().unwrap_or_default(),
                        new_read_state: new_read,
                        result: Ok(String::new()),
                    });
                });
            } else if let Some(imap_config) = app.imap_config.clone() {
                let mailbox = app.active_server_mailbox();
                let acct_idx = app.active_account;
                app.bg_count += 1;
                let tx = bg_tx.clone();
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    for path in &paths {
                        if let Some(mid) = get_message_id_from_file(path) {
                            let result = if new_read {
                                rt.block_on(mark_read_on_server(&imap_config, &mid, &mailbox))
                            } else {
                                rt.block_on(mark_unread_on_server(&imap_config, &mid, &mailbox))
                            };
                            if let Err(e) = result {
                                log::warn!("Failed to toggle read for {}: {}", mid, e);
                            }
                        }
                    }
                    let _ = tx.send(BgResult::ToggleRead {
                        account_index: acct_idx,
                        path: paths.first().cloned().unwrap_or_default(),
                        new_read_state: new_read,
                        result: Ok(String::new()),
                    });
                });
            }
        }

        Action::CopyPath => {
            if let Some(path) = app.selected_email_path() {
                match super::helpers::copy_to_clipboard(&path.display().to_string()) {
                    Ok(()) => app.set_status("Path copied to clipboard".to_string()),
                    Err(e) => app.set_status_level(format!("Copy failed: {e}"), StatusLevel::Error),
                }
            }
        }

        Action::OpenAttachment(path) => match crate::parse::open_file_with_system(&path) {
            Ok(()) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                app.set_status(format!("Opened: {name}"));
            }
            Err(e) => {
                app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
            }
        },

        Action::SaveAttachments { sources, dest_dir } => {
            let mut saved = 0usize;
            let mut failed = 0usize;
            for source in &sources {
                match crate::parse::save_attachment(source, &dest_dir) {
                    Ok(_) => saved += 1,
                    Err(e) => {
                        log::warn!("Save attachment failed for {}: {e}", source.display());
                        failed += 1;
                    }
                }
            }
            app.last_save_dir = Some(dest_dir.clone());
            if failed == 0 {
                let dir_display = dest_dir.display();
                app.set_status_level(
                    format!("Saved {} file(s) to {dir_display}", saved),
                    StatusLevel::Success,
                );
            } else {
                app.set_status_level(
                    format!("Saved {}/{} file(s) ({} failed)", saved, saved + failed, failed),
                    StatusLevel::Warning,
                );
            }
        }

        Action::OpenHtmlInBrowser(path) => match crate::parse::open_file_with_system(&path) {
            Ok(()) => {
                app.set_status("Opened in browser".to_string());
            }
            Err(e) => {
                app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
            }
        },

        Action::Fetch => {
            if app.bg_count > 0 {
                app.queued_action = Some(Action::Fetch);
                app.set_status(format!(
                    "Quick sync queued ({} ops pending...)",
                    app.bg_count
                ));
                return Ok(());
            }
            let account_config = app.account_config.clone();
            let acct_idx = app.active_account;
            let tx = bg_tx.clone();

            if app.is_graph() {
                let graph_config = app.graph_config.clone().unwrap();
                app.bg_count += 1;
                app.set_status_level("Quick sync (Graph)...".to_string(), StatusLevel::Progress);
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt
                        .block_on(lib_do_sync_graph(&account_config, &graph_config, 100, true))
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Fetch {
                        account_index: acct_idx,
                        result,
                        new_index: None,
                        new_mailbox_states: None,
                    });
                });
            } else {
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
                // Clone index + prev states for the background thread
                let mut index_clone = app.accounts[acct_idx].message_id_index.clone();
                let prev_states = app.accounts[acct_idx].mailbox_states.clone();

                app.bg_count += 1;
                app.set_status_level("Quick sync...".to_string(), StatusLevel::Progress);
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let sync_result = rt
                        .block_on(super::helpers::lib_do_sync(
                            &account_config,
                            &imap_config,
                            100,
                            true,
                            Some(&mut index_clone),
                            Some(&prev_states),
                        ));
                    let (result, new_index, new_states) = match sync_result {
                        Ok((msg, meta)) => (
                            Ok(msg),
                            Some(index_clone),
                            Some(meta.mailbox_states),
                        ),
                        Err(e) => (Err(e.to_string()), None, None),
                    };
                    let _ = tx.send(BgResult::Fetch {
                        account_index: acct_idx,
                        result,
                        new_index,
                        new_mailbox_states: new_states,
                    });
                });
            }
        }

        Action::FetchAccount(acct_idx) => {
            // Per-account quick sync used by the startup auto-fetch path.
            // Triggered from `BgResult::IndexReady` once that account's
            // `message_id_index` has been populated. Unlike `Action::Fetch`,
            // does *not* gate on `bg_count > 0` -- multiple accounts'
            // fetches must be allowed to run concurrently.
            let acct = match app.accounts.get(acct_idx) {
                Some(a) => a,
                None => return Ok(()),
            };
            let account_config = acct.account_config.clone();
            let account_name = account_config.name.clone();
            let tx = bg_tx.clone();

            if acct.is_graph() {
                let graph_config = match acct.graph_config.clone() {
                    Some(c) => c,
                    None => return Ok(()),
                };
                app.bg_count += 1;
                app.set_status_level(
                    format!("Quick sync ({account_name}, Graph)..."),
                    StatusLevel::Progress,
                );
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new()
                        .expect("failed to create tokio runtime");
                    let result = rt
                        .block_on(lib_do_sync_graph(&account_config, &graph_config, 100, true))
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Fetch {
                        account_index: acct_idx,
                        result,
                        new_index: None,
                        new_mailbox_states: None,
                    });
                });
            } else {
                let imap_config = match acct.imap_config.clone() {
                    Some(c) => c,
                    None => return Ok(()), // local-only / no IMAP -- no-op
                };
                let mut index_clone = acct.message_id_index.clone();
                let prev_states = acct.mailbox_states.clone();

                app.bg_count += 1;
                app.set_status_level(
                    format!("Quick sync ({account_name})..."),
                    StatusLevel::Progress,
                );
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new()
                        .expect("failed to create tokio runtime");
                    let sync_result = rt.block_on(super::helpers::lib_do_sync(
                        &account_config,
                        &imap_config,
                        100,
                        true,
                        Some(&mut index_clone),
                        Some(&prev_states),
                    ));
                    let (result, new_index, new_states) = match sync_result {
                        Ok((msg, meta)) => (
                            Ok(msg),
                            Some(index_clone),
                            Some(meta.mailbox_states),
                        ),
                        Err(e) => (Err(e.to_string()), None, None),
                    };
                    let _ = tx.send(BgResult::Fetch {
                        account_index: acct_idx,
                        result,
                        new_index,
                        new_mailbox_states: new_states,
                    });
                });
            }
        }

        Action::ServerSearch { query, targets } => {
            app.server_search_loading = true;
            app.server_search_status = Some("Searching...".to_string());
            app.bg_count += 1;
            let tx = bg_tx.clone();

            if app.is_graph() {
                let graph_config = app.graph_config.clone().unwrap();
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt.block_on(lib_do_multi_search_graph(
                        &graph_config,
                        &query,
                        &targets,
                    ));
                    let _ = tx.send(BgResult::ServerSearch {
                        result: result.map_err(|e| e.to_string()),
                    });
                });
            } else {
                let imap_config = match app.imap_config.clone() {
                    Some(c) => c,
                    None => {
                        app.set_status_level(
                            "IMAP not configured".to_string(),
                            StatusLevel::Error,
                        );
                        app.server_search_loading = false;
                        app.server_search_status = None;
                        app.bg_count -= 1;
                        return Ok(());
                    }
                };
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt.block_on(super::helpers::lib_do_multi_search(
                        &imap_config,
                        &query,
                        &targets,
                    ));
                    let _ = tx.send(BgResult::ServerSearch {
                        result: result.map_err(|e| e.to_string()),
                    });
                });
            }
        }

        Action::SearchResultOpen
        | Action::SearchResultReply(_)
        | Action::SearchResultForward
        | Action::SearchResultArchive
        | Action::SearchResultOpenInBrowser => {
            handle_search_result_action(app, terminal, action, bg_tx)?;
        }

        Action::Sync => {
            if app.bg_count > 0 {
                app.queued_action = Some(Action::Sync);
                app.set_status(format!(
                    "Full sync queued ({} ops pending...)",
                    app.bg_count
                ));
                return Ok(());
            }
            let account_config = app.account_config.clone();
            let acct_idx = app.active_account;
            let tx = bg_tx.clone();

            if app.is_graph() {
                let graph_config = app.graph_config.clone().unwrap();
                app.bg_count += 1;
                app.set_status_level(
                    "Full sync (Graph)...".to_string(),
                    StatusLevel::Progress,
                );
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    let result = rt
                        .block_on(lib_do_sync_graph(
                            &account_config,
                            &graph_config,
                            usize::MAX,
                            true,
                        ))
                        .map_err(|e| e.to_string());
                    let _ = tx.send(BgResult::Sync {
                        account_index: acct_idx,
                        result,
                        new_index: None,
                        new_mailbox_states: None,
                    });
                });
            } else {
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
                app.bg_count += 1;
                app.set_status_level("Full sync...".to_string(), StatusLevel::Progress);
                std::thread::spawn(move || {
                    let rt =
                        tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
                    // Full sync: pass None for index (rebuild from disk) and None for prev_states
                    // (always reconcile).
                    let sync_result = rt
                        .block_on(super::helpers::lib_do_sync(
                            &account_config,
                            &imap_config,
                            usize::MAX,
                            true,
                            None,
                            None,
                        ));
                    let (result, new_index, new_states) = match sync_result {
                        Ok((msg, meta)) => (
                            Ok(msg),
                            None, // full sync doesn't carry index (will rebuild from cache invalidation)
                            Some(meta.mailbox_states),
                        ),
                        Err(e) => (Err(e.to_string()), None, None),
                    };
                    let _ = tx.send(BgResult::Sync {
                        account_index: acct_idx,
                        result,
                        new_index,
                        new_mailbox_states: new_states,
                    });
                });
            }
        }

        Action::OpenComposeWizard(mode) => {
            open_compose_wizard(app, mode);
        }

        Action::ComposeWizardCancel => {
            app.compose_wizard = None;
            app.focus = Focus::List;
            app.set_status("Compose cancelled".to_string());
        }

        Action::ComposeWizardSubmit => {
            submit_compose_wizard(app, terminal)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Compose wizard handlers
// ---------------------------------------------------------------------------

fn open_compose_wizard(app: &mut App, mode: ComposeMode) {
    // Load contact cache (if any) for the active account.
    let contacts = {
        let root = crate::config::account_dir(&app.account_config.name);
        crate::contacts::load_cache(&root).ok().flatten()
    };

    let (to, cc, bcc, subject) = match &mode {
        ComposeMode::New => (String::new(), String::new(), String::new(), String::new()),
        ComposeMode::Forward { source_path } => {
            let subject = crate::draft::fwd_subject_from_source(source_path)
                .unwrap_or_else(|_| String::from("Fwd: "));
            (String::new(), String::new(), String::new(), subject)
        }
    };

    app.compose_wizard = Some(ComposeWizard {
        mode,
        to,
        cc,
        bcc,
        subject,
        focus: ComposeField::To,
        suggestions: Vec::new(),
        suggestion_idx: 0,
        contacts,
    });
    app.focus = Focus::ComposeWizard;
    // No suggestions shown until the user types (see recompute_compose_suggestions).
}

fn submit_compose_wizard(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let Some(mut wizard) = app.compose_wizard.take() else {
        return Ok(());
    };
    app.focus = Focus::List;

    // Normalize the address fields: strip the trailing `, ` that
    // `accept_suggestion` leaves behind for further typing, plus any other
    // trailing separators. Otherwise mailparse::addrparse sees an empty
    // entry at the end and the send fails.
    wizard.to = normalize_recipient_field(&wizard.to);
    wizard.cc = normalize_recipient_field(&wizard.cc);
    wizard.bcc = normalize_recipient_field(&wizard.bcc);
    wizard.subject = wizard.subject.trim().to_string();

    // Basic validation: must have at least one recipient across to/cc/bcc.
    if wizard.to.is_empty() && wizard.cc.is_empty() && wizard.bcc.is_empty() {
        app.set_status_level(
            "Cannot submit: no recipients (to/cc/bcc all empty)".to_string(),
            StatusLevel::Error,
        );
        // Re-open the wizard so the user can fix the field.
        app.compose_wizard = Some(wizard);
        app.focus = Focus::ComposeWizard;
        return Ok(());
    }

    let draft_result = match &wizard.mode {
        ComposeMode::New => write_new_draft_from_wizard(app, &wizard),
        ComposeMode::Forward { source_path } => {
            write_forward_draft_from_wizard(app, source_path, &wizard)
        }
    };

    let path = match draft_result {
        Ok(p) => p,
        Err(e) => {
            app.set_status_level(format!("Draft creation failed: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };

    // Invalidate drafts-mailbox cache so the list reloads on next render.
    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }

    // Hand off to $EDITOR.
    suspend_terminal(terminal)?;
    let edit_result = edit_file(&path);
    resume_terminal(terminal)?;
    match edit_result {
        Ok(()) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            app.set_status(format!("Created: {}", name));
        }
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }
    app.reload_current_mailbox();
    Ok(())
}

fn write_new_draft_from_wizard(app: &App, wizard: &ComposeWizard) -> Result<PathBuf> {
    let dir = app
        .find_mailbox_by_kind(MailboxKind::Drafts)
        .map(|i| app.mailboxes[i].dir.clone())
        .or_else(|| app.drafts_dir.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;

    let default_from = app
        .smtp_config
        .as_ref()
        .map(|s| s.default_from.clone())
        .unwrap_or_else(|| app.account_config.default_from.clone());
    let from = default_from.as_str();
    let now = chrono::Utc::now().to_rfc2822();

    // Build a unique filename from the subject slug (fall back to timestamp).
    let slug = slugify_subject_for_filename(&wizard.subject);
    let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    let base_name = if slug.is_empty() {
        format!("draft-{timestamp}.md")
    } else {
        format!("draft-{timestamp}-{slug}.md")
    };
    let mut path = dir.join(&base_name);
    let mut counter = 1usize;
    while path.exists() {
        let name = if slug.is_empty() {
            format!("draft-{timestamp}-{counter}.md")
        } else {
            format!("draft-{timestamp}-{slug}-{counter}.md")
        };
        path = dir.join(name);
        counter += 1;
    }

    let mut fm = String::from("---\n");
    fm.push_str(&format!("to: {}\n", yaml_escape(&wizard.to)));
    if !wizard.cc.trim().is_empty() {
        fm.push_str(&format!("cc: {}\n", yaml_escape(&wizard.cc)));
    } else {
        fm.push_str("cc:\n");
    }
    if !wizard.bcc.trim().is_empty() {
        fm.push_str(&format!("bcc: {}\n", yaml_escape(&wizard.bcc)));
    } else {
        fm.push_str("bcc:\n");
    }
    fm.push_str(&format!("subject: {}\n", yaml_escape(&wizard.subject)));
    fm.push_str("status: draft\n");
    fm.push_str(&format!("from: \"{from}\"\n"));
    fm.push_str(&format!("date: {now}\n"));
    fm.push_str("reply_to:\n");
    fm.push_str("attachments: []\n");
    fm.push_str("---\n\n");

    std::fs::write(&path, fm)?;
    Ok(path)
}

fn write_forward_draft_from_wizard(
    app: &App,
    source_path: &std::path::Path,
    wizard: &ComposeWizard,
) -> Result<PathBuf> {
    let default_from_owned = app
        .smtp_config
        .as_ref()
        .map(|s| s.default_from.clone())
        .unwrap_or_else(|| app.account_config.default_from.clone());
    let default_from = default_from_owned.as_str();
    let drafts_dir = app
        .find_mailbox_by_kind(MailboxKind::Drafts)
        .map(|i| app.mailboxes[i].dir.clone())
        .or_else(|| app.drafts_dir.clone());
    let path = create_forward_draft(source_path, default_from, drafts_dir.as_deref())?;

    // Patch the frontmatter fields in place.
    patch_draft_frontmatter(&path, wizard)?;
    Ok(path)
}

fn patch_draft_frontmatter(path: &std::path::Path, wizard: &ComposeWizard) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut lines = content.lines();

    // Expect the file to start with `---`.
    let Some(first) = lines.next() else {
        return Ok(());
    };
    if first.trim() != "---" {
        return Ok(());
    }

    // Collect frontmatter lines until the closing `---`.
    let mut fm_lines: Vec<String> = Vec::new();
    let mut body_lines: Vec<String> = Vec::new();
    let mut in_body = false;
    for line in lines {
        if !in_body {
            if line.trim() == "---" {
                in_body = true;
                continue;
            }
            fm_lines.push(line.to_string());
        } else {
            body_lines.push(line.to_string());
        }
    }

    // Rewrite to/cc/bcc/subject; leave everything else alone.
    // Simple single-line field rewriter: replace `key:` lines if found,
    // otherwise append them before the closing `---`.
    let mut rewrote_to = false;
    let mut rewrote_cc = false;
    let mut rewrote_bcc = false;
    let mut rewrote_subject = false;
    for line in fm_lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("to:") {
            *line = format!("to: {}", yaml_escape(&wizard.to));
            rewrote_to = true;
        } else if trimmed.starts_with("cc:") {
            if wizard.cc.trim().is_empty() {
                *line = "cc:".to_string();
            } else {
                *line = format!("cc: {}", yaml_escape(&wizard.cc));
            }
            rewrote_cc = true;
        } else if trimmed.starts_with("bcc:") {
            if wizard.bcc.trim().is_empty() {
                *line = "bcc:".to_string();
            } else {
                *line = format!("bcc: {}", yaml_escape(&wizard.bcc));
            }
            rewrote_bcc = true;
        } else if trimmed.starts_with("subject:") {
            *line = format!("subject: {}", yaml_escape(&wizard.subject));
            rewrote_subject = true;
        }
    }
    if !rewrote_to {
        fm_lines.push(format!("to: {}", yaml_escape(&wizard.to)));
    }
    if !rewrote_cc && !wizard.cc.trim().is_empty() {
        fm_lines.push(format!("cc: {}", yaml_escape(&wizard.cc)));
    }
    if !rewrote_bcc && !wizard.bcc.trim().is_empty() {
        fm_lines.push(format!("bcc: {}", yaml_escape(&wizard.bcc)));
    }
    if !rewrote_subject {
        fm_lines.push(format!("subject: {}", yaml_escape(&wizard.subject)));
    }

    let mut rebuilt = String::from("---\n");
    for line in fm_lines {
        rebuilt.push_str(&line);
        rebuilt.push('\n');
    }
    rebuilt.push_str("---\n");
    for line in body_lines {
        rebuilt.push_str(&line);
        rebuilt.push('\n');
    }
    std::fs::write(path, rebuilt)?;
    Ok(())
}

/// Normalize a recipient-list field on wizard submit:
/// - trim leading/trailing whitespace,
/// - strip trailing commas and whitespace left behind by the
///   "accept suggestion + continue typing" flow,
/// - collapse whitespace inside the separators between recipients.
///
/// Leaves the interior of each recipient alone (including display-name
/// quoting), so `"Hellin, Sylvain" <addr>, bob@x.com, ` becomes
/// `"Hellin, Sylvain" <addr>, bob@x.com`.
fn normalize_recipient_field(s: &str) -> String {
    let trimmed = s.trim();
    // Repeatedly strip any trailing `,` or whitespace chars.
    let cleaned = trimmed.trim_end_matches(|c: char| c == ',' || c.is_whitespace());
    cleaned.to_string()
}

/// YAML double-quote escape for a scalar string value.
fn yaml_escape(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn slugify_subject_for_filename(subject: &str) -> String {
    let slug: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = crate::types::collapse_hyphens(&slug);
    // Trim to a reasonable length so filenames don't blow up.
    slug.chars().take(40).collect()
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
                app.set_status_level(
                    "Failed to save email locally".to_string(),
                    StatusLevel::Error,
                );
            }
        }

        Action::SearchResultOpenInBrowser => {
            if let Some(path) = ensure_search_result_saved(app) {
                let html_path = path.with_extension("html");
                if html_path.exists() {
                    match crate::parse::open_file_with_system(&html_path) {
                        Ok(()) => app.set_status("Opened in browser".to_string()),
                        Err(e) => {
                            app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error)
                        }
                    }
                } else {
                    app.set_status("No HTML version available".to_string());
                }
            } else {
                app.set_status_level(
                    "Failed to save email locally".to_string(),
                    StatusLevel::Error,
                );
            }
        }

        Action::SearchResultReply(reply_all) => {
            if let Some(path) = ensure_search_result_saved(app) {
                let default_from = app
                    .smtp_config
                    .as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_else(|| app.account_config.default_from.clone());
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
                app.set_status_level(
                    "Failed to save email locally".to_string(),
                    StatusLevel::Error,
                );
            }
        }

        Action::SearchResultForward => {
            if let Some(path) = ensure_search_result_saved(app) {
                let default_from = app
                    .smtp_config
                    .as_ref()
                    .map(|s| s.default_from.clone())
                    .unwrap_or_else(|| app.account_config.default_from.clone());
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
                app.set_status_level(
                    "Failed to save email locally".to_string(),
                    StatusLevel::Error,
                );
            }
        }

        Action::SearchResultArchive => {
            if let Some(path) = ensure_search_result_saved(app) {
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

                if app.is_graph() {
                    let graph_config = app.graph_config.clone().unwrap();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = rt
                            .block_on(crate::graph::archive_email_graph(
                                &graph_config,
                                &archive_dir,
                                &path,
                                &archive_server_name,
                            ))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Archive {
                            account_index: acct_idx,
                            result,
                        });
                    });
                } else {
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
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new()
                            .expect("failed to create tokio runtime");
                        let result = rt
                            .block_on(archive_email_locally(
                                &imap_config,
                                &archive_dir,
                                &path,
                                &archive_server_name,
                            ))
                            .map(|()| String::new())
                            .map_err(|e| e.to_string());
                        let _ = tx.send(BgResult::Archive {
                            account_index: acct_idx,
                            result,
                        });
                    });
                }
            } else {
                app.set_status_level(
                    "Failed to save email locally".to_string(),
                    StatusLevel::Error,
                );
            }
        }

        _ => {}
    }
    Ok(())
}

fn parse_name_address(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some(lt) = s.find('<') {
        if let Some(gt) = s.find('>') {
            let name = s[..lt].trim().trim_matches('"').trim().to_string();
            let addr = s[lt + 1..gt].trim().to_string();
            return (name, addr);
        }
    }
    (String::new(), s.to_string())
}

fn parse_graph_recipients(field: Option<&str>) -> Vec<(String, String)> {
    match field {
        Some(s) if !s.trim().is_empty() => crate::send::split_addresses(s)
            .into_iter()
            .map(|a| parse_name_address(&a))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_trailing_comma_and_space() {
        // The exact case the user reported: wizard leaves `, ` dangling after
        // an accepted suggestion.
        let input = "\"Hellin, Sylvain\" <sylvain.hellin@hines.com>, ";
        let out = normalize_recipient_field(input);
        assert_eq!(out, "\"Hellin, Sylvain\" <sylvain.hellin@hines.com>");
    }

    #[test]
    fn normalize_strips_multiple_trailing_separators() {
        assert_eq!(normalize_recipient_field("a@x.com,,  "), "a@x.com");
        assert_eq!(normalize_recipient_field("a@x.com  , "), "a@x.com");
        assert_eq!(normalize_recipient_field("a@x.com"), "a@x.com");
    }

    #[test]
    fn normalize_preserves_multi_recipient_list() {
        assert_eq!(
            normalize_recipient_field("alice@x.com, bob@x.com, "),
            "alice@x.com, bob@x.com"
        );
    }

    #[test]
    fn normalize_preserves_interior_commas_in_quoted_names() {
        let input = "\"Doe, Jane\" <jane@x.com>, \"Roe, John\" <john@x.com>, ";
        let out = normalize_recipient_field(input);
        assert_eq!(
            out,
            "\"Doe, Jane\" <jane@x.com>, \"Roe, John\" <john@x.com>"
        );
    }

    #[test]
    fn normalize_empty_and_whitespace_only() {
        assert_eq!(normalize_recipient_field(""), "");
        assert_eq!(normalize_recipient_field("   "), "");
        assert_eq!(normalize_recipient_field(", ,  "), "");
    }

    /// End-to-end guarantee: the normalized output must round-trip
    /// through `mailparse::addrparse` without leaving an empty entry,
    /// since that's what actually triggered the send failure in the
    /// bug report.
    #[test]
    fn normalized_field_parses_cleanly_with_mailparse() {
        let cases = [
            "\"Hellin, Sylvain\" <sylvain.hellin@hines.com>, ",
            "alice@x.com, bob@x.com, ",
            "Alice <alice@x.com>,",
        ];
        for raw in cases {
            let cleaned = normalize_recipient_field(raw);
            let parsed = mailparse::addrparse(&cleaned)
                .unwrap_or_else(|e| panic!("addrparse failed for {cleaned:?}: {e}"));
            assert!(
                parsed.iter().next().is_some(),
                "no recipients parsed from {cleaned:?}"
            );
            for info in parsed.iter() {
                match info {
                    mailparse::MailAddr::Single(s) => {
                        assert!(
                            !s.addr.trim().is_empty(),
                            "empty address from {cleaned:?}"
                        );
                    }
                    mailparse::MailAddr::Group(g) => {
                        for s in &g.addrs {
                            assert!(
                                !s.addr.trim().is_empty(),
                                "empty group address from {cleaned:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
