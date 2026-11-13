use super::app::{
    mailbox_key, App, BgResult, MailboxKind, MessageRef, SearchOverlayFocus, StatusLevel,
};

/// The status level of a completed sync/fetch: Success, unless the drain
/// suffix says mutations were rolled back (#0039 review note) or the sync
/// suffix says a fetch is not converging (#0115). Neither may ride a green
/// line.
fn drained_sync_level(text: &str) -> StatusLevel {
    // The warning wins over the skip. A refused tick still drains the queues at
    // its tail (#0114), so the skip line carries the rollback suffix when that
    // drain failed, and a rolled-back mutation must not be reported as
    // information (#0122 review).
    if text.contains(super::helpers::FAILED_OPS_MARKER)
        || text.contains(super::helpers::NON_CONVERGING_MARKER)
    {
        return StatusLevel::Warning;
    }
    // A tick that was refused the engine lock did nothing at all (#0122): not a
    // green sync that happened, not a red one that failed.
    if text.contains(super::helpers::SYNC_SKIPPED_MARKER) {
        return StatusLevel::Info;
    }
    StatusLevel::Success
}

/// Whether a `BgResult::MailboxLoaded` may be applied or must be dropped
/// as stale (P1 step 2). A background walk is only valid if the user is
/// still looking at the same account and mailbox it was requested for,
/// AND no newer request or optimistic list mutation has bumped the
/// generation since (an older walk could resurrect an archived/deleted
/// email or clobber a newer reload's result).
fn mailbox_loaded_is_current(
    active_account: usize,
    active_mailbox: usize,
    current_generation: u64,
    account_index: usize,
    mailbox_idx: usize,
    generation: u64,
) -> bool {
    account_index == active_account
        && mailbox_idx == active_mailbox
        && generation == current_generation
}

/// Bring the list back in step with the store after a fetch or sync wrote to
/// it (#0038 follow-up).
///
/// Ingest inserts rows, the flag pass rewrites `\Seen` and the prune deletes
/// rows the server no longer lists, in every target mailbox of the account. The
/// list reads those same rows, so all of that account's per-mailbox caches are
/// dropped. The mailbox the user is looking at is then reloaded off the UI
/// thread through the same `request_mailbox_load` path a mailbox switch takes,
/// and the sidebar counts are recomputed with one grouped query.
///
/// An inactive account keeps only the cache drop: it has no list on screen and
/// no counts to redraw, and switching to it reloads from the store anyway.
fn refresh_after_server_sync(app: &mut App, account_index: usize) {
    if account_index == app.active_account {
        app.invalidate_all_caches();
        app.reload_current_mailbox();
        app.recount_all_mailboxes();
    } else {
        app.invalidate_all_caches_on(account_index);
    }
}

/// Render a settled `contact.rebuild` the way the pre-daemon rebuild rendered
/// its own verdict (#0126).
///
/// The four lines are `App::refresh_contacts`'s, unchanged: `written` is the
/// refreshed count, the two refusals are #0053's guard keeping the cache that
/// is on disk, and a failure is the red line. The one that moved is the save
/// error, which was a line of its own (#0067) and is the operation's failure
/// now, with the save's own error behind it.
///
/// A rebuild that settles after the user switched accounts still says what it
/// did and does not touch the view: the Contacts pane is the *active*
/// account's, and `reset_contacts_view` has already emptied it.
fn apply_contacts_rebuild(
    app: &mut App,
    account_index: usize,
    result: Result<serde_json::Value, String>,
) {
    let settled = match result {
        Ok(settled) => settled,
        Err(e) => {
            return app
                .set_status_level(format!("Contacts refresh failed: {e}"), StatusLevel::Error);
        }
    };
    let contacts = settled["contacts"].as_u64().unwrap_or(0);
    let kept = settled["kept"].as_u64().unwrap_or(0);
    let saved = settled["saved"].as_str();
    if let Some(line) =
        saved.and_then(|s| mp_client::format::refused_rebuild_line(s, contacts, kept))
    {
        return app.set_status_level(line, StatusLevel::Warning);
    }
    match saved {
        Some("written") => {}
        other => log::warn!(
            "[tui] contact.rebuild settled with an unknown verdict {other:?}; \
             treating it as written"
        ),
    }
    // The index does not travel with the settle: the view asks the daemon for
    // it again, as `contact.changed` makes it do for a rebuild another client
    // ran. The cache file is the daemon's and is not read here.
    if account_index == app.active_account {
        app.reload_contacts();
    }
    app.set_status(format!("Contacts refreshed ({contacts})"));
}

/// A drain outside a tick rolled `failed` of `account_index`'s queued
/// mutations back (#0133): the warning a tick's rollback suffix earns, and the
/// refresh a tick owes, so the rows it put back reappear, the open search's
/// hits among them.
pub(super) fn land_rollback(app: &mut App, account_index: usize, failed: u64) {
    let prefix = match app.accounts.get(account_index) {
        Some(acct) if app.accounts.len() > 1 => format!("{}: ", acct.account_config.name),
        _ => String::new(),
    };
    app.set_status_level(
        format!(
            "{prefix}{}",
            mp_client::format::mutations_rolled_back_line(failed)
        ),
        StatusLevel::Warning,
    );
    refresh_after_server_sync(app, account_index);
    rerun_search_after_rollback(app, account_index);
}

/// Mutations of `account_index` were rolled back, by a drain outside a tick
/// or by a tick's own: the open search overlay of that account runs its
/// search again (PERSO-286), since the move's `state.remove` dropped the hit
/// it named and the rollback put the message back where the hit listed it. A
/// closed overlay opens on a blank form, so it has nothing to run again.
fn rerun_search_after_rollback(app: &mut App, account_index: usize) {
    if account_index == app.active_account && matches!(app.overlay, super::app::Overlay::Search) {
        app.rerun_last_search();
    }
}

/// The overlay footer a settled `message.search_server` paints: the count of
/// hits in the list, the CLI's caveat when the store answered a term (`local
/// store only: has:attachment`, PERSO-131), the mailboxes that refused, and
/// the CLI's sentence when the limit or Graph's cap cut the answer
/// (PERSO-175), with a narrower search as the remedy, since the overlay takes
/// no limit.
///
/// `count` is the list's, not the settle's `hits`: the list also holds the
/// local pass's hits, which the daemon deduplicated against. `order` is what
/// the hits are in, [`crate::app::search_order_label`]'s words (PERSO-273).
pub(crate) fn server_search_footer(
    count: usize,
    order: Option<&str>,
    settled: &serde_json::Value,
) -> String {
    let settled: mp_protocol::listing::ServerSearchSettled =
        serde_json::from_value(settled.clone()).unwrap_or_default();
    let mut line = format!("{count} result{}", if count == 1 { "" } else { "s" });
    if let Some(order) = order {
        line.push_str(&format!(", {order}"));
    }
    if let Some(note) = settled.local_store_note() {
        line.push_str(&format!(" ({note})"));
    }
    let unreachable = settled.unreachable.len();
    if unreachable > 0 {
        line.push_str(&format!(
            "; {unreachable} mailbox{} unreachable",
            if unreachable == 1 { "" } else { "es" }
        ));
    }
    if settled.capped {
        line.push_str(&format!(". {}", mp_client::format::graph_search_cap_line()));
    } else if settled.truncated {
        line.push_str(&format!(
            ". {}",
            mp_client::format::more_results_line(count, "narrow the search to see the rest")
        ));
    }
    line
}

/// ` (name)` for a known account of a multi-account setup, empty otherwise,
/// for status lines that would not otherwise say which account they are about.
fn account_label(app: &App, account_index: usize) -> String {
    match app.accounts.get(account_index) {
        Some(acct) if app.accounts.len() > 1 => format!(" ({})", acct.account_config.name),
        _ => String::new(),
    }
}

/// Record the outcome of one account's sync on that account (#0071).
///
/// Every sync completion path lands here: the startup multi-account fetch, the
/// watcher-triggered quick sync, a manual `F`, a full `S`, over IMAP or Graph
/// alike, because all of them arrive as `BgResult::Fetch` or `BgResult::Sync`
/// carrying the account they belong to. Writing the outcome *on the account*
/// rather than into the shared status line is the whole fix: an account that
/// failed keeps its mark while the accounts that succeeded overwrite the line
/// (#0068).
fn record_sync_health(app: &mut App, account_index: usize, outcome: Result<(), &str>) {
    let now = chrono::Local::now();
    if let Some(acct) = app.accounts.get_mut(account_index) {
        acct.sync_health = acct.sync_health.updated(outcome, now);
    }
}

pub fn handle_bg_result(app: &mut App, result: BgResult) {
    app.bg_count = app.bg_count.saturating_sub(1);
    match result {
        BgResult::Send {
            account_index,
            result,
        } => {
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() {
                        "Email sent".into()
                    } else {
                        msg
                    };
                    app.set_status_level(text, StatusLevel::Success);
                    if account_index == app.active_account {
                        // Send moves a draft to Sent -- only those two need invalidation
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                            app.invalidate_cache_idx(idx);
                        }
                        if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Sent) {
                            app.invalidate_cache_idx(idx);
                        }
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                }
                Err(e) => app.set_status_level(format!("Send failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::Rsvp {
            account_index,
            result,
        } => {
            match result {
                Ok(msg) => {
                    app.set_status_level(msg, StatusLevel::Success);
                    // An iMIP reply is now a row in `sent` (the outbox
                    // ingests the appended copy during the send), so the
                    // derived own-RSVP has changed: refresh the open mailbox.
                    // The Calendar view reads its days again here as well as
                    // on the `calendar.changed` the answer publishes, quietly,
                    // so the line above stays.
                    if account_index == app.active_account {
                        app.invalidate_cache_idx(app.active_mailbox);
                        app.reload_current_mailbox();
                    } else {
                        app.invalidate_all_caches_on(account_index);
                    }
                    app.rebuild_calendar_if_loaded();
                }
                Err(e) => app.set_status_level(format!("RSVP failed: {e}"), StatusLevel::Error),
            }
        }

        BgResult::EventCreated {
            operation_id,
            result,
        } => {
            let form = app.calendar_view.pending_forms.remove(&operation_id);
            match (result, form) {
                (Ok(line), _) => {
                    app.set_status_level(line, StatusLevel::Success);
                    app.rebuild_calendar_if_loaded();
                }
                (Err(e), Some(form)) => app.reopen_event_form(
                    form,
                    format!("The event was not created: {e}; Enter retries it"),
                ),
                (Err(e), None) => app.set_status_level(
                    format!("The event was not created: {e}"),
                    StatusLevel::Error,
                ),
            }
        }

        BgResult::FreeBusy {
            operation_id,
            result,
        } => app.land_free_busy(&operation_id, result),

        BgResult::EventChanged {
            operation_id,
            result,
        } => {
            let form = app.calendar_view.pending_forms.remove(&operation_id);
            let removal = app.calendar_view.pending_removals.remove(&operation_id);
            match (result, form, removal) {
                (Ok(line), _, _) => {
                    app.set_status_level(line, StatusLevel::Success);
                    app.rebuild_calendar_if_loaded();
                }
                (Err(e), Some(form), _) => app.reopen_event_form(
                    form,
                    format!("The change did not go through: {e}; Enter sends it again"),
                ),
                (Err(e), None, Some(removal)) => {
                    let verb = if removal.cancel {
                        "Cancelling"
                    } else {
                        "Deleting"
                    };
                    let why = format!("{verb} {} failed: {e}", removal.summary);
                    app.reopen_event_remove(removal, why);
                }
                (Err(e), None, None) => app.set_status_level(
                    format!("The event was not changed: {e}"),
                    StatusLevel::Error,
                ),
            }
        }

        BgResult::SendApproved {
            account_index,
            result,
        } => match result {
            Ok(msg) => {
                let text = if msg.is_empty() {
                    "Approved emails sent".into()
                } else {
                    msg
                };
                app.set_status_level(text, StatusLevel::Success);
                if account_index == app.active_account {
                    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                        app.invalidate_cache_idx(idx);
                    }
                    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Sent) {
                        app.invalidate_cache_idx(idx);
                    }
                    app.reload_current_mailbox();
                } else {
                    app.invalidate_all_caches_on(account_index);
                }
            }
            Err(e) => {
                app.set_status_level(format!("Send-approved failed: {e}"), StatusLevel::Error)
            }
        },

        BgResult::Fetch {
            account_index,
            result,
            rolled_back,
        } => land_sync(app, account_index, result, rolled_back),

        BgResult::Sync {
            account_index,
            result,
            rolled_back,
        } => {
            record_sync_health(
                app,
                account_index,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
            );
            match result {
                Ok(msg) => {
                    let text = if msg.is_empty() {
                        "Sync complete".into()
                    } else {
                        msg
                    };
                    let level = drained_sync_level(&text);
                    app.set_status_level(text, level);
                    refresh_after_server_sync(app, account_index);
                }
                Err(e) => {
                    let name = account_label(app, account_index);
                    app.set_status_level(format!("Sync failed{name}: {e}"), StatusLevel::Error)
                }
            }
            if rolled_back > 0 {
                rerun_search_after_rollback(app, account_index);
            }
        }

        BgResult::MailboxLoaded {
            account_index,
            mailbox_idx,
            generation,
            entries,
        } => {
            if !mailbox_loaded_is_current(
                app.active_account,
                app.active_mailbox,
                app.mailbox_load_generation,
                account_index,
                mailbox_idx,
                generation,
            ) {
                // Stale: the user switched account/mailbox, a newer reload
                // superseded this one, or an optimistic mutation
                // (archive/delete) made this walk's snapshot unsafe to
                // apply. Do NOT populate the cache either -- the walk may
                // predate an in-flight file move. The cache slot stays
                // `None`, so the next visit triggers a fresh load.
                //
                // Dropped or not, the conversation listing in flight is
                // done, and one a delta owed meanwhile starts now (#0142).
                app.restream_landed(generation);
                return;
            }
            // The cursor's identity must be read from the OLD list: a
            // reload re-sorts (an approved draft changes status/date) and
            // grows (new inbox mail shifts every row down), so the bare
            // `list_index` would land on a different email. Anchor on the
            // message ref, fall back to the clamped index when that email
            // is gone from the fresh list.
            let anchor = app.cursor_anchor();
            let fallback = app.list_index;
            let entries = std::sync::Arc::new(entries);
            if let Some(slot) = app.email_cache.get_mut(mailbox_idx) {
                // Cache slot and `app.emails` share the allocation (P2):
                // no deep clone on delivery.
                *slot = Some(std::sync::Arc::clone(&entries));
            }
            app.emails = entries;
            // A key the fresh list no longer holds must leave the selection
            // with it (#0052): an externally deleted draft is gone from the
            // index, not just from the screen.
            app.scrub_selection();
            // Reapply the active search filter (if any) to the fresh
            // entries, then put the cursor back on the anchored email.
            app.rebuild_visible();
            app.restore_cursor(anchor, fallback);
            // A conversation-overlay jump into this mailbox parked a target
            // (#0008); now that the fresh list is here, put the cursor on it,
            // overriding the anchor restore above.
            app.consume_pending_select();
            app.sync_listed_count(mailbox_idx);
            // Clear the "Loading <mailbox>..." indication set by a
            // cache-miss mailbox switch (harmless no-op otherwise).
            if matches!(&app.status_message, Some(m) if m.starts_with("Loading ")) {
                app.status_message = None;
                app.status_ticks = 0;
            }
            app.restream_landed(generation);
        }

        BgResult::ServerSearch { generation, result } => {
            // A settle for a search the user has since re-submitted must not
            // relabel the newer search's footer (#0105).
            if generation != app.server_search_generation {
                return;
            }
            app.server_search_loading = false;
            app.server_search_operation = None;
            match result {
                Ok(settled) => {
                    // The hits are already in the list: each one arrived as a
                    // `message.server_hit` event while the search ran, and the
                    // dedup against the local pass was the daemon's. What is
                    // left is the count the footer prints and the mailboxes
                    // that refused, which do not fail the search, and the
                    // terms the store answered (PERSO-131). The server's hits
                    // take the settle's `hit_order` (PERSO-273).
                    arrange_server_hits(app, &settled);
                    let count = app.server_search_results.len();
                    let order = crate::app::search_order_label(
                        app.server_search_local_order,
                        app.server_search_server_order,
                    );
                    app.server_search_status =
                        Some(server_search_footer(count, order.as_deref(), &settled));
                    if count > 0 && app.server_search_moves_focus {
                        app.server_search_focus = SearchOverlayFocus::List;
                    }
                }
                Err(e) => {
                    app.server_search_status = Some(format!("Error: {}", e));
                }
            }
        }

        BgResult::ContactsRebuilt {
            account_index,
            result,
        } => apply_contacts_rebuild(app, account_index, result),

        BgResult::ContactsLoaded {
            account_index,
            generation,
            result,
        } => {
            app.bg_count = app.bg_count.saturating_sub(1);
            app.land_contacts(account_index, generation, result);
        }

        BgResult::SearchHitFetched {
            generation,
            message_id,
            result,
        } => {
            if generation != app.server_search_generation {
                return;
            }
            apply_search_hit_fetch(app, &message_id, result);
        }

        BgResult::AccountOpened {
            account_index,
            counts,
            credentials,
        } => {
            // Phase two of startup (#0003): this account's store opened on a
            // background thread, ran its integrity check (and, on failure, the
            // drop-and-rebuild path of #0066) and read the real counts. Fill
            // them in and drop the loading marker. The daemon's word on its
            // credentials lands first, because the auto-fetch below reads it.
            let is_remote = if let Some(acct) = app.accounts.get_mut(account_index) {
                acct.mailbox_counts = counts.clone();
                acct.opening = false;
                if credentials.is_some() {
                    acct.credentials = credentials;
                }
                acct.has_imap() || acct.has_graph()
            } else {
                false
            };

            if account_index == app.active_account {
                // Mirror the counts into the live view and load the open
                // mailbox now -- `App::new` deliberately left it empty rather
                // than pay the store open before the first paint. The load
                // runs off the UI thread via `BgResult::MailboxLoaded`, and
                // the store it opens is already validated, so no second
                // integrity check.
                app.mailbox_counts = counts;
                app.reload_current_mailbox();
            }

            // Startup auto-fetch (#0001), sequenced *after* the open: one quick
            // sync per account with a remote source. `FetchAccount` is ungated,
            // so accounts still sync concurrently; deferring it to here (rather
            // than queueing it before the store existed) avoids a sync racing
            // the first open of the same file and a redundant integrity check.
            if is_remote {
                app.push_action(super::app::Action::FetchAccount(account_index));
            }
        }
    }
}

/// Land one finished quick pass: the status line, the account's health mark
/// and the refresh the rows owe. The new mail it brought is notified by its
/// own event, `mail.arrived` (PERSO-186).
///
/// Two callers and one behaviour (P5-U8). A pass this client asked for arrives
/// as [`BgResult::Fetch`] when its `operation.finished` lands; a pass the
/// daemon's own watcher ran arrives as a `sync.completed` event nobody asked
/// for. Both are one tick over one account and read the same way, and the
/// account's mark (#0071) and the wording (#0068) may not depend on which of
/// the two brought it.
///
/// A pass whose drains rolled mutations back (`rolled_back`, its
/// `failed_mutations`) runs the open search again as a drain outside a tick
/// does ([`land_rollback`]).
///
/// `bg_count` is deliberately not touched here: the caller that started
/// background work is the caller that counts it down.
pub(super) fn land_sync(
    app: &mut App,
    account_index: usize,
    result: Result<String, String>,
    rolled_back: u64,
) {
    record_sync_health(
        app,
        account_index,
        result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
    );
    match result {
        Ok(msg) => {
            let text = if msg.is_empty() {
                "Fetch complete".into()
            } else {
                msg
            };
            let level = drained_sync_level(&text);
            app.set_status_level(text, level);
            refresh_after_server_sync(app, account_index);
        }
        Err(e) => {
            // Named, because a multi-account run turns an anonymous "Fetch
            // failed" into a line nobody can act on (#0068).
            let name = account_label(app, account_index);
            app.set_status_level(format!("Fetch failed{name}: {e}"), StatusLevel::Error)
        }
    }
    if rolled_back > 0 {
        rerun_search_after_rollback(app, account_index);
    }
}

/// Fold one row delta into the open list, or ask for the mailbox again
/// (P5-U4).
///
/// The consumer end of [`crate::queries::MessageRowDelta`], called by the
/// event drain (P5-U8): a removal drops the row, and an invalidate or a
/// removal of a uid this session never listed reloads the mailbox through the
/// same off-thread path a mailbox switch takes.
///
/// The cache slot moves with the list, because the two share the allocation and
/// a slot that kept the pre-delta rows would undo the delta on the next visit.
///
/// A delta about a mailbox not on screen, of this account or another, drops
/// that mailbox's cache slot, so its next visit reads it again rather than
/// showing a row another client or the daemon moved away (PERSO-286); a
/// removal also drops the search hit it names.
pub(super) fn apply_row_delta(app: &mut App, delta: &super::queries::MessageRowDelta) -> bool {
    if let super::queries::MessageRowDelta::Remove {
        account,
        mailbox,
        uid,
    } = delta
    {
        drop_removed_hit(app, account, mailbox, *uid);
    }
    if delta.account() != app.account_config.name {
        if let Some(index) = app
            .accounts
            .iter()
            .position(|acct| acct.account_config.name == delta.account())
        {
            if let Some(idx) = app.accounts[index]
                .mailboxes
                .iter()
                .position(|mb| mailbox_key(mb) == delta.mailbox())
            {
                app.invalidate_cache_idx_on(index, idx);
            }
        }
        return true;
    }
    // A conversation list cannot fold a row delta (#0142): the row carries no
    // `thread_id`, and a delta about Sent moves a conversation listed in the
    // inbox. Any delta of the account re-streams the list, coalesced.
    if app.conversation_mode() {
        app.request_restream();
        return false;
    }
    let Some(mailbox) = app.current_local_mailbox_key() else {
        return true;
    };
    if delta.mailbox() != mailbox {
        if let Some(idx) = app
            .mailboxes
            .iter()
            .position(|mb| mailbox_key(mb) == delta.mailbox())
        {
            app.invalidate_cache_idx(idx);
        }
        return true;
    }
    let mut rows = (*app.emails).clone();
    if super::queries::apply_row_delta(&mut rows, &mailbox, delta) {
        let rows = std::sync::Arc::new(rows);
        if let Some(slot) = app.email_cache.get_mut(app.active_mailbox) {
            *slot = Some(std::sync::Arc::clone(&rows));
        }
        app.emails = rows;
        app.scrub_selection();
        app.rebuild_visible();
        return true;
    }
    app.reload_current_mailbox();
    false
}

/// Drop the search hit of `account` that a `state.remove` names by its
/// mailbox and uid (PERSO-286): the message left the mailbox the hit lists,
/// as the overlay's own `a` drops a hit it archived. The search is the active
/// account's, and only a local hit knows its uid.
fn drop_removed_hit(app: &mut App, account: &str, mailbox: &str, uid: i64) {
    if account != app.account_config.name {
        return;
    }
    let Some(index) = app
        .server_search_results
        .iter()
        .position(|hit| hit.uid == Some(uid) && hit.source_label == mailbox)
    else {
        return;
    };
    app.server_search_results.remove(index);
    if index < app.server_search_index {
        app.server_search_index -= 1;
    }
    if app.server_search_index >= app.server_search_results.len() {
        app.server_search_index = app.server_search_results.len().saturating_sub(1);
    }
}

/// Land a fetch-into-store result on the overlay (#0104): the hit named by
/// its Message-ID becomes a resolved row, and the mailbox lists pick the new
/// row up. The fetch is `message.fetch` since P5-U10c, so this lands one
/// settled operation rather than two client-side paths.
pub(super) fn apply_search_hit_fetch(app: &mut App, message_id: &str, result: Result<i64, String>) {
    match result {
        Ok(row_id) => {
            if let Some(hit) = app
                .server_search_results
                .iter_mut()
                .find(|r| r.fetched.message_id.as_deref() == Some(message_id))
            {
                hit.entry.msg = Some(MessageRef::new(row_id));
            }
            app.server_search_status = Some("Fetched into the local store".to_string());
            // The new row must show up in its mailbox's list and count.
            app.invalidate_all_caches();
            app.recount_all_mailboxes();
            app.reload_current_mailbox();
        }
        Err(e) => {
            app.server_search_status = Some(format!("Fetch failed: {e}"));
        }
    }
}

/// One streamed `message.server_hit` (`LST-08`, #0126).
///
/// The overlay's list is the local-first pass's (#0105) with the server's hits
/// added as they arrive, where it used to grow by one batch when the last
/// mailbox answered. The daemon did the dedup against the Message-IDs the
/// local pass was showing, and it orders the hits in its settle's `hit_order`
/// (PERSO-273). Under newest or oldest first a hit goes in at its date, where
/// the settle's merge will leave it, so the list is never out of order while
/// the hits stream; under best match first it is appended after the local
/// pass's ranked hits, and so is every hit of a daemon that numbers none (one
/// that predates PERSO-273 and orders nothing). The cursor stays on its hit.
///
/// A hit for an operation this overlay is not showing is dropped: a fast
/// retype leaves two searches in flight, and the id is what tells them apart.
pub(super) fn apply_server_hit(app: &mut App, payload: &serde_json::Value) {
    let operation = payload["operation_id"].as_str();
    if operation.is_none() || operation != app.server_search_operation.as_deref() {
        return;
    }
    let hit: mp_protocol::listing::ServerSearchHit =
        match serde_json::from_value(payload["hit"].clone()) {
            Ok(hit) => hit,
            Err(e) => {
                log::warn!("[events] a message.server_hit did not decode: {e}");
                return;
            }
        };
    let seq = payload["seq"].as_u64();
    let entry = super::commands::hit_entry(&hit, seq.unwrap_or_default());
    let results = &app.server_search_results;
    let merge = app
        .search_order
        .on_server()
        .date_merge(app.server_search_local_order)
        .filter(|_| seq.is_some());
    let at = match merge {
        Some(order) => order.date_position(results, &entry.entry.date_sort, |hit| {
            hit.entry.date_sort.as_str()
        }),
        None => results.len(),
    };
    if at <= app.server_search_index && at < results.len() {
        app.server_search_index += 1;
    }
    app.server_search_results.insert(at, entry);
    let count = app.server_search_results.len();
    app.server_search_status = Some(format!(
        "{count} result{}; searching server...",
        if count == 1 { "" } else { "s" }
    ));
    if app.server_search_moves_focus {
        app.server_search_focus = SearchOverlayFocus::List;
    }
}

/// A settled server search's hits in its `hit_order` (PERSO-273), merged by
/// date with the local pass's under newest and oldest first and after them
/// under best match first
/// ([`mp_protocol::listing::ServerSearchSettled::arrange_after`]); the cursor
/// stays on its hit.
///
/// The hits streamed mailbox by mailbox as each one's survivors downloaded,
/// so the order they arrived in is no order at all; the settle's is the one
/// list every client shows. The order it echoes is what the overlay names
/// for them, and an older daemon's settle, which echoes none, reads as
/// relevance, as its local pass does.
///
/// The cursor follows the row it was on by the row's place before the
/// settle, not by its `seq`: an older daemon numbers no hit, so every server
/// hit carries the same `seq`.
fn arrange_server_hits(app: &mut App, settled: &serde_json::Value) {
    let decoded: mp_protocol::listing::ServerSearchSettled =
        serde_json::from_value(settled.clone()).unwrap_or_default();
    let cursor = app.server_search_index;
    let (local, server): (Vec<_>, Vec<_>) = std::mem::take(&mut app.server_search_results)
        .into_iter()
        .enumerate()
        .partition(|(_, hit)| hit.seq.is_none());
    app.server_search_server_order =
        (!server.is_empty()).then(|| mp_client::queries::decode_search_order(settled));
    let arranged = decoded.arrange_after(
        app.server_search_local_order,
        local,
        server
            .into_iter()
            .map(|(at, hit)| (hit.seq.unwrap_or_default(), (at, hit))),
        |(_, hit)| hit.entry.date_sort.as_str(),
    );
    if let Some(at) = arranged.iter().position(|(was, _)| *was == cursor) {
        app.server_search_index = at;
    }
    app.server_search_results = arranged.into_iter().map(|(_, hit)| hit).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // drained_sync_level (#0039 review note)
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // server_search_footer (PERSO-131)
    // -----------------------------------------------------------------------

    /// A settle whose `has:attachment` the store answered carries the CLI's
    /// caveat beside the count; one the server answered whole, or one from a
    /// daemon that predates the field, keeps the plain count.
    #[test]
    fn a_locally_answered_term_is_named_in_the_overlay_footer() {
        let local = serde_json::json!({
            "account": "tum", "query": "has:attachment", "hits": 2,
            "deduplicated": 0, "unreachable": [],
            "coverage": "local", "local_terms": ["has:attachment"],
        });
        assert_eq!(
            server_search_footer(2, None, &local),
            "2 results (local store only: has:attachment)"
        );
        assert_eq!(
            server_search_footer(0, None, &local),
            "0 results (local store only: has:attachment)"
        );

        let mut refused = local.clone();
        refused["unreachable"] = serde_json::json!([{"mailbox": "Archive", "error": "timeout"}]);
        assert_eq!(
            server_search_footer(1, None, &refused),
            "1 result (local store only: has:attachment); 1 mailbox unreachable"
        );

        let server = serde_json::json!({
            "account": "tum", "query": "from:borrmann", "hits": 3,
            "deduplicated": 0, "unreachable": [],
            "coverage": "server", "local_terms": [],
        });
        assert_eq!(server_search_footer(3, None, &server), "3 results");
        assert_eq!(
            server_search_footer(3, Some("oldest first"), &server),
            "3 results, oldest first"
        );

        let older = serde_json::json!({
            "account": "tum", "query": "from:borrmann", "hits": 3,
            "deduplicated": 0,
            "unreachable": [{"mailbox": "Archive", "error": "timeout"}, {"mailbox": "Sent", "error": "no"}],
        });
        assert_eq!(
            server_search_footer(3, None, &older),
            "3 results; 2 mailboxes unreachable"
        );
    }

    /// A settle the limit cut says so after the count, in the CLI's words
    /// with the overlay's remedy; one Graph's cap cut names the cap instead
    /// (PERSO-175).
    #[test]
    fn a_cut_server_search_says_so_in_the_overlay_footer() {
        let cut = serde_json::json!({
            "account": "tum", "query": "ledger", "hits": 50,
            "deduplicated": 0, "unreachable": [],
            "coverage": "server", "local_terms": [],
            "truncated": true, "capped": false,
        });
        assert_eq!(
            server_search_footer(53, None, &cut),
            "53 results. More results than the 53 shown: narrow the search to see the rest."
        );

        let mut capped = cut.clone();
        capped["capped"] = serde_json::json!(true);
        assert_eq!(
            server_search_footer(1000, None, &capped),
            "1000 results. Microsoft Graph stops a search at 1000 results: narrow the query \
             (after:, before:, in:) to reach the rest."
        );
    }

    // -----------------------------------------------------------------------
    // The order of the streamed hits (PERSO-273)
    // -----------------------------------------------------------------------

    /// One overlay row of the local pass, its message named by `subject`,
    /// dated by the wire `date_sort` the store's row carries.
    fn local_row(subject: &str, date_sort: &str) -> crate::app::SearchResultEntry {
        let fetched = mp_core::parse::FetchedEmail {
            from: "a@example.com".into(),
            to: "me@example.com".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: subject.into(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".into(),
            body_text: String::new(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(format!("<{subject}@example.com>")),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        let mut entry = crate::helpers::fetched_to_email_entry(None, None, &fetched);
        entry.date_sort = date_sort.into();
        crate::app::SearchResultEntry {
            entry,
            fetched,
            source_label: "inbox".into(),
            seq: None,
            uid: None,
        }
    }

    /// The `message.server_hit` payload of hit `seq` of operation `op-1`,
    /// dated `date_sort`; a `seq` of `None` is an older daemon's, which
    /// numbers no hit.
    fn hit_event(seq: Option<u64>, subject: &str, date_sort: &str) -> serde_json::Value {
        let hit = mp_protocol::listing::ServerSearchHit {
            account: "tum".into(),
            mailbox: "Archive".into(),
            subject: subject.into(),
            message_id: Some(format!("<{subject}@example.com>")),
            date_sort: date_sort.into(),
            ..Default::default()
        };
        let mut event = serde_json::json!({"operation_id": "op-1", "hit": hit});
        if let Some(seq) = seq {
            event["seq"] = serde_json::json!(seq);
        }
        event
    }

    /// Settle the overlay's search `op-1` (generation 2) with `settled`.
    fn settle(app: &mut App, settled: serde_json::Value) {
        handle_bg_result(
            app,
            BgResult::ServerSearch {
                generation: 2,
                result: Ok(settled),
            },
        );
    }

    /// An overlay whose local pass listed `local`, echoing `local_order`, and
    /// whose server search `op-1` is running.
    fn searching(
        order: mp_protocol::listing::SearchOrder,
        local: Vec<crate::app::SearchResultEntry>,
        local_order: Option<mp_protocol::listing::SearchOrder>,
    ) -> App {
        let mut app = App::default_for_tests();
        app.search_order = order;
        app.server_search_generation = 2;
        app.server_search_operation = Some("op-1".into());
        app.server_search_loading = true;
        app.server_search_results = local;
        app.server_search_local_order = local_order;
        app
    }

    fn subjects(app: &App) -> Vec<&str> {
        app.server_search_results
            .iter()
            .map(|hit| hit.entry.subject.as_str())
            .collect()
    }

    /// Newest first, a server-only hit dated today goes above the older local
    /// hits the moment it streams in, not under them, and every other one at
    /// its date, so the list is never out of order; the cursor stays on its
    /// hit as rows go in above it. The settle's merge leaves the list as the
    /// stream built it, and the footer reads plainly "newest first".
    #[test]
    fn newest_first_puts_each_server_hit_at_its_date_among_the_local_hits() {
        use mp_protocol::listing::SearchOrder;
        let mut app = searching(
            SearchOrder::Newest,
            vec![
                local_row("local-may", "2026-05-01T09:00:00"),
                local_row("local-march", "2026-03-01T09:00:00"),
            ],
            Some(SearchOrder::Newest),
        );
        app.server_search_index = 1;

        apply_server_hit(
            &mut app,
            &hit_event(Some(0), "server-april", "2026-04-01T09:00:00"),
        );
        apply_server_hit(
            &mut app,
            &hit_event(Some(1), "server-today", "2026-07-02T08:00:00"),
        );
        apply_server_hit(&mut app, &hit_event(Some(2), "server-undated", ""));
        // A hit of a search the overlay no longer shows is dropped.
        let mut stale = hit_event(Some(3), "stale", "2026-07-03T08:00:00");
        stale["operation_id"] = serde_json::json!("op-0");
        apply_server_hit(&mut app, &stale);
        let streamed = [
            "server-today",
            "local-may",
            "server-april",
            "local-march",
            "server-undated",
        ];
        assert_eq!(subjects(&app), streamed, "each hit went in at its date");
        assert_eq!(
            app.server_search_index, 3,
            "the cursor stays on local-march as rows go in above it"
        );

        settle(
            &mut app,
            serde_json::json!({
                "account": "tum", "query": "ledger", "hits": 3,
                "order": "newest", "hit_order": [1, 0, 2],
            }),
        );
        assert_eq!(subjects(&app), streamed);
        assert_eq!(app.server_search_index, 3, "still on local-march");
        assert_eq!(
            app.server_search_status.as_deref(),
            Some("5 results, newest first")
        );
    }

    /// Best match first, the local pass's ranked hits stay on top: the
    /// server's are appended as they stream and the settle lists them in its
    /// `hit_order` after the local pass's, the cursor following its hit, and
    /// the footer names both orders the daemon echoed.
    #[test]
    fn best_match_first_lists_the_server_hits_in_hit_order_after_the_local_pass() {
        use mp_protocol::listing::SearchOrder;
        let mut app = searching(
            SearchOrder::Relevance,
            vec![
                local_row("local-b", "2026-03-01T09:00:00"),
                local_row("local-a", "2026-05-01T09:00:00"),
            ],
            Some(SearchOrder::Relevance),
        );
        for (seq, subject, date) in [
            (0, "arrived-first", "2026-04-01T09:00:00"),
            (1, "arrived-second", "2026-02-01T09:00:00"),
            (2, "arrived-third", "2026-07-02T08:00:00"),
        ] {
            apply_server_hit(&mut app, &hit_event(Some(seq), subject, date));
        }
        assert_eq!(
            subjects(&app),
            [
                "local-b",
                "local-a",
                "arrived-first",
                "arrived-second",
                "arrived-third"
            ],
            "a streamed hit is appended where it arrived"
        );
        app.server_search_index = 3;

        settle(
            &mut app,
            serde_json::json!({
                "account": "tum", "query": "ledger", "hits": 3,
                "order": "newest", "hit_order": [2, 0, 1],
            }),
        );
        assert_eq!(
            subjects(&app),
            [
                "local-b",
                "local-a",
                "arrived-third",
                "arrived-first",
                "arrived-second"
            ]
        );
        assert_eq!(
            app.server_search_index, 4,
            "the cursor follows its hit, arrived-second"
        );
        assert_eq!(
            app.server_search_status.as_deref(),
            Some("5 results, best match first, then the server's newest first")
        );
    }

    /// A daemon that predates PERSO-273 numbers no hit and echoes no order
    /// and no `hit_order`: the hits are appended and stay in the order they
    /// arrived, the cursor stays on the hit it was on rather than jumping to
    /// the first server hit (every one carries the same `seq`), and the footer
    /// reads them as relevance, as `mp search` does, rather than naming the
    /// order the overlay asked for.
    #[test]
    fn an_older_daemons_settle_keeps_the_arrival_order_and_the_cursor() {
        use mp_protocol::listing::SearchOrder;
        let mut app = searching(
            SearchOrder::Oldest,
            vec![local_row("local", "2026-05-01T09:00:00")],
            Some(SearchOrder::Relevance),
        );
        for (subject, date) in [
            ("one", "2026-07-02T08:00:00"),
            ("two", "2026-01-01T09:00:00"),
            ("three", "2026-03-01T09:00:00"),
        ] {
            apply_server_hit(&mut app, &hit_event(None, subject, date));
        }
        app.server_search_index = 2;
        settle(
            &mut app,
            serde_json::json!({"account": "tum", "query": "x", "hits": 3}),
        );
        assert_eq!(subjects(&app), ["local", "one", "two", "three"]);
        assert_eq!(app.server_search_index, 2, "the cursor stays on two");
        assert_eq!(
            app.server_search_status.as_deref(),
            Some("4 results, best match first")
        );

        // With no local hit either, nothing is merged or moved.
        let mut app = searching(SearchOrder::Oldest, Vec::new(), None);
        for (subject, date) in [("one", "2026-07-02T08:00:00"), ("two", "")] {
            apply_server_hit(&mut app, &hit_event(None, subject, date));
        }
        app.server_search_index = 1;
        settle(
            &mut app,
            serde_json::json!({"account": "tum", "query": "x", "hits": 2}),
        );
        assert_eq!(subjects(&app), ["one", "two"]);
        assert_eq!(app.server_search_index, 1);
    }

    #[test]
    fn a_rollback_suffix_downgrades_the_sync_status_to_warning() {
        assert!(matches!(
            drained_sync_level(
                "Synced 3 mailboxes; 2 mutation(s) failed and were rolled back (see the log)"
            ),
            StatusLevel::Warning
        ));
        assert!(matches!(
            drained_sync_level("Sync complete"),
            StatusLevel::Success
        ));
    }

    /// #0115: the same rule for the other suffix a green line must not carry.
    #[test]
    fn a_non_converging_suffix_downgrades_the_sync_status_to_warning() {
        assert!(matches!(
            drained_sync_level(
                "Synced: 16 new, 84 existing, fetch not converging on Sent Items (see the log)"
            ),
            StatusLevel::Warning
        ));
        assert!(matches!(
            drained_sync_level("Synced: 16 new, 84 existing"),
            StatusLevel::Success
        ));
    }

    /// #0122: a tick refused the engine lock is neither a success nor a
    /// failure, so it is shown as information.
    #[test]
    fn a_refused_engine_lock_reports_the_skipped_sync_as_information() {
        assert!(matches!(
            drained_sync_level(
                "Sync skipped: another engine is syncing 'work'; leaving the ingest to it"
            ),
            StatusLevel::Info
        ));
    }

    /// #0122 review: the tail drain runs whether or not the ingest was
    /// refused, so a skipped sync can still carry the rollback suffix. The
    /// mutations it rolled back are what the line is about, and Info would
    /// hide them.
    #[test]
    fn a_skipped_sync_that_rolled_mutations_back_is_still_a_warning() {
        assert!(matches!(
            drained_sync_level(
                "Sync skipped: another engine is syncing 'work'; leaving the ingest to it; \
                 2 mutation(s) failed and were rolled back (see the log)"
            ),
            StatusLevel::Warning
        ));
        assert!(matches!(
            drained_sync_level(
                "Sync skipped: another engine is syncing 'work'; leaving the ingest to it; \
                 fetch not converging on Sent Items (see the log)"
            ),
            StatusLevel::Warning
        ));
    }

    // -----------------------------------------------------------------------
    // mailbox_loaded_is_current (P1 step 2: background mailbox loads)
    // -----------------------------------------------------------------------

    #[test]
    fn mailbox_loaded_applies_when_everything_matches() {
        assert!(mailbox_loaded_is_current(0, 2, 7, 0, 2, 7));
    }

    /// User switched account while the walk was in flight.
    #[test]
    fn mailbox_loaded_dropped_on_account_switch() {
        assert!(!mailbox_loaded_is_current(1, 2, 7, 0, 2, 7));
    }

    /// User switched to another mailbox (e.g. a cached one, which does not
    /// bump the generation) while the walk was in flight.
    #[test]
    fn mailbox_loaded_dropped_on_mailbox_switch() {
        assert!(!mailbox_loaded_is_current(0, 3, 7, 0, 2, 7));
    }

    /// A newer reload (or an optimistic archive/delete mutation) bumped
    /// the generation; the older walk must not clobber it / resurrect
    /// removed entries.
    #[test]
    fn mailbox_loaded_dropped_on_stale_generation() {
        assert!(!mailbox_loaded_is_current(0, 2, 8, 0, 2, 7));
    }

    /// Out-of-order delivery: a walk stamped with a *newer* generation
    /// than the app's counter can only happen through wraparound or a
    /// bug -- treated as stale either way.
    #[test]
    fn mailbox_loaded_dropped_on_future_generation() {
        assert!(!mailbox_loaded_is_current(0, 2, 7, 0, 2, 8));
    }

    // -----------------------------------------------------------------------
    // refresh_after_server_sync (#0038 follow-up: a refresh that refreshes)
    // -----------------------------------------------------------------------

    /// Point the data directory at a temp dir so `recount_all_mailboxes`
    /// resolves its store inside the fixture. Thread-local, so no other test
    /// can observe it (#0077).
    struct DataDir {
        _dir: mp_core::config::test_env::TestDataDir,
    }

    impl DataDir {
        fn new() -> Self {
            Self {
                _dir: mp_core::config::test_env::TestDataDir::new(),
            }
        }
    }

    /// An app parked on mailbox 1 of a two-mailbox account, both caches warm.
    fn app_with_warm_caches() -> App {
        let mut app = App::default_for_tests();
        app.account_config.name = "alice".to_string();
        app.mailboxes = vec![
            crate::app::MailboxInfo {
                label: "Inbox".into(),
                icon: "",
                id: "inbox".into(),
                kind: MailboxKind::Inbox,
                server_name: None,
            },
            crate::app::MailboxInfo {
                label: "Archive".into(),
                icon: "",
                id: "archive".into(),
                kind: MailboxKind::Archive,
                server_name: None,
            },
        ];
        app.mailbox_counts = vec![3, 4];
        app.email_cache = vec![
            Some(std::sync::Arc::new(Vec::new())),
            Some(std::sync::Arc::new(Vec::new())),
        ];
        app.active_mailbox = 1;
        app
    }

    /// A minimal `AccountState` with one warm mailbox cache, for the account
    /// that is not on screen, built as a struct literal.
    fn background_account(name: &str) -> crate::app::AccountState {
        crate::app::AccountState {
            account_config: mp_core::config::AccountConfig {
                name: name.to_string(),
                ..Default::default()
            },
            credentials: None,
            archive_server_name: "Archive".to_string(),
            drafts_dir: None,
            mailboxes: Vec::new(),
            mailbox_counts: vec![7],
            email_cache: vec![Some(std::sync::Arc::new(Vec::new()))],
            sidebar_index: 0,
            active_mailbox: 0,
            list_index: 0,
            cursor_ref: None,
            headers_scroll: 0,
            preview_scroll: 0,
            selection: std::collections::HashSet::new(),
            search_query: String::new(),
            opening: false,
            blocked: false,
            has_unseen: false,
            sync_health: mp_core::sync_health::SyncHealth::default(),
        }
    }

    /// A completed sync rewrote rows in every target mailbox, so every cache
    /// of that account goes, the open mailbox is queued for a background
    /// reload, and the sidebar counts are recomputed. Before this the handler
    /// only set a status line, so a message archived in another client stayed
    /// in the local inbox until a mailbox switch.
    #[test]
    fn a_finished_sync_drops_every_cache_and_reloads_the_open_mailbox() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();

        refresh_after_server_sync(&mut app, 0);

        assert!(
            app.email_cache.iter().all(|slot| slot.is_none()),
            "a sync writes to every target mailbox, so every cache is stale"
        );
        let queued: Vec<usize> = app
            .pending_actions
            .iter()
            .filter_map(|a| match a {
                crate::app::Action::LoadMailbox { mailbox_idx, .. } => Some(*mailbox_idx),
                _ => None,
            })
            .collect();
        assert_eq!(
            queued,
            vec![1],
            "the open mailbox reloads off the UI thread, like a mailbox switch"
        );
        assert_eq!(
            app.mailbox_counts,
            vec![0, 0],
            "the sidebar counts come back from the store, not from the stale cache"
        );
    }

    /// A sync on a background account has no list on screen and no counts to
    /// redraw: it drops that account's caches and leaves the active account
    /// alone. Account 1 carries a warm cache of its own, so the drop is
    /// observable rather than a no-op on an empty `accounts` vector.
    #[test]
    fn a_finished_sync_on_another_account_leaves_the_open_list_alone() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("alice"), background_account("bob")];

        refresh_after_server_sync(&mut app, 1);

        assert!(
            app.accounts[1]
                .email_cache
                .iter()
                .all(|slot| slot.is_none()),
            "the synced account's rows changed under its cache"
        );
        assert_eq!(
            app.accounts[1].mailbox_counts,
            vec![7],
            "an off-screen account keeps its counts until it is switched to"
        );
        assert!(
            app.accounts[0]
                .email_cache
                .iter()
                .all(|slot| slot.is_some()),
            "the other background account is untouched"
        );
        assert!(
            app.email_cache.iter().all(|slot| slot.is_some()),
            "the active account's caches are not stale"
        );
        assert!(app.pending_actions.is_empty(), "nothing to reload");
        assert_eq!(app.mailbox_counts, vec![3, 4]);
    }

    // -----------------------------------------------------------------------
    // Per-account sync health (#0071, the race #0068 lost)
    // -----------------------------------------------------------------------

    /// Two accounts, one broken. `perso` fails at login in milliseconds,
    /// `tum` finishes fifteen seconds later and overwrites the status line
    /// with a success. That is the exact sequence that hid a seven-week
    /// outage: the assertion is that `perso` is still marked failed
    /// afterwards, with its reason intact, while `tum` reads healthy.
    #[test]
    fn a_failed_account_stays_failed_while_another_account_syncs_cleanly() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("perso"), background_account("tum")];
        app.active_account = 0;

        handle_bg_result(
            &mut app,
            BgResult::Fetch {
                account_index: 0,
                result: Err("IMAP login failed: no such user".to_string()),
                rolled_back: 0,
            },
        );
        handle_bg_result(
            &mut app,
            BgResult::Fetch {
                account_index: 1,
                result: Ok("Synced: 8 new, 0 existing".to_string()),
                rolled_back: 0,
            },
        );

        assert!(
            app.accounts[0].sync_health.is_failed(),
            "the broken account keeps its mark after another account succeeds"
        );
        assert_eq!(
            app.accounts[0].sync_health.failure_lines().unwrap().1,
            "IMAP login failed: no such user"
        );
        assert!(!app.accounts[1].sync_health.is_failed());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Synced: 8 new, 0 existing"),
            "the status line still shows the last writer, which is why the \
             health lives on the account instead"
        );
    }

    /// The mark is cleared by that same account syncing cleanly, not by the
    /// next status line, and a full sync (`BgResult::Sync`) clears a quick
    /// sync's failure: both paths write the same per-account state.
    #[test]
    fn an_accounts_own_success_clears_its_mark() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("perso")];
        app.active_account = 0;

        handle_bg_result(
            &mut app,
            BgResult::Fetch {
                account_index: 0,
                result: Err("IMAP login failed".to_string()),
                rolled_back: 0,
            },
        );
        assert!(app.accounts[0].sync_health.is_failed());

        handle_bg_result(
            &mut app,
            BgResult::Sync {
                account_index: 0,
                result: Ok("Synced: 3 new, 0 existing".to_string()),
                rolled_back: 0,
            },
        );
        assert!(!app.accounts[0].sync_health.is_failed());
        assert_eq!(app.accounts[0].sync_health.failure_lines(), None);
    }

    /// Repeated failures of the same account count up rather than resetting,
    /// so an outage reads differently from a hiccup.
    #[test]
    fn repeated_failures_of_one_account_accumulate_across_results() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("perso")];
        app.active_account = 0;

        for _ in 0..3 {
            handle_bg_result(
                &mut app,
                BgResult::Fetch {
                    account_index: 0,
                    result: Err("IMAP login failed".to_string()),
                    rolled_back: 0,
                },
            );
        }

        assert!(app.accounts[0]
            .sync_health
            .failure_lines()
            .unwrap()
            .0
            .contains("x3"));
    }

    // -----------------------------------------------------------------------
    // Two-phase startup: BgResult::AccountOpened (#0003)
    // -----------------------------------------------------------------------

    /// A background account's store finishes opening: its real counts land,
    /// its outbox badge refreshes and the loading marker clears, while the
    /// active account and its on-screen list are left untouched. A local-only
    /// account (the daemon reports no IMAP credential and it is not Graph)
    /// queues no auto-fetch.
    #[test]
    fn account_opened_fills_a_background_account_and_clears_its_loading_marker() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("alice"), background_account("bob")];
        app.active_account = 0;
        app.accounts[0].opening = true;
        app.accounts[1].opening = true;

        handle_bg_result(
            &mut app,
            BgResult::AccountOpened {
                account_index: 1,
                counts: vec![42],
                credentials: Some(crate::app::Credentials::default()),
            },
        );

        assert!(
            !app.accounts[1].opening,
            "the opened account drops its loading marker"
        );
        assert_eq!(app.accounts[1].mailbox_counts, vec![42]);
        assert!(
            app.accounts[0].opening,
            "opening one account does not touch the others"
        );
        assert_eq!(
            app.mailbox_counts,
            vec![3, 4],
            "a background open never rewrites the active account's live counts"
        );
        assert!(
            app.pending_actions
                .iter()
                .all(|a| !matches!(a, crate::app::Action::FetchAccount(_))),
            "a local-only account has no remote source to auto-fetch"
        );
    }

    /// The active account's store finishing opening adopts its counts into the
    /// live view and queues the open mailbox's load off the UI thread (the
    /// first paint deliberately left it empty). A remote account also queues
    /// its startup auto-fetch, but only now that the store is validated.
    #[test]
    fn account_opened_for_the_active_account_loads_its_mailbox_and_auto_fetches() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("alice")];
        app.active_account = 0;
        app.accounts[0].opening = true;

        // The daemon reports an IMAP credential, so the auto-fetch fires.
        let credentials = crate::app::Credentials {
            imap: true,
            smtp: true,
            calendar_scope_missing: false,
            rejected_login: None,
        };
        handle_bg_result(
            &mut app,
            BgResult::AccountOpened {
                account_index: 0,
                counts: vec![5, 9],
                credentials: Some(credentials),
            },
        );

        assert!(!app.accounts[0].opening);
        assert_eq!(
            app.accounts[0].credentials,
            Some(credentials),
            "the daemon's credential answer lands on the account"
        );
        assert_eq!(
            app.mailbox_counts,
            vec![5, 9],
            "the live view adopts the opened active account's counts"
        );
        let loads: Vec<usize> = app
            .pending_actions
            .iter()
            .filter_map(|a| match a {
                crate::app::Action::LoadMailbox { mailbox_idx, .. } => Some(*mailbox_idx),
                _ => None,
            })
            .collect();
        assert_eq!(
            loads,
            vec![1],
            "the active account's open mailbox loads once the store is validated"
        );
        assert!(
            app.pending_actions
                .iter()
                .any(|a| matches!(a, crate::app::Action::FetchAccount(0))),
            "a remote active account kicks its startup auto-fetch after the open"
        );
    }

    // -----------------------------------------------------------------------
    // Row deltas over the open list (P5-U4, the seam P5-U8 turns on)
    // -----------------------------------------------------------------------

    /// An invalidate over the open mailbox's listing owes a reload, which goes
    /// through the same off-thread path a mailbox switch takes.
    #[test]
    fn an_invalidate_over_the_open_mailbox_reloads_it_off_the_ui_thread() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();

        let delta = super::super::queries::MessageRowDelta::decode(&mp_protocol::EventEnvelope {
            instance_id: "bg".to_string(),
            revision: 4,
            kind: "state.invalidate".to_string(),
            payload: serde_json::json!({
                "resource": "mailbox:alice/archive",
                "scope": {"query": "list"},
            }),
        })
        .expect("a state.invalidate over a listing decodes");

        assert!(
            !apply_row_delta(&mut app, &delta),
            "an invalidate owes a reload"
        );
        assert!(
            app.pending_actions
                .iter()
                .any(|a| matches!(a, crate::app::Action::LoadMailbox { mailbox_idx: 1, .. })),
            "the reload is queued as the background load, not run here"
        );
    }

    /// A door that answers every `message.list` with one fixed listing.
    struct Listing(serde_json::Value);

    impl super::super::queries::Queries for Listing {
        fn call(
            &self,
            _method: &str,
            _params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            Ok(self.0.clone())
        }
    }

    /// A remove of a listed row drops it from the open list and from the
    /// mailbox's cache slot alike, so a later visit to the mailbox does not
    /// bring the removed row back from the cache. The account name is this
    /// test's own, because the uid index is process-wide.
    #[test]
    fn a_remove_drops_the_row_from_the_open_list_and_its_cache_slot() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        let account = "bg-remove-delta";
        app.account_config.name = account.to_string();

        // The listing seeds the uid index, as a mailbox load does.
        let row = |id: i64, uid: i64| serde_json::json!({"id": id, "uid": uid, "subject": format!("row {id}")});
        let door = Listing(serde_json::json!({
            "account": account,
            "mailbox": "archive",
            "messages": [row(11, 101), row(12, 102)],
        }));
        let rows = std::sync::Arc::new(
            super::super::queries::list_emails(&door, account, "archive").expect("a listing"),
        );
        app.email_cache[1] = Some(std::sync::Arc::clone(&rows));
        app.emails = rows;

        let delta = super::super::queries::MessageRowDelta::decode(&mp_protocol::EventEnvelope {
            instance_id: "bg".to_string(),
            revision: 5,
            kind: mp_protocol::events::KIND_REMOVE.to_string(),
            payload: serde_json::json!({"resource": format!("message:{account}/archive/101")}),
        })
        .expect("a state.remove of a message decodes");

        assert!(
            apply_row_delta(&mut app, &delta),
            "a listed uid folds in place"
        );
        let left = |rows: &[crate::app::EmailEntry]| -> Vec<Option<MessageRef>> {
            rows.iter().map(|row| row.msg).collect()
        };
        assert_eq!(left(&app.emails), [Some(MessageRef::new(12))]);
        assert_eq!(
            left(app.email_cache[1].as_deref().expect("the slot stays warm")),
            [Some(MessageRef::new(12))],
            "the cache slot holds the post-delta rows"
        );
        assert!(app.pending_actions.is_empty(), "nothing to reload");
    }

    /// The delta a `state.remove` of `resource` decodes to.
    fn removal(resource: &str) -> super::super::queries::MessageRowDelta {
        super::super::queries::MessageRowDelta::decode(&mp_protocol::EventEnvelope {
            instance_id: "bg".to_string(),
            revision: 6,
            kind: mp_protocol::events::KIND_REMOVE.to_string(),
            payload: serde_json::json!({"resource": resource}),
        })
        .expect("a state.remove of a message decodes")
    }

    /// A removal from a mailbox that is not on screen drops that mailbox's
    /// cache slot, so its next visit reads it again rather than showing the
    /// row another client moved away; the open list is left alone
    /// (PERSO-286).
    #[test]
    fn a_remove_from_a_mailbox_not_on_screen_drops_its_cache_slot() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        let open = std::sync::Arc::clone(&app.emails);

        assert!(apply_row_delta(&mut app, &removal("message:alice/inbox/7")));

        assert!(app.email_cache[0].is_none(), "the Inbox listing is stale");
        assert!(app.email_cache[1].is_some(), "the open Archive is not");
        assert!(std::sync::Arc::ptr_eq(&open, &app.emails));
        assert!(app.pending_actions.is_empty(), "nothing to reload");
    }

    /// A delta of another account drops that account's cached listing of the
    /// mailbox and touches nothing on screen, even where the active account
    /// has a mailbox of the same name open (PERSO-286).
    #[test]
    fn a_delta_of_another_account_drops_its_slot_and_leaves_the_open_list() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        let mut bob = background_account("bob");
        bob.mailboxes = vec![app.mailboxes[1].clone()];
        app.accounts = vec![background_account("alice"), bob];
        app.active_account = 0;

        assert!(apply_row_delta(&mut app, &removal("message:bob/archive/7")));

        assert!(app.accounts[1].email_cache[0].is_none(), "bob's Archive");
        assert!(app.email_cache[1].is_some(), "alice's open Archive");
        assert!(app.pending_actions.is_empty(), "nothing to reload");
    }

    /// A removal drops the local search hit it names by its mailbox and uid,
    /// as the overlay's own `a` drops a hit it archived, and the cursor stays
    /// on the hit it was on; a hit of another mailbox under the same uid and
    /// a server hit stay (PERSO-286).
    #[test]
    fn a_remove_drops_the_search_hit_it_names() {
        let _data = DataDir::new();
        let mut app = app_with_warm_caches();
        let hit = |subject: &str, mailbox: &str, uid: Option<i64>| crate::app::SearchResultEntry {
            source_label: mailbox.into(),
            uid,
            ..local_row(subject, "2024-01-01T12:00:00Z")
        };
        app.server_search_results = vec![
            hit("moved", "inbox", Some(7)),
            hit("kept", "inbox", Some(8)),
            hit("elsewhere", "archive", Some(7)),
            hit("server", "INBOX", None),
        ];
        app.server_search_index = 2;

        apply_row_delta(&mut app, &removal("message:alice/inbox/7"));
        apply_row_delta(&mut app, &removal("message:bob/inbox/8"));

        let subjects: Vec<&str> = app
            .server_search_results
            .iter()
            .map(|hit| hit.fetched.subject.as_str())
            .collect();
        assert_eq!(subjects, ["kept", "elsewhere", "server"]);
        assert_eq!(app.server_search_index, 1, "still on the same hit");
    }

    /// The overlay showing `query`'s results over the active account `alice`.
    fn app_showing_a_search(query: &str) -> App {
        let mut app = app_with_warm_caches();
        app.accounts = vec![background_account("alice"), background_account("bob")];
        app.active_account = 0;
        app.overlay = crate::app::Overlay::Search;
        app.server_search_last = Some(crate::app::LastSearch {
            query: mp_core::search::parse(query).expect("the query parses"),
            targets: None,
            local_mailbox: Some("inbox".into()),
        });
        app
    }

    /// The searches a test's app queued, as the query strings they run.
    fn queued_searches(app: &App) -> Vec<(String, Option<String>)> {
        app.pending_actions
            .iter()
            .filter_map(|a| match a {
                crate::app::Action::ServerSearch {
                    query,
                    local_mailbox,
                    ..
                } => Some((
                    mp_core::search::to_query_string(query),
                    local_mailbox.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    /// A rollback of the account the overlay searched runs its search again
    /// (PERSO-286): the `state.remove` the move published dropped a hit, and
    /// the rolled-back move put the message back where the hit listed it.
    #[test]
    fn a_rollback_runs_the_open_search_again() {
        let _data = DataDir::new();
        let mut app = app_showing_a_search("from:carol");
        let generation = app.server_search_generation;

        land_rollback(&mut app, 0, 1);

        assert_eq!(
            queued_searches(&app),
            [("from:\"carol\"".to_string(), Some("inbox".to_string()))]
        );
        assert_ne!(app.server_search_generation, generation, "a new search");
    }

    /// A rollback of another account, or with the overlay closed, runs no
    /// search: the overlay's hits are the active account's, and a closed
    /// overlay opens on a blank form.
    #[test]
    fn a_rollback_elsewhere_runs_no_search() {
        let _data = DataDir::new();
        let mut app = app_showing_a_search("from:carol");
        land_rollback(&mut app, 1, 1);
        assert!(queued_searches(&app).is_empty(), "bob's rollback");

        let mut app = app_showing_a_search("from:carol");
        app.overlay = crate::app::Overlay::None;
        land_rollback(&mut app, 0, 1);
        assert!(queued_searches(&app).is_empty(), "the overlay is closed");
    }

    /// A tick of `account` whose mutation drains rolled `failed` back, as
    /// its `sync.completed` carries it.
    fn tick(account: &str, failed: u64) -> mp_protocol::events::SyncCompleted {
        mp_protocol::events::SyncCompleted {
            account: account.to_string(),
            severity: if failed > 0 {
                mp_protocol::events::Severity::Warning
            } else {
                mp_protocol::events::Severity::Ok
            },
            saved: 0,
            skipped: 0,
            flags_updated: 0,
            pruned: 0,
            prunes_deferred: 0,
            uid_rebound: 0,
            uidvalidity_resets: 0,
            bodies_truncated: 0,
            non_converging: Vec::new(),
            failed_mutations: failed,
            error: None,
            new_inbox_mail: Vec::new(),
        }
    }

    /// The ways a tick of `alice` that rolled `failed` mutations back reaches
    /// the overlay of [`app_showing_a_search`]: the daemon's own tick as its
    /// `sync.completed` event, and a quick and a full pass this client asked
    /// for as their `operation.finished`. Each returns the searches it queued.
    fn searches_after_a_tick(failed: u64) -> Vec<Vec<(String, Option<String>)>> {
        let outcome = tick("alice", failed);
        let mut runs = Vec::new();

        let mut app = app_showing_a_search("from:carol");
        app.events.watermark("instance", 10);
        app.apply_event(&mp_protocol::EventEnvelope {
            instance_id: "instance".to_string(),
            revision: 11,
            kind: mp_protocol::events::KIND_SYNC_COMPLETED.to_string(),
            payload: serde_json::to_value(&outcome).expect("a tick serialises"),
        });
        runs.push(queued_searches(&app));

        let finished = serde_json::json!({
            "state": "succeeded",
            "result": {"outcome": outcome, "new_inbox_mail": []},
        });
        for awaited in [
            crate::events::Awaited::Quick {
                account_index: 0,
                account: "alice".to_string(),
            },
            crate::events::Awaited::Full {
                account_index: 0,
                account: "alice".to_string(),
            },
        ] {
            let mut app = app_showing_a_search("from:carol");
            handle_bg_result(&mut app, crate::commands::settled(&awaited, &finished));
            runs.push(queued_searches(&app));
        }
        runs
    }

    /// A tick whose drains rolled a mutation back runs the open search again
    /// as a `mutations.rolled_back` does (PERSO-286), whichever way the tick
    /// reaches the TUI: the rollback put back the message whose hit the
    /// move's `state.remove` dropped.
    #[test]
    fn a_tick_that_rolled_a_mutation_back_runs_the_open_search_again() {
        let _data = DataDir::new();
        let carol = vec![("from:\"carol\"".to_string(), Some("inbox".to_string()))];
        assert_eq!(
            searches_after_a_tick(1),
            [carol.clone(), carol.clone(), carol],
            "the daemon's tick, a quick pass and a full pass"
        );
    }

    /// The daemon the overlay's search runs against: the local pass lists one
    /// hit, and the server leg starts operation `op-1`.
    struct SearchDoor;

    impl crate::queries::Queries for SearchDoor {
        fn call(&self, method: &str, _: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            match method {
                "message.search" => {
                    let mut hit =
                        serde_json::to_value(mp_protocol::listing::MessageListRow::default())?;
                    hit["mailbox"] = serde_json::json!("inbox");
                    Ok(serde_json::json!({"hits": [hit], "order": "newest"}))
                }
                "message.search_server" => Ok(serde_json::json!({"operation_id": "op-1"})),
                other => Err(anyhow::anyhow!("unexpected {other}")),
            }
        }
    }

    /// A rollback's run of the open search again (PERSO-286) leaves the user
    /// where they are typing: the local pass, the server's streamed hit and
    /// its settle land in the list without taking the focus from the form
    /// field, and the text typed there stays.
    #[test]
    fn a_rollbacks_rerun_leaves_the_focus_and_the_typed_text_in_the_form() {
        use crate::app::{SearchField, SearchOverlayFocus};
        use crossterm::event::{KeyCode, KeyEvent};
        let _data = DataDir::new();
        let mut app = app_showing_a_search("from:carol");
        let typing = SearchOverlayFocus::Field(SearchField::Keywords);
        app.server_search_focus = typing;
        for c in "ledg".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let where_typing = |app: &App, after: &str| {
            assert_eq!(app.server_search_focus, typing, "the focus after {after}");
            assert_eq!(app.search_form.keywords, "ledg", "the text after {after}");
        };

        land_rollback(&mut app, 0, 1);
        let Some(crate::app::Action::ServerSearch {
            query,
            targets,
            local_mailbox,
        }) = std::mem::take(&mut app.pending_actions)
            .into_iter()
            .find(|action| matches!(action, crate::app::Action::ServerSearch { .. }))
        else {
            panic!("the rollback queues the search");
        };
        crate::commands::start_search(
            &mut app,
            &SearchDoor,
            &query,
            targets.as_deref(),
            local_mailbox.as_deref(),
        );
        assert_eq!(app.server_search_results.len(), 1, "the local pass landed");
        where_typing(&app, "the local pass");

        apply_server_hit(&mut app, &hit_event(Some(0), "server", ""));
        assert_eq!(app.server_search_results.len(), 2, "the server hit landed");
        where_typing(&app, "a streamed hit");

        let generation = app.server_search_generation;
        handle_bg_result(
            &mut app,
            BgResult::ServerSearch {
                generation,
                result: Ok(serde_json::json!({"hits": 1, "unreachable": []})),
            },
        );
        assert!(!app.server_search_loading, "the search settled");
        where_typing(&app, "the settle");
    }

    /// A rollback that lands in the pass of the main loop where the user
    /// submitted a new search, before that search started (PERSO-286), queues
    /// no run of the older one: the submitted search runs alone, after the
    /// rollback, so its hits already hold what the rollback put back, it moves
    /// the focus into its results as a submitted search does, and it is the
    /// one the overlay shows. Whether the rollback arrives as a
    /// `mutations.rolled_back`, as the daemon's tick or as a pass this client
    /// awaited.
    #[test]
    fn a_rollback_never_queues_the_older_search_after_a_submitted_one() {
        use crate::app::{SearchField, SearchOverlayFocus};
        use crossterm::event::{KeyCode, KeyEvent};
        let _data = DataDir::new();

        let submitted = || {
            let mut app = app_showing_a_search("from:carol");
            app.search_form.scope.0 = crate::app::SearchScope::CurrentAccount;
            app.server_search_focus = SearchOverlayFocus::Field(SearchField::Keywords);
            for c in "ledger".chars() {
                app.handle_key(KeyEvent::from(KeyCode::Char(c)));
            }
            app.handle_key(KeyEvent::from(KeyCode::Enter));
            let queued = queued_searches(&app);
            assert_eq!(queued.len(), 1, "the submit queues its search");
            assert!(queued[0].0.contains("ledger"), "{queued:?}");
            app.events.watermark("instance", 10);
            (app, queued)
        };
        let event = |kind: &str, payload: serde_json::Value| mp_protocol::EventEnvelope {
            instance_id: "instance".to_string(),
            revision: 11,
            kind: kind.to_string(),
            payload,
        };
        let finished = serde_json::json!({
            "state": "succeeded",
            "result": {"outcome": tick("alice", 1), "new_inbox_mail": []},
        });
        let land = |app: &mut App, trigger: &str| match trigger {
            "mutations.rolled_back" => {
                app.apply_event(&event(
                    mp_protocol::events::KIND_MUTATIONS_ROLLED_BACK,
                    serde_json::json!({"account": "alice", "failed": 1}),
                ));
            }
            "the daemon's tick" => {
                app.apply_event(&event(
                    mp_protocol::events::KIND_SYNC_COMPLETED,
                    serde_json::to_value(tick("alice", 1)).expect("a tick serialises"),
                ));
            }
            "an awaited pass" => {
                let awaited = crate::events::Awaited::Quick {
                    account_index: 0,
                    account: "alice".to_string(),
                };
                handle_bg_result(app, crate::commands::settled(&awaited, &finished));
            }
            other => unreachable!("no trigger {other}"),
        };

        for trigger in [
            "mutations.rolled_back",
            "the daemon's tick",
            "an awaited pass",
        ] {
            let (mut app, queued) = submitted();
            land(&mut app, trigger);
            assert_eq!(queued_searches(&app), queued, "after {trigger}");

            let Some(crate::app::Action::ServerSearch {
                query,
                targets,
                local_mailbox,
            }) = std::mem::take(&mut app.pending_actions)
                .into_iter()
                .find(|action| matches!(action, crate::app::Action::ServerSearch { .. }))
            else {
                panic!("the submitted search is queued after {trigger}");
            };
            crate::commands::start_search(
                &mut app,
                &SearchDoor,
                &query,
                targets.as_deref(),
                local_mailbox.as_deref(),
            );
            assert_eq!(
                app.server_search_focus,
                SearchOverlayFocus::List,
                "the submitted search takes the focus after {trigger}"
            );
            let shown = app.server_search_last.as_ref().expect("a search ran");
            assert_eq!(
                mp_core::search::to_query_string(&shown.query),
                queued[0].0,
                "the overlay shows the submitted search after {trigger}"
            );
        }
    }

    /// A tick that rolled nothing back runs no search: nothing it did put a
    /// dropped hit back, and a search per tick would hit the server each
    /// time.
    #[test]
    fn a_tick_that_rolled_nothing_back_runs_no_search() {
        let _data = DataDir::new();
        assert_eq!(
            searches_after_a_tick(0),
            [Vec::new(), Vec::new(), Vec::new()]
        );
    }
}
