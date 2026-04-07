use super::app::{App, BgResult, MailboxKind, SearchOverlayFocus, SearchResultEntry, StatusLevel};
use crate::imap_client::update_read_status_locally;

/// Decrement bg_mutations on the correct account.
fn decrement_mutations(app: &mut App, account_index: usize) {
    if account_index == app.active_account {
        app.bg_mutations = app.bg_mutations.saturating_sub(1);
    } else if let Some(acct) = app.accounts.get_mut(account_index) {
        acct.bg_mutations = acct.bg_mutations.saturating_sub(1);
    }
}

pub(super) fn handle_bg_result(app: &mut App, result: BgResult) {
    app.bg_count = app.bg_count.saturating_sub(1);

    match result {
        BgResult::Archive { account_index, result } => {
            decrement_mutations(app, account_index);
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Email archived".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    if account_index == app.active_account {
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Archive) {
                            app.invalidate_cache_idx(idx);
                        }
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                }
                Err(e) => {
                    app.push_status(format!("Archive failed: {e}"), StatusLevel::Error);
                    if account_index == app.active_account {
                        app.invalidate_all_caches();
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                    app.set_persistent_error(format!(
                        "Archive failed: {e}\nEmail restored to inbox. Sync (F) to fix?"
                    ));
                }
            }
        }

        BgResult::Delete { account_index, result } => {
            decrement_mutations(app, account_index);
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Email deleted".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                }
                Err(e) => {
                    app.push_status(format!("Delete failed: {e}"), StatusLevel::Error);
                    if account_index == app.active_account {
                        app.invalidate_all_caches();
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                    app.set_persistent_error(format!(
                        "Delete failed: {e}\nEmail restored. Sync (F) to fix?"
                    ));
                }
            }
        }

        BgResult::Send { account_index, result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Email sent".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    if account_index == app.active_account {
                        app.invalidate_all_caches();
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                }
                Err(e) => app.set_status_level(format!("Send failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::SendApproved { account_index, result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Approved emails sent".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    if account_index == app.active_account {
                        app.invalidate_all_caches();
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                }
                Err(e) => app.set_status_level(format!("Send-approved failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::Fetch { account_index, result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Fetch complete".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    if account_index == app.active_account {
                        app.invalidate_all_caches();
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                }
                Err(e) => app.set_status_level(format!("Fetch failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::Sync { account_index, result } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() { "Sync complete".into() } else { msg };
                    app.set_status_level(text, StatusLevel::Success);
                    if account_index == app.active_account {
                        app.invalidate_all_caches();
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                }
                Err(e) => app.set_status_level(format!("Sync failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::ToggleRead { account_index, path, new_read_state, result } => {
            // ToggleRead does NOT use bg_mutations -- it doesn't block fetch/sync
            match result {
                Ok(_) => { /* Server confirmed, local already updated optimistically */ }
                Err(e) => {
                    // Rollback local state
                    let reverted = !new_read_state;
                    update_read_status_locally(&path, reverted).ok();
                    if account_index == app.active_account {
                        if let Some(entry) = app.emails.iter_mut().find(|e| e.path == path) {
                            entry.read = reverted;
                        }
                        if let Some(Some(cached)) = app.email_cache.get_mut(app.active_mailbox) {
                            if let Some(ce) = cached.iter_mut().find(|e| e.path == path) {
                                ce.read = reverted;
                            }
                        }
                    }
                    app.push_status(format!("Read status sync failed: {e}"), StatusLevel::Warning);
                }
            }
        }

        BgResult::ServerSearch { result } => {
            app.server_search_loading = false;
            match result {
                Ok(hits) => {
                    let count = hits.len();
                    app.server_search_results = hits
                        .into_iter()
                        .map(|hit| SearchResultEntry {
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
                        app.server_search_focus = SearchOverlayFocus::List;
                    }
                }
                Err(e) => {
                    app.server_search_status = Some(format!("Error: {}", e));
                }
            }
        }
    }
}
