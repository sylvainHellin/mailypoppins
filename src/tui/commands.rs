//! The action-to-command layer (P5-U6, #0124): which daemon method every
//! [`Action`] issues, and the fifteen the dispatcher below owns outright.
//!
//! The contract is P5-U5's (`src/tui/actions_tests.rs`): [`ActionRoute`],
//! [`route`] and [`dispatch`], and nothing else. [`route`] is the classification
//! the plan asks for - *"every `Action` variant maps to a daemon method or is
//! documented as client-only (editor, browser, clipboard, file picker, terminal
//! suspend)"* - made a function with no wildcard arm, so a new variant fails to
//! compile until it is classified.
//!
//! # What `dispatch` owns and what it hands back
//!
//! `true` means the action was daemon-routed here and `handle_action` owes it
//! nothing. `false` means "not mine": a client-only or local action, or one of
//! the operation-kind actions (`sync.*`, `send.*`, `calendar.rsvp`,
//! `message.search`, `message.list_server`) that still owns a background thread
//! and a [`BgResult`](crate::tui::app::BgResult) channel in `handle_action`,
//! because an operation answers `{operation_id}` at once and finishes later.
//!
//! A refusal from the daemon is not a `false`. It lands on the status line
//! exactly as a refused store mutation did, which is why nothing here returns a
//! `Result` the caller would have to present a second way.
//!
//! # Why the door is an argument
//!
//! [`Queries`] is the object-safe wrapper over `Session::call` that P5-U3
//! contracted and P5-U4 built, for both a `Session` and a `QueryHandle`; a
//! `Commands` trait with the identical method would be a second name for one
//! thing. It is an argument rather than read off `app.session` because
//! `dispatch` needs `&mut App` and a borrow of the session inside it would
//! collide, which is what `Session::handle()` exists for.
//!
//! # A TUI mutation queues, it does not settle
//!
//! Every call below sends `settle: false`. `mp archive` resolves the backend,
//! commits the row and drains the owed op before it answers, which is its
//! blocking UX; the TUI has never done either (#0039). It commits the row
//! change and the owed op in one transaction and lets the next sync tick drain
//! it, which is why `u` over a thousand-message selection costs no network and
//! why the mark-read of an explicit open cannot stall a frame. The parameter
//! defaults to `true` on the wire so the CLI does not move.
//!
//! # A batch is one call per message
//!
//! In the selection's order. Today a reference to a row that is gone is skipped
//! with a log line while the rest of the selection proceeds, which one call per
//! row gives for free and a plural address would have to re-invent as a
//! partial-failure shape.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::{json, Value};

use mp_protocol::events::SyncCompleted;

use super::app::{
    mailbox_key, Action, App, BgResult, MailboxKind, MessageRef, RsvpChoice, StatusLevel,
};
use super::events::Awaited;
use super::helpers::SYNC_SKIPPED_MARKER;
use super::queries::Queries;
use crate::notify::NewMailMeta;
use crate::selector::Selector;

/// Where an [`Action`] does its work.
#[derive(Debug, PartialEq, Eq)]
pub enum ActionRoute {
    /// The methods this action issues, in issue order. An action that issues
    /// one call per selected message names its method once.
    Daemon(&'static [&'static str]),
    /// The action's whole effect is in this process, for the reason given: an
    /// editor session, a browser launch, a clipboard write, a file the user
    /// picked, a path already materialised.
    ClientOnly(&'static str),
    /// Pure UI state: an overlay opens, a cursor moves, a wizard closes.
    Local,
}

/// What `action` costs the daemon.
///
/// **No wildcard arm, on purpose**: this is where a new [`Action`] variant
/// fails to build, so it has to be classified rather than inheriting whatever
/// the wildcard said. The table it agrees with is `ACTION_ROUTING` in
/// `src/tui/actions_tests.rs`, which is the reviewed document; this is the
/// function the rest of the TUI branches on.
pub fn route(action: &Action) -> ActionRoute {
    match action {
        // `$EDITOR`, over a read-only rendition of a received row (#0075) or
        // over the writable file of a drafts row (#0052). The mark-read it
        // performs on the way in is `MarkAsRead`'s method, queued and routed
        // as its own action.
        Action::EditCurrent => {
            ActionRoute::ClientOnly("$EDITOR, over a rendition or a draft file this process opens")
        }
        Action::Reply(_) => ActionRoute::Daemon(&["draft.reply"]),
        Action::Send => ActionRoute::Daemon(&["send.draft"]),
        Action::SendApproved => ActionRoute::Daemon(&["send.approved"]),
        Action::NewDraft => ActionRoute::Daemon(&["draft.create"]),
        Action::Approve | Action::BatchApprove(_) => ActionRoute::Daemon(&["draft.approve"]),
        Action::MarkDraft | Action::BatchMarkDraft(_) => ActionRoute::Daemon(&["draft.demote"]),
        Action::Archive => ActionRoute::Daemon(&["message.archive"]),
        // One key over two kinds of row: a received row is a store mutation, a
        // drafts row is a local file removal (#0073), and a parse-skipped
        // draft (#0080) is the same removal by path.
        Action::Delete => ActionRoute::Daemon(&["message.delete", "draft.discard"]),
        Action::BatchArchive(_) => ActionRoute::Daemon(&["message.archive"]),
        Action::BatchDelete(_) => ActionRoute::Daemon(&["message.delete"]),
        Action::BatchDeleteDrafts(_) => ActionRoute::Daemon(&["draft.discard"]),
        Action::MoveToMailbox { .. } => ActionRoute::Daemon(&["message.move"]),
        Action::ToggleRead | Action::MarkAsRead(_) | Action::BatchToggleRead(_) => {
            ActionRoute::Daemon(&["message.set_read"])
        }
        Action::ToggleFlag | Action::BatchToggleFlag(_) => {
            ActionRoute::Daemon(&["message.set_flag"])
        }
        Action::CopyMessageRef => ActionRoute::ClientOnly("the system clipboard"),
        Action::OpenLogFile => ActionRoute::ClientOnly("$EDITOR, over this process's own log file"),
        Action::OpenConfigFile => {
            ActionRoute::ClientOnly("$EDITOR, over the configuration file the client resolves")
        }
        // The materialisation is `message.materialise_attachment` and happens
        // when the key resolves the part list; the action carries the path it
        // produced, so what is left is the desktop's file opener.
        Action::OpenAttachment(_) => {
            ActionRoute::ClientOnly("the system file opener, over an already-materialised path")
        }
        Action::SaveAttachments { .. } => {
            ActionRoute::ClientOnly("a copy into a directory only this process can name (ANO-15)")
        }
        Action::Fetch | Action::FetchAccount(_) => ActionRoute::Daemon(&["sync.quick"]),
        Action::LoadMailbox { .. } => ActionRoute::Daemon(&["message.list", "draft.list"]),
        Action::Sync => ActionRoute::Daemon(&["sync.full"]),
        // The local pass first, then the server leg (LST-08).
        Action::ServerSearch { .. } => {
            ActionRoute::Daemon(&["message.search", "message.list_server"])
        }
        Action::SearchResultOpen => {
            ActionRoute::ClientOnly("$EDITOR, over a rendition this process opens")
        }
        Action::SearchResultJump => ActionRoute::Local,
        Action::SearchResultYankPath => ActionRoute::ClientOnly("the system clipboard"),
        // LST-09's `message.fetch` is not built and nothing else ingests a
        // server-only hit.
        Action::SearchResultFetch => ActionRoute::ClientOnly(
            "no method ingests a server-only hit; see TUI_ACTION_ENGINE_RESIDUE",
        ),
        Action::SearchResultReply(_) => ActionRoute::Daemon(&["draft.reply"]),
        Action::SearchResultForward => ActionRoute::Daemon(&["draft.forward"]),
        Action::SearchResultArchive => ActionRoute::Daemon(&["message.archive"]),
        Action::SearchResultOpenInBrowser => ActionRoute::Daemon(&["message.materialise_html"]),
        Action::OpenHtmlInBrowser(_) => {
            ActionRoute::ClientOnly("the system browser, over an already-materialised path")
        }
        Action::OpenComposeWizard(_) => ActionRoute::Local,
        Action::ComposeWizardSubmit => ActionRoute::Daemon(&["draft.create"]),
        Action::ComposeWizardCancel => ActionRoute::Local,
        Action::ComposeEditSignature => {
            ActionRoute::ClientOnly("$EDITOR, over a signature file or a temporary copy of one")
        }
        Action::Rsvp { .. } => ActionRoute::Daemon(&["calendar.rsvp"]),
        Action::ComposeToContact { .. } => ActionRoute::Local,
        Action::SendContactVcard { .. } => ActionRoute::Daemon(&["draft.create"]),
        Action::CopyContactEmail { .. } => ActionRoute::ClientOnly("the system clipboard"),
        Action::OpenEventSource { .. } => {
            ActionRoute::ClientOnly("$EDITOR, over the invite the agenda row came from")
        }
        Action::EditSignatureFile { .. } => {
            ActionRoute::ClientOnly("$EDITOR, over a signature file")
        }
        // ATT-03's `draft.attach` is not built; the append is a write to a file
        // the user already picked.
        Action::AttachFileToDraft { .. } => ActionRoute::ClientOnly(
            "a frontmatter append to a file the user picked; draft.attach is not built",
        ),
    }
}

/// Run `action` against the daemon, answering whether it was this layer's.
///
/// See the module header for what `true` and `false` mean and why a refusal is
/// neither.
pub fn dispatch(app: &mut App, commands: &dyn Queries, action: &Action) -> bool {
    match action {
        Action::Approve => {
            status_flip(app, commands, Flip::Approve);
            true
        }
        Action::BatchApprove(ids) => {
            status_flip_batch(app, commands, ids, Flip::Approve);
            true
        }
        Action::MarkDraft => {
            status_flip(app, commands, Flip::Demote);
            true
        }
        Action::BatchMarkDraft(ids) => {
            status_flip_batch(app, commands, ids, Flip::Demote);
            true
        }
        Action::Archive => {
            if let Some(msg) = app.selected_email_ref() {
                archive_msgs(app, commands, vec![msg], false);
            }
            true
        }
        Action::BatchArchive(msgs) => {
            archive_msgs(app, commands, msgs.clone(), true);
            true
        }
        Action::Delete => {
            // A Drafts row has no `messages` row, so `d` on it is the
            // local-only draft discard (#0073); a parse-skipped draft (#0080)
            // has no index row either and goes by its path.
            let skip_path = app
                .selected_email()
                .and_then(|e| e.skip.as_ref().map(|s| s.path.clone()));
            let draft_id = app.selected_email().and_then(|e| e.draft_id.clone());
            if let Some(path) = skip_path {
                delete_skip_file(app, &path);
            } else if let Some(id) = draft_id {
                delete_draft(app, commands, &id);
            } else if let Some(msg) = app.selected_email_ref() {
                delete_msgs(app, commands, vec![msg], false);
            }
            true
        }
        Action::BatchDelete(msgs) => {
            delete_msgs(app, commands, msgs.clone(), true);
            true
        }
        Action::BatchDeleteDrafts(ids) => {
            delete_drafts_batch(app, commands, ids);
            true
        }
        Action::MoveToMailbox { msgs, dest_idx } => {
            move_msgs(app, commands, msgs.clone(), *dest_idx);
            true
        }
        Action::ToggleRead => {
            if let Some(email) = app.selected_email() {
                let new_read = !email.read;
                if let Some(msg) = email.msg {
                    let label = if new_read {
                        "Marked as read"
                    } else {
                        "Marked as unread"
                    };
                    if set_read_flag(app, commands, &[msg], new_read) {
                        app.set_status(label.to_string());
                    }
                }
            }
            true
        }
        Action::MarkAsRead(msg) => {
            // The mark that rides on an explicit open (#0110): same path as the
            // manual `u` toggle, no status line of its own, addressing the row
            // the open resolved rather than the one the cursor ended on after a
            // coalesced batch (#0108).
            mark_open_read(app, commands, *msg);
            true
        }
        Action::BatchToggleRead(msgs) => {
            let any_unread = msgs
                .iter()
                .any(|m| app.emails.iter().any(|e| e.msg == Some(*m) && !e.read));
            let count = msgs.len();
            if set_read_flag(app, commands, msgs, any_unread) {
                app.selection.clear();
                app.set_status(if any_unread {
                    format!("Marked {count} as read")
                } else {
                    format!("Marked {count} as unread")
                });
            }
            true
        }
        Action::ToggleFlag => {
            if let Some(email) = app.selected_email() {
                let new_flag = !email.flagged;
                if let Some(msg) = email.msg {
                    let label = if new_flag { "Flagged" } else { "Unflagged" };
                    if set_flag(app, commands, &[msg], new_flag) {
                        app.set_status(label.to_string());
                    }
                }
            }
            true
        }
        // The four operation-kind actions (P5-U8). Each starts its operation
        // and returns; the finish arrives as an `operation.finished` event and
        // lands through [`settled`]. Until this unit each kept a worker thread
        // and a `BgResult` channel in `handle_action`, which is why they were
        // the four `dispatch` used to hand back.
        Action::Fetch => {
            start_pass(app, commands, "sync.quick", "Quick sync", Action::Fetch);
            true
        }
        Action::Sync => {
            start_pass(app, commands, "sync.full", "Full sync", Action::Sync);
            true
        }
        Action::FetchAccount(index) => {
            start_account_pass(app, commands, *index);
            true
        }
        Action::SendApproved => {
            send_approved(app, commands);
            true
        }
        Action::Rsvp { msg, choice } => {
            rsvp(app, commands, *msg, *choice);
            true
        }
        Action::BatchToggleFlag(msgs) => {
            let any_unflagged = msgs
                .iter()
                .any(|m| app.emails.iter().any(|e| e.msg == Some(*m) && !e.flagged));
            let count = msgs.len();
            if set_flag(app, commands, msgs, any_unflagged) {
                app.selection.clear();
                app.set_status(if any_unflagged {
                    format!("Flagged {count}")
                } else {
                    format!("Unflagged {count}")
                });
            }
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/// Start one operation and remember it until its `operation.finished` arrives
/// (P5-U8).
///
/// An operation answers `{operation_id}` at once and finishes later, so
/// something has to wait for it. Until this unit that was a worker thread per
/// action reading `operation.status` every 100 ms; now it is the event stream
/// the connection has been subscribed to since its first `state.bootstrap`, so
/// a minute-long sync costs one call instead of six hundred and the wait is a
/// map entry rather than a thread.
///
/// `false` means the operation never started and the caller has already been
/// told why; `bg_count` is bumped only on the `true` path, because it is the
/// finished event that brings it down again.
pub(super) fn start_operation(
    app: &mut App,
    commands: &dyn Queries,
    method: &str,
    params: Value,
    awaited: Awaited,
) -> bool {
    let started = match commands.call(method, params) {
        Ok(started) => started,
        Err(e) => {
            app.set_status_level(format!("{method}: {e:#}"), StatusLevel::Error);
            return false;
        }
    };
    let Some(id) = started["operation_id"].as_str().map(str::to_string) else {
        app.set_status_level(
            format!("{method} answered no operation id"),
            StatusLevel::Error,
        );
        return false;
    };
    app.bg_count += 1;
    app.events.started(id, awaited);
    true
}

/// One finished operation as the [`BgResult`] its arm used to post.
///
/// The event's payload carries `result` exclusive-or `error`
/// (`src/daemon/operations.rs`), and the four shapes below are the four the
/// arms already knew how to present, so nothing above this changed when the
/// poll went away.
pub(super) fn settled(awaited: &Awaited, payload: &Value) -> BgResult {
    let result = match payload["state"].as_str() {
        Some("succeeded") => Ok(payload["result"].clone()),
        _ => Err(operation_error(payload)),
    };
    match awaited {
        Awaited::Quick {
            account_index,
            account,
        } => match result {
            Ok(settled) => BgResult::Fetch {
                account_index: *account_index,
                result: Ok(sync_status_line(account, &settled)),
                new_inbox_mail: new_inbox_mail(&settled),
            },
            Err(e) => BgResult::Fetch {
                account_index: *account_index,
                result: Err(e),
                new_inbox_mail: Vec::new(),
            },
        },
        Awaited::Full {
            account_index,
            account,
        } => BgResult::Sync {
            account_index: *account_index,
            result: result.map(|settled| sync_status_line(account, &settled)),
        },
        Awaited::SendApproved { account_index } => BgResult::SendApproved {
            account_index: *account_index,
            result: result.map(|settled| approved_line(&settled)),
        },
        Awaited::Rsvp { account_index } => BgResult::Rsvp {
            account_index: *account_index,
            result: result.and_then(|settled| rsvp_line(&settled)),
        },
    }
}

/// The sentence a settled-badly operation carries, which is the engine's own
/// error rendered by the daemon.
fn operation_error(payload: &Value) -> String {
    payload["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| "the operation stopped without saying why".to_string())
}

/// One of the two passes over the active account, as its key issues it.
///
/// The three client-side checks stay ahead of the call, each because its
/// sentence belongs to the key and not to `mp`: the gate that parks a pass
/// behind the one already running, the transport check whose refusal the daemon
/// words for `mp sync`, and the progress line, which still says "(Graph)"
/// because that is what the user reads while it runs.
fn start_pass(app: &mut App, commands: &dyn Queries, method: &str, label: &str, action: Action) {
    if super::actions::sync_is_blocked(app) {
        return super::actions::park_until_idle(app, action, label);
    }
    if !app.is_graph() && app.imap_config.is_none() {
        return app.set_status_level("IMAP not configured".to_string(), StatusLevel::Error);
    }
    let account = app.account_config.name.clone();
    let account_index = app.active_account;
    let awaited = if method == "sync.quick" {
        Awaited::Quick {
            account_index,
            account: account.clone(),
        }
    } else {
        Awaited::Full {
            account_index,
            account: account.clone(),
        }
    };
    let progress = if app.is_graph() {
        format!("{label} (Graph)...")
    } else {
        format!("{label}...")
    };
    app.set_status_level(progress, StatusLevel::Progress);
    start_operation(app, commands, method, json!({"account": account}), awaited);
}

/// The startup auto-fetch's per-account quick pass (#0001).
///
/// Ungated, unlike [`start_pass`]: the accounts open concurrently and each one
/// fetches as it comes up, so a second account may not be parked behind the
/// first. A local-only account is a silent no-op, because the auto-fetch runs
/// over every account and must not narrate one that has no server.
fn start_account_pass(app: &mut App, commands: &dyn Queries, index: usize) {
    let Some(account) = app.accounts.get(index) else {
        return;
    };
    let graph = account.is_graph();
    if graph && account.graph_config.is_none() {
        return;
    }
    if !graph && account.imap_config.is_none() {
        return;
    }
    let name = account.account_config.name.clone();
    app.set_status_level(
        if graph {
            format!("Quick sync ({name}, Graph)...")
        } else {
            format!("Quick sync ({name})...")
        },
        StatusLevel::Progress,
    );
    start_operation(
        app,
        commands,
        "sync.quick",
        json!({"account": name}),
        Awaited::Quick {
            account_index: index,
            account: name,
        },
    );
}

/// `send.approved` over the open Drafts directory.
///
/// The two client-side refusals are the key's own sentences: a view with no
/// drafts directory answers what the empty scan answered, and a transport that
/// cannot be resolved is named before an operation is started for it.
fn send_approved(app: &mut App, commands: &dyn Queries) {
    if app.active_drafts_dir().is_none() {
        return app.set_status_level("No approved emails found".to_string(), StatusLevel::Success);
    }
    let graph = match super::helpers::resolve_send_transport(
        &app.account_config,
        app.graph_config.clone(),
        app.smtp_config.clone(),
    ) {
        Ok((graph, _smtp)) => graph.is_some(),
        Err(missing) => return app.set_status_level(missing.to_string(), StatusLevel::Error),
    };
    let account = app.account_config.name.clone();
    let account_index = app.active_account;
    app.set_status_level(
        if graph {
            "Sending approved via Graph...".to_string()
        } else {
            "Sending approved...".to_string()
        },
        StatusLevel::Progress,
    );
    start_operation(
        app,
        commands,
        "send.approved",
        json!({"account": account}),
        Awaited::SendApproved { account_index },
    );
}

/// `calendar.rsvp` for the row under the cursor.
///
/// The invitation's own iMIP payload is the source of truth for the reply and
/// the daemon reads it itself; what stays here are the three refusals whose
/// wording belongs to this key, the first of them the read
/// `TUI_APP_STORE_RESIDUE` keeps in `app/mod.rs`.
fn rsvp(app: &mut App, commands: &dyn Queries, msg: MessageRef, choice: RsvpChoice) {
    if app.load_message_ics(msg).is_none() {
        return app.set_status_level(
            "That message carries no invitation to reply to".to_string(),
            StatusLevel::Error,
        );
    }
    if app.graph_config.is_some()
        && app.account_config.auth_method == crate::config::AuthMethod::Graph
    {
        return app.set_status_level(
            "RSVP is not supported for Graph accounts yet (#0036)".to_string(),
            StatusLevel::Error,
        );
    }
    if app.smtp_config.is_none() {
        return app.set_status_level("SMTP not configured".to_string(), StatusLevel::Error);
    }
    // The three words `calendar.rsvp` takes, which are `mp invite
    // accept|tentative|decline`'s own subcommand names.
    let response = match choice {
        RsvpChoice::Accept => "accept",
        RsvpChoice::Tentative => "tentative",
        RsvpChoice::Decline => "decline",
    };
    let account = app.account_config.name.clone();
    let account_index = app.active_account;
    app.set_status_level(
        format!("Sending {} reply...", choice.label().to_lowercase()),
        StatusLevel::Progress,
    );
    start_operation(
        app,
        commands,
        "calendar.rsvp",
        json!({"account": account, "row_id": msg.row_id(), "response": response}),
        Awaited::Rsvp { account_index },
    );
}

/// The one line a finished pass shows.
///
/// [`mp_client::format::sync_status_line`] is the shared rendering of a
/// [`SyncCompleted`], word for word what `tui::helpers::finish_sync` built and
/// what its mutation-drain suffix appended; the blocked line is the one the
/// same helper returned when another engine held the lock (#0122).
fn sync_status_line(account: &str, settled: &Value) -> String {
    if settled["blocked"].as_bool().unwrap_or(false) {
        return format!("{SYNC_SKIPPED_MARKER} '{account}'; leaving the ingest to it");
    }
    match serde_json::from_value::<SyncCompleted>(settled["outcome"].clone()) {
        Ok(outcome) => mp_client::format::sync_status_line(&outcome),
        Err(e) => {
            log::warn!("[commands] the sync outcome of {account} did not decode: {e}");
            String::new()
        }
    }
}

/// The inbox arrivals a pass reported, for the desktop notification (#0009).
fn new_inbox_mail(settled: &Value) -> Vec<NewMailMeta> {
    settled["new_inbox_mail"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    NewMailMeta::new(
                        row["from"].as_str().unwrap_or_default(),
                        row["subject"].as_str().unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `send.approved`, as the line the arm has always posted: how many drafts went
/// and how many did not.
///
/// An account with no approved draft is `0 sent, 0 failed`, which is the
/// sentence the in-process batch printed for an empty scan.
fn approved_line(settled: &Value) -> String {
    let sent = settled["sent"].as_u64().unwrap_or_default();
    let failed = settled["failed"].as_u64().unwrap_or_default();
    if sent == 0 && failed == 0 {
        return "No approved emails found".to_string();
    }
    format!("{sent} sent, {failed} failed")
}

/// `calendar.rsvp`'s settled outcome, as the line the arm has always posted.
///
/// A reply that reached nobody is a failure and not a green line, which is the
/// `any_succeeded` check the arm made on the outcome it had in hand.
fn rsvp_line(settled: &Value) -> Result<String, String> {
    let organizer = settled["organizer"].as_str().unwrap_or_default();
    if !settled["delivered"].as_bool().unwrap_or(false) {
        return Err(format!("Failed to send RSVP to {organizer}"));
    }
    Ok(format!(
        "{} — replied to {organizer}",
        settled["subject"].as_str().unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// The renditions a key materialises
// ---------------------------------------------------------------------------

/// The attachments of one row, materialised into files a picker can name
/// (`RD-06`'s neighbour, `#0052` scope item 8).
///
/// Two calls rather than one: `message.get` for the part list the row carries,
/// then `message.materialise_attachment` per part. The files land in the
/// daemon's handle directory instead of the per-row materialisation directory
/// `mp open` writes to, which is the one visible consequence of the move: a
/// handle expires (ten minutes by default) where a materialised copy survived
/// until the temp directory was swept.
///
/// An empty vector is a message with no attachments, which is the caller's
/// status line to write; `Err` is a sentence for it to print.
pub(super) fn attachment_files(
    commands: &dyn Queries,
    account: &str,
    row_id: i64,
) -> Result<Vec<PathBuf>, String> {
    let shown = commands
        .call(
            "message.get",
            json!({"account": account, "row_id": row_id, "body": false}),
        )
        .map_err(|e| format!("{e:#}"))?;
    let parts = shown["attachments"].as_array().map_or(0, Vec::len);
    (0..parts)
        .map(|part| {
            commands
                .call(
                    "message.materialise_attachment",
                    json!({"account": account, "row_id": row_id, "part": part}),
                )
                .map_err(|e| format!("{e:#}"))
                .and_then(|handle| {
                    handle["path"]
                        .as_str()
                        .map(PathBuf::from)
                        .ok_or_else(|| format!("part {part} was materialised without a path"))
                })
        })
        .collect()
}

/// The `Fwd:` subject the forward wizard opens with, built from the row's own
/// subject rather than from the list entry, whose `(no subject)` placeholder
/// must never reach a sent header.
///
/// `message.get` with no body: the subject is one field of the record and this
/// is a keypress, not a read of the message.
pub(super) fn forward_subject(commands: &dyn Queries, account: &str, row_id: i64) -> String {
    let subject = commands
        .call(
            "message.get",
            json!({"account": account, "row_id": row_id, "body": false}),
        )
        .map_err(|e| {
            log::warn!("[commands] the subject of row {row_id} of {account}: {e:#}");
        })
        .ok()
        .and_then(|shown| shown["subject"].as_str().map(str::to_string))
        .unwrap_or_default();
    crate::draft::fwd_subject(&subject)
}

/// The browser rendition of one row, as a file a `file://` URL can name.
///
/// `message.materialise_html` is the daemon's own copy of what the TUI built
/// inline: the charset meta, the CSP tag and the `cid:` inlining, all three of
/// them #0037 fixes that a second renderer would have had to keep in step. A
/// message whose sender wrote no markup is refused, which is not an error and
/// is the caller's "No HTML version available".
pub(super) fn html_rendition(
    commands: &dyn Queries,
    account: &str,
    row_id: i64,
) -> Result<PathBuf, String> {
    let handle = commands
        .call(
            "message.materialise_html",
            json!({"account": account, "row_id": row_id}),
        )
        .map_err(|e| format!("{e:#}"))?;
    handle["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "the rendition was materialised without a path".to_string())
}

/// The file behind an indexed draft id, without a status line: the batch flows
/// count their misses instead of narrating each one.
///
/// `draft.path` is the family's resolver and answers from a fresh directory
/// scan, which is what makes a draft written a moment ago resolvable: it is the
/// same reason the store-backed helper used `Store::open` rather than
/// `open_store`, drafts being local-only files an account with no store still
/// has.
///
/// A refusal is `Ok(None)`: `draft.path` answers `-32602` for an id nothing
/// resolves to, and that is the "no longer in the index" case rather than a
/// failure to read the index. There is no second case left to tell it from -
/// a daemon that could not scan the directory answers the same code - so the
/// caller's two status lines became one.
pub(super) fn draft_path(commands: &dyn Queries, account: &str, id: &str) -> Option<PathBuf> {
    match commands.call("draft.path", json!({"account": account, "id": id})) {
        Ok(location) => location["path"].as_str().map(PathBuf::from),
        Err(e) => {
            log::warn!("[commands] {account} has no draft {id}: {e:#}");
            None
        }
    }
}

/// The draft a reply or a forward of `row_id` writes, as its path and its
/// selector.
///
/// `draft.reply` and `draft.forward` are the whole of what the client used to
/// do in three steps (read the row, build the `SourceMessage`, run
/// `create_draft_from_source`), and the daemon runs them through the very same
/// library functions, so the file is byte-identical to the one `mp reply` and
/// `mp forward` write. `headers` is the compose wizard's override of the
/// recipients and the subject it asked for before the draft existed.
pub(super) fn draft_from_source(
    commands: &dyn Queries,
    account: &str,
    row_id: i64,
    kind: crate::draft::DraftFromSource,
    headers: Option<&crate::draft::DraftRecipientEdit>,
) -> Result<(PathBuf, String), String> {
    let (method, all) = match kind {
        crate::draft::DraftFromSource::Reply { all } => ("draft.reply", Some(all)),
        crate::draft::DraftFromSource::Forward => ("draft.forward", None),
    };
    let mut params = json!({"account": account, "source": {"row_id": row_id}});
    if let Some(all) = all {
        params["all"] = json!(all);
    }
    if let Some(headers) = headers {
        params["headers"] = json!({
            "to": headers.to,
            "cc": headers.cc,
            "bcc": headers.bcc,
            "subject": headers.subject,
        });
    }
    let created = commands
        .call(method, params)
        .map_err(|e| format!("{e:#}"))?;
    let path = created["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{method} wrote a draft it did not name"))?;
    let selector = created["selector"].as_str().unwrap_or_default().to_string();
    Ok((path, selector))
}

/// The rows of the local index that answer `query`, as the search overlay's
/// own row type (#0105).
///
/// `message.search` with `body: true`: the overlay renders a hit's body out of
/// its `fetched` payload, so the body travels with the row rather than costing
/// a `message.get` per hit. The query is rendered back into the grammar the
/// method parses ([`crate::search::to_query_string`]), which is what keeps the
/// AST off the wire.
pub(super) fn local_search(
    commands: &dyn Queries,
    account: &str,
    query: &crate::search::Query,
    mailbox: Option<&str>,
    limit: usize,
) -> Vec<super::app::SearchResultEntry> {
    let params = json!({
        "account": account,
        "query": crate::search::to_query_string(query),
        "mailbox": mailbox,
        "limit": limit,
        "body": true,
    });
    match commands.call("message.search", params) {
        Ok(answer) => super::queries::decode_search_hits(&answer),
        Err(e) => {
            // The same silence the store-backed pass kept: a local pass that
            // cannot run leaves the server leg to answer, and the overlay says
            // "Searching server..." either way.
            log::warn!("[commands] the local pass over {account} found nothing: {e:#}");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// The message mutations
// ---------------------------------------------------------------------------

/// One mutation call for one row, answering whether the daemon took it.
///
/// A refusal is a log line and a skip, which is what the store-backed path did
/// with a row that was already gone (`mutations::message_id_of`): the rest of
/// the selection proceeds, and a selection where every call refused is the
/// caller's own "nothing to …" line.
fn mutate_one(commands: &dyn Queries, method: &str, mut params: Value) -> bool {
    // The interactive contract: commit the row and the owed op, and let the
    // next sync tick drain it. See the module header.
    params["settle"] = json!(false);
    match commands.call(method, params) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("[commands] {method} refused: {e:#}");
            false
        }
    }
}

/// The rows of `msgs` the daemon accepted `method` for, in the selection's
/// order.
fn mutate_each(
    commands: &dyn Queries,
    method: &str,
    account: &str,
    msgs: &[MessageRef],
    extra: &[(&str, Value)],
) -> Vec<MessageRef> {
    msgs.iter()
        .copied()
        .filter(|msg| {
            let mut params = json!({"account": account, "row_id": msg.row_id()});
            for (key, value) in extra {
                params[*key] = value.clone();
            }
            mutate_one(commands, method, params)
        })
        .collect()
}

/// Archive one or many messages (`MSG-01`): each row moves into the archive
/// mailbox and the owed server move is queued with it (#0039).
///
/// The drain carries them to the server at the next sync/fetch resume point and
/// rolls a refusal back, so there is no per-op thread and no status to wait on:
/// the local move is instant and confirmed. `batch` says whether the selection
/// should be cleared afterwards, the only difference between the single and the
/// batch arm.
pub(super) fn archive_msgs(
    app: &mut App,
    commands: &dyn Queries,
    msgs: Vec<MessageRef>,
    batch: bool,
) {
    let Some(dest_idx) = app.find_mailbox_by_kind(MailboxKind::Archive) else {
        app.set_status_level(
            "Archive mailbox not configured".to_string(),
            StatusLevel::Error,
        );
        return;
    };
    let account = app.account_config.name.clone();
    let touched_invite = any_invite(app, &msgs);
    let archived_refs = mutate_each(commands, "message.archive", &account, &msgs, &[]);
    if archived_refs.is_empty() {
        app.set_status_level(
            "Archive failed: nothing to archive".to_string(),
            StatusLevel::Error,
        );
        return;
    }

    let archived: HashSet<MessageRef> = archived_refs.iter().copied().collect();
    app.remove_selected_from_list_batch(&archived);
    if batch {
        app.selection.clear();
    }
    refresh_after_mutation(app, Some(dest_idx), touched_invite);

    let count = archived_refs.len();
    app.set_status_level(
        if count == 1 {
            "Email archived".to_string()
        } else {
            format!("Archived {count} emails")
        },
        StatusLevel::Success,
    );
}

/// Delete one or many messages (`MSG-02`): the store rows go and the owed
/// server deletes are queued with them (#0039).
///
/// The rows are removed rather than tombstoned, so a refused server delete is
/// answered by the next sync refetching the message; the drain surfaces the
/// refusal (see [`crate::pending_ops`]).
fn delete_msgs(app: &mut App, commands: &dyn Queries, msgs: Vec<MessageRef>, batch: bool) {
    let account = app.account_config.name.clone();
    let touched_invite = any_invite(app, &msgs);
    let deleted_refs = mutate_each(commands, "message.delete", &account, &msgs, &[]);
    if deleted_refs.is_empty() {
        app.set_status_level(
            "Delete failed: nothing to delete".to_string(),
            StatusLevel::Error,
        );
        return;
    }

    // Every deleted row's id is dead the moment the row is: the list, the
    // selection set and the cursor anchor must not carry one across this
    // boundary, because a re-ingest of the same message mints a new id.
    let deleted: HashSet<MessageRef> = deleted_refs.iter().copied().collect();
    app.remove_selected_from_list_batch(&deleted);
    if batch {
        app.selection.clear();
    }
    refresh_after_mutation(app, None, touched_invite);

    let count = deleted_refs.len();
    app.set_status_level(
        if count == 1 {
            "Email deleted".to_string()
        } else {
            format!("Deleted {count} emails")
        },
        StatusLevel::Success,
    );
}

/// Quick-move to an arbitrary mailbox (`MSG-05`, #0018): the generalised
/// archive.
fn move_msgs(app: &mut App, commands: &dyn Queries, msgs: Vec<MessageRef>, dest_idx: usize) {
    let (destination, dest_label) = match app.mailboxes.get(dest_idx) {
        Some(mb) => (mailbox_key(mb), mb.label.clone()),
        None => return,
    };
    // No client-side check that the destination has a server-side folder any
    // more: `find_server_name_for_role` is the daemon's own resolution of the
    // account's `[[mailboxes]]` mapping, and the sidebar's `server_name` was a
    // second copy of it that refused moves the daemon can name a folder for.

    let account = app.account_config.name.clone();
    let touched_invite = any_invite(app, &msgs);
    let moved = mutate_each(
        commands,
        "message.move",
        &account,
        &msgs,
        &[("destination", json!(destination))],
    );
    if moved.is_empty() {
        app.set_status_level(
            "Move failed: nothing to move".to_string(),
            StatusLevel::Error,
        );
        return;
    }

    let removed: HashSet<MessageRef> = moved.iter().copied().collect();
    app.remove_selected_from_list_batch(&removed);
    app.selection.clear();
    refresh_after_mutation(app, Some(dest_idx), touched_invite);

    let count = moved.len();
    app.set_status_level(
        if count == 1 {
            format!("Moved to {dest_label}")
        } else {
            format!("Moved {count} emails to {dest_label}")
        },
        StatusLevel::Success,
    );
}

/// Set the read flag on one or many messages (`MSG-03`).
///
/// Returns false when nothing was applied, so the caller can skip its status
/// line. The in-memory list is updated beside the row because the list is what
/// the user is looking at; the drain rolls both the store row and (on the next
/// refresh) the list back if the server refuses.
fn set_read_flag(app: &mut App, commands: &dyn Queries, msgs: &[MessageRef], read: bool) -> bool {
    let account = app.account_config.name.clone();
    let flagged = mutate_each(
        commands,
        "message.set_read",
        &account,
        msgs,
        &[("read", json!(read))],
    );
    if flagged.is_empty() {
        return false;
    }
    for msg in &flagged {
        app.set_email_read(*msg, read);
    }
    true
}

/// Set the `\Flagged` star on one or many messages (`MSG-04`, #0007).
fn set_flag(app: &mut App, commands: &dyn Queries, msgs: &[MessageRef], flagged: bool) -> bool {
    let account = app.account_config.name.clone();
    let starred = mutate_each(
        commands,
        "message.set_flag",
        &account,
        msgs,
        &[("flagged", json!(flagged))],
    );
    if starred.is_empty() {
        return false;
    }
    for msg in &starred {
        app.set_email_flagged(*msg, flagged);
    }
    true
}

/// Mark the row an explicit open resolved read (`MSG-08`, #0110).
///
/// The trigger is an explicit open: `Enter` / `e`, or a focus move into the
/// body pane. This reverses #0087, whose trigger was merely showing a row in
/// the preview, so a `j` / `k` walk down an unread inbox leaves it unread and
/// queues no `\Seen` ops.
///
/// The message is passed in rather than read off the cursor: the queued
/// [`Action::MarkAsRead`] is drained after a whole coalesced key batch (#0108),
/// by which time the cursor may sit on a different row than the one that was
/// opened. A ref the list no longer holds is a no-op, as is an already-read
/// one, so the mark costs one call per genuine open rather than one per
/// keypress.
pub(super) fn mark_open_read(app: &mut App, commands: &dyn Queries, msg: MessageRef) -> bool {
    let Some(email) = app.emails.iter().find(|e| e.msg == Some(msg)) else {
        return false;
    };
    if email.read {
        return false;
    }
    set_read_flag(app, commands, &[msg], true)
}

/// True when any of `msgs` is an invite row in the current list, read *before*
/// the mutation removes them.
pub(super) fn any_invite(app: &App, msgs: &[MessageRef]) -> bool {
    app.emails
        .iter()
        .any(|e| e.is_invite && e.msg.is_some_and(|m| msgs.contains(&m)))
}

/// What a mutation leaves stale beyond the list it just changed.
///
/// The destination mailbox's cached list no longer matches its rows, every
/// sidebar count is one query away from the truth, and an invite that moved or
/// died changes the agenda the Calendar view is holding (the same refresh
/// `bg.rs` runs after an RSVP). The mutation has already been committed when
/// this is called, so all three read the new state.
pub(super) fn refresh_after_mutation(app: &mut App, dest_idx: Option<usize>, touched_invite: bool) {
    if let Some(idx) = dest_idx {
        app.invalidate_cache_idx(idx);
    }
    app.recount_all_mailboxes();
    if touched_invite {
        app.rebuild_calendar_if_loaded();
    }
}

// ---------------------------------------------------------------------------
// The drafts
// ---------------------------------------------------------------------------

/// Which way a draft's `status:` is flipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flip {
    Approve,
    Demote,
}

impl Flip {
    /// The method that performs it.
    fn method(self) -> &'static str {
        match self {
            Flip::Approve => "draft.approve",
            Flip::Demote => "draft.demote",
        }
    }

    /// How the operation names itself in a failure line.
    fn what(self) -> &'static str {
        match self {
            Flip::Approve => "Approve",
            Flip::Demote => "Mark-draft",
        }
    }

    /// The status a draft already in the target state carries.
    fn target_status(self) -> &'static str {
        match self {
            Flip::Approve => "approved",
            Flip::Demote => "draft",
        }
    }

    /// The line one flip shows, in the CLI's two shapes: the draft moved, or it
    /// was already where it was being moved to. Named by its selector, which is
    /// the handle the user can hand to `mp` (#0050), not by a path.
    fn line(self, already: bool, selector: &Selector) -> String {
        match (self, already) {
            (Flip::Approve, false) => format!("Approved {selector}"),
            (Flip::Approve, true) => format!("Already approved: {selector}"),
            (Flip::Demote, false) => format!("Demoted {selector}"),
            (Flip::Demote, true) => format!("Already a draft: {selector}"),
        }
    }
}

/// The indexed id of the draft under the cursor, or `None` with the status line
/// saying why there is not one.
///
/// `why` is what to say when the cursor is on a received message instead, which
/// every draft-only operation has to answer for itself. An empty list is not an
/// error at all and says nothing.
fn cursor_draft_id(app: &mut App, why: &str) -> Option<String> {
    let id = app.selected_email()?.draft_id.clone();
    match id {
        Some(id) => Some(id),
        None => {
            app.set_status_level(why.to_string(), StatusLevel::Warning);
            None
        }
    }
}

/// Whether the row under the cursor already carries `status`.
///
/// The daemon's answer says what the draft is now, not what it was, so
/// "Already approved" is read off the row the user is looking at. It is the
/// same source the list column renders, so the line cannot disagree with the
/// screen it is printed under.
fn cursor_status_is(app: &App, status: &str) -> bool {
    app.selected_email()
        .is_some_and(|entry| entry.status == status)
}

/// Flip the `status:` of the draft under the cursor (`DFT-04`, `DFT-05`).
fn status_flip(app: &mut App, commands: &dyn Queries, flip: Flip) {
    let why = format!(
        "{} needs a draft; received mail has no draft status to flip",
        flip.what()
    );
    let Some(id) = cursor_draft_id(app, &why) else {
        return;
    };
    let already = cursor_status_is(app, flip.target_status());
    let account = app.account_config.name.clone();
    match commands.call(flip.method(), json!({"account": account, "id": id})) {
        Ok(_) => {
            let selector = Selector::for_draft(&account, &id);
            app.set_status(flip.line(already, &selector));
            refresh_drafts_after_flip(app);
        }
        Err(e) => {
            app.set_status_level(format!("{} failed: {e:#}", flip.what()), StatusLevel::Error)
        }
    }
}

/// Flip the `status:` of every selected draft, counting what took it.
///
/// The counting and its two status-line shapes are the pre-nuke build's: a
/// batch is not all-or-nothing, and a draft the flip refuses (an already-sent
/// one, say) is one failure among N rather than an abort. The reason lands in
/// the log, because the status line has room for a count and not for N errors.
fn status_flip_batch(app: &mut App, commands: &dyn Queries, ids: &[String], flip: Flip) {
    let total = ids.len();
    let account = app.account_config.name.clone();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for id in ids {
        match commands.call(flip.method(), json!({"account": account, "id": id})) {
            Ok(_) => succeeded += 1,
            Err(e) => {
                log::warn!("[drafts] {} failed for {id}: {e:#}", flip.what());
                failed += 1;
            }
        }
    }
    let line = match flip {
        Flip::Approve if failed == 0 => format!("Approved {succeeded} drafts"),
        Flip::Approve => format!("Approved {succeeded}/{total} drafts ({failed} failed)"),
        Flip::Demote if failed == 0 => format!("Marked {succeeded} as draft"),
        Flip::Demote => format!("Marked {succeeded}/{total} as draft ({failed} failed)"),
    };
    if failed == 0 {
        app.set_status(line);
    } else {
        app.set_status_level(line, StatusLevel::Warning);
    }
    refresh_drafts_after_flip(app);
}

/// Discard the draft under the cursor (`MSG-02`'s drafts half, #0073).
///
/// The TUI never force-deletes: an approved draft keeps its guard, and the user
/// demotes it with the mark-draft key first, exactly as the CLI asks. An
/// in-flight draft (#0063) is refused by the same library check the CLI runs,
/// now on the daemon's side of the socket.
fn delete_draft(app: &mut App, commands: &dyn Queries, id: &str) {
    let account = app.account_config.name.clone();
    match commands.call("draft.discard", json!({"account": account, "id": id})) {
        Ok(_) => {
            let selector = Selector::for_draft(&account, id);
            app.set_status(format!("Deleted {selector}"));
            refresh_drafts_after_flip(app);
        }
        Err(e) => app.set_status_level(format!("Delete failed: {e:#}"), StatusLevel::Error),
    }
}

/// Delete a parse-skipped draft by its path (#0080).
///
/// A skipped file has no index row and no `id:`, so there is nothing to address
/// and no guard [`delete_draft`] runs can apply: the file the error row names is
/// removed straight from disk, and the drafts refresh drops the row it stood
/// for. It is the one branch of `d` that reaches no method, because none takes a
/// path.
fn delete_skip_file(app: &mut App, path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {
            app.set_status(format!("Deleted {}", path.display()));
            refresh_drafts_after_flip(app);
        }
        Err(e) => app.set_status_level(format!("Delete failed: {e:#}"), StatusLevel::Error),
    }
}

/// Delete every selected draft, counting what went and logging what was
/// refused (#0073). A draft the guard keeps (approved, or mid-send) is one miss
/// among N, not an abort.
fn delete_drafts_batch(app: &mut App, commands: &dyn Queries, ids: &[String]) {
    let account = app.account_config.name.clone();
    let total = ids.len();
    let mut deleted = 0usize;
    for id in ids {
        match commands.call("draft.discard", json!({"account": account, "id": id})) {
            Ok(_) => deleted += 1,
            Err(e) => log::warn!("[drafts] not deleting {id}: {e:#}"),
        }
    }
    if deleted == total {
        app.set_status(format!("Deleted {deleted} drafts"));
    } else {
        app.set_status_level(
            format!("Deleted {deleted} of {total} drafts; the rest were kept (see the log)"),
            StatusLevel::Warning,
        );
    }
    refresh_drafts_after_flip(app);
}

/// Re-index the drafts directory after a change and put the list, the sidebar
/// counts and the cached Drafts mailbox back in step with the files.
///
/// Same sequence as `mp mark-approved`'s `reindex_drafts`, plus the refresh of
/// the two things the CLI does not have: an open list and a sidebar count.
pub(super) fn refresh_drafts_after_flip(app: &mut App) {
    if let Err(e) = crate::store::drafts::refresh_account(&app.account_config.name) {
        log::warn!("[drafts] refreshing after a status flip failed: {e:#}");
    }
    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }
    app.recount_all_mailboxes();
    app.reload_current_mailbox();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::EmailEntry;
    use crate::tui::test_daemon::TestDaemon;

    const ACCOUNT: &str = "alice";

    /// A daemon over a fixture data root, reachable as a [`Queries`].
    ///
    /// [`TestDaemon`](crate::tui::test_daemon::TestDaemon) is the shared
    /// fixture; what is added here is the seeded account it serves and the
    /// tempdir that holds it, which lives as long as the fixture because every
    /// path in sight resolves under it.
    struct Daemon {
        daemon: TestDaemon,
        _data: crate::config::test_env::TestDataDir,
    }

    impl Daemon {
        fn new() -> Daemon {
            let data = crate::config::test_env::TestDataDir::new();
            std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
            drop(crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store"));
            Daemon {
                daemon: TestDaemon::new(&[ACCOUNT]),
                _data: data,
            }
        }
    }

    impl Queries for Daemon {
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            self.daemon.call(method, params)
        }
    }

    /// One list row, everything about it derived from `subject`.
    fn entry(subject: &str, id: i64, is_invite: bool) -> EmailEntry {
        EmailEntry {
            msg: Some(MessageRef::new(id)),
            draft_id: None,
            skip: None,
            from: "Sender <s@example.com>".to_string(),
            to: "me@example.com".to_string(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: subject.to_string(),
            status: "inbox".to_string(),
            date_display: "2026-07-01".to_string(),
            date_sort: "2026-07-01T00:00:00".to_string(),
            has_attachments: false,
            read: false,
            answered: false,
            forwarded: false,
            flagged: false,
            is_invite,
        }
    }

    /// One drafts row under the cursor, in the state the file is in.
    fn draft_entry(id: &str, status: &str) -> EmailEntry {
        EmailEntry {
            msg: None,
            draft_id: Some(id.to_string()),
            skip: None,
            from: String::new(),
            to: "alice@example.com".to_string(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "Re: Hello".to_string(),
            status: status.to_string(),
            date_display: "2026-07-01".to_string(),
            date_sort: "2026-07-01T00:00:00".to_string(),
            has_attachments: false,
            read: true,
            answered: false,
            forwarded: false,
            flagged: false,
            is_invite: false,
        }
    }

    /// An app on `ACCOUNT` whose cursor sits on `id`'s Drafts row.
    fn app_on_draft(id: &str, status: &str) -> App {
        let mut app = App::default_for_tests();
        app.account_config.name = ACCOUNT.to_string();
        app.emails = std::sync::Arc::new(vec![draft_entry(id, status)]);
        app.rebuild_visible();
        app
    }

    /// Write one draft file and index it, handing back its id.
    fn a_draft(id: &str) -> String {
        let dir = crate::config::drafts_dir(ACCOUNT);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.md")),
            format!(
                "---\nid: {id}\nfrom: me@example.com\nto: you@example.com\nsubject: Hello\n\
                 status: draft\ndate: 2024-01-01T09:00:00+00:00\n---\n\nBody.\n"
            ),
        )
        .unwrap();
        crate::store::drafts::refresh_account(ACCOUNT).unwrap();
        id.to_string()
    }

    /// The mark that rides on an explicit open (#0110) is a real mutation, not
    /// an intent: the store row gains `\Seen` and exactly one `SetRead` op is
    /// owed to the server, because `message.set_read` queues the pair (#0039).
    /// A second call over the same row is a no-op, so re-opening does not queue
    /// a duplicate op.
    ///
    /// It was `actions.rs`'s until P5-U6 moved the mutation here; what is new
    /// is the door, and that the row is written by the daemon over an account
    /// with no credentials, which only a queueing call can do.
    #[test]
    fn an_open_marks_the_row_it_resolved_and_queues_one_server_op() {
        let daemon = Daemon::new();
        let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
        let blobs = crate::store::BlobStore::for_account(ACCOUNT);
        let email = crate::parse::FetchedEmail {
            from: "Sender <s@example.com>".into(),
            to: "me@example.com".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "Unread".into(),
            date: "Mon, 20 Jul 2026 09:00:00 +0000".into(),
            body_text: "Hello.".into(),
            html_body: None,
            has_attachments: false,
            message_id: Some("<inbox-1@example.com>".into()),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        let row_id = crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: ACCOUNT,
                mailbox: "inbox",
                uid: 1,
                email: &email,
                raw: None,
            },
        )
        .unwrap()
        .row_id;
        drop(store);

        let mut app = App::default_for_tests();
        app.account_config.name = ACCOUNT.to_string();
        // The cursor sits on a *different* row than the one that was opened,
        // which is what a `Tab` and a `J` coalesced into one batch produce
        // (#0108): the mark must follow the ref it was given, not the cursor.
        app.emails = std::sync::Arc::new(vec![
            entry("Unread", row_id, false),
            entry("Moved onto", row_id + 1, false),
        ]);
        app.visible = vec![0, 1];
        app.list_index = 1;

        let msg = MessageRef::new(row_id);
        assert!(
            mark_open_read(&mut app, &daemon, msg),
            "the open marked nothing"
        );
        assert!(app.emails[0].read, "the opened list row is stale");
        assert!(!app.emails[1].read, "the row under the cursor was marked");

        let store = crate::store::open_store(ACCOUNT).unwrap();
        assert!(crate::store::read::find_by_id(&store, row_id)
            .unwrap()
            .unwrap()
            .is_read());
        let queued = crate::pending_ops::queued_ops(&store, ACCOUNT).unwrap();
        assert_eq!(queued.len(), 1, "expected exactly one owed server op");
        assert_eq!(
            queued[0].op,
            crate::ops::ServerOp::SetRead {
                message_id: "<inbox-1@example.com>".to_string(),
                // The row's own mailbox as `find_server_name_for_role` spells
                // it, which for an account with no `[[mailboxes]]` mapping is
                // the role verbatim. It is the daemon's spelling since P4-U8
                // and it addresses the row's mailbox rather than the open one.
                mailbox: "inbox".to_string(),
                read: true,
            }
        );
        drop(store);

        assert!(
            !mark_open_read(&mut app, &daemon, msg),
            "an already-read row re-marked"
        );
        let store = crate::store::open_store(ACCOUNT).unwrap();
        assert_eq!(
            crate::pending_ops::queued_ops(&store, ACCOUNT)
                .unwrap()
                .len(),
            1,
            "re-opening queued a duplicate op"
        );
    }

    /// The agenda is only rebuilt when a mutation actually touched an invite,
    /// which is read off the list rows *before* they are removed.
    #[test]
    fn only_a_mutation_that_touches_an_invite_asks_for_an_agenda_rebuild() {
        let mut app = App::default_for_tests();
        app.emails =
            std::sync::Arc::new(vec![entry("Standup", 1, true), entry("Receipt", 2, false)]);

        assert!(any_invite(&app, &[MessageRef::new(1)]));
        assert!(any_invite(&app, &[MessageRef::new(2), MessageRef::new(1)]));
        assert!(!any_invite(&app, &[MessageRef::new(2)]));
        assert!(!any_invite(&app, &[MessageRef::new(404)]));
    }

    /// Approve and mark-draft flip the file `mp mark-approved` /
    /// `mp mark-draft` flip, name the draft by its selector, and leave the file
    /// holding the new status.
    ///
    /// "Already approved" is read off the row the user is looking at rather
    /// than off the library's return sentence, which the daemon does not carry:
    /// the list column and the status line therefore cannot disagree.
    #[test]
    fn approve_and_mark_draft_flip_the_indexed_status() {
        let daemon = Daemon::new();
        let id = a_draft("one");
        let selector = Selector::for_draft(ACCOUNT, &id);
        let mut app = app_on_draft(&id, "draft");

        status_flip(&mut app, &daemon, Flip::Approve);
        assert_eq!(
            app.status_message.as_deref(),
            Some(&*format!("Approved {selector}"))
        );
        assert!(draft_file_says(&id, "status: approved"));

        // The reload the flip triggers emptied the list (the fixture app has
        // no session to load from), so the cursor is put back by hand, on the
        // row as the flip left it.
        app.emails = std::sync::Arc::new(vec![draft_entry(&id, "approved")]);
        app.rebuild_visible();
        status_flip(&mut app, &daemon, Flip::Approve);
        assert_eq!(
            app.status_message.as_deref(),
            Some(&*format!("Already approved: {selector}"))
        );

        app.emails = std::sync::Arc::new(vec![draft_entry(&id, "approved")]);
        app.rebuild_visible();
        status_flip(&mut app, &daemon, Flip::Demote);
        assert_eq!(
            app.status_message.as_deref(),
            Some(&*format!("Demoted {selector}"))
        );
        assert!(draft_file_says(&id, "status: draft"));
    }

    /// An illegal transition fails with the daemon's own error text, which is
    /// the sentence `mp mark-draft` prints: a sent email has left the draft
    /// pipeline and is not rewritten back into it.
    #[test]
    fn marking_a_sent_draft_back_to_draft_fails_like_the_cli() {
        let daemon = Daemon::new();
        let id = a_draft("one");
        let dir = crate::config::drafts_dir(ACCOUNT);
        let path = dir.join("one.md");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("status: draft", "status: sent")).unwrap();
        crate::store::drafts::refresh_account(ACCOUNT).unwrap();

        let mut app = app_on_draft(&id, "sent");
        status_flip(&mut app, &daemon, Flip::Demote);

        let status = app.status_message.clone().unwrap();
        assert!(
            status.starts_with("Mark-draft failed:")
                && status.contains("Cannot revert a sent email back to draft"),
            "{status}"
        );
    }

    /// The batch flips every selected draft and counts what it could not do,
    /// which is the pre-nuke build's contract: one refusal is one failure, not
    /// an abort.
    #[test]
    fn the_batch_flips_every_selected_draft_and_counts_the_refusals() {
        let daemon = Daemon::new();
        let one = a_draft("one");
        let two = a_draft("two");
        let mut app = App::default_for_tests();
        app.account_config.name = ACCOUNT.to_string();

        status_flip_batch(
            &mut app,
            &daemon,
            &[one.clone(), two.clone()],
            Flip::Approve,
        );
        assert_eq!(app.status_message.as_deref(), Some("Approved 2 drafts"));
        assert!(draft_file_says(&one, "status: approved"));
        assert!(draft_file_says(&two, "status: approved"));

        status_flip_batch(
            &mut app,
            &daemon,
            &[one.clone(), "not-in-the-index".to_string()],
            Flip::Demote,
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Marked 1/2 as draft (1 failed)")
        );
        assert!(draft_file_says(&one, "status: draft"));
        assert!(draft_file_says(&two, "status: approved"));
    }

    // -----------------------------------------------------------------------
    // The operations, started here and finished by event (P5-U8)
    // -----------------------------------------------------------------------

    /// A started operation is remembered by id and counted as background
    /// work, and its finished event lands as the `BgResult` its arm posted.
    ///
    /// `calendar.rebuild` is the cheapest real operation the fixture can run:
    /// it folds the stored replies onto the stored invitations of an account
    /// with neither, so what is pinned is the machinery every sync,
    /// send-approved and RSVP arm rides on, not the fold. The finish itself
    /// arrives over a socket in a real run (`tests/tui_daemon_recovery.rs`),
    /// so here the settled payload is handed to [`settled`] directly.
    #[test]
    fn a_started_operation_is_remembered_until_its_finish_lands() {
        let daemon = Daemon::new();
        let mut app = App::default_for_tests();

        assert!(start_operation(
            &mut app,
            &daemon,
            "calendar.rebuild",
            json!({"account": ACCOUNT}),
            Awaited::Quick {
                account_index: 0,
                account: ACCOUNT.to_string(),
            },
        ));
        assert_eq!(app.bg_count, 1, "an operation is background work");

        let landed = settled(
            &Awaited::Quick {
                account_index: 0,
                account: ACCOUNT.to_string(),
            },
            &json!({
                "operation_id": "whatever",
                "state": "succeeded",
                "result": {"blocked": false, "outcome": {
                    "account": ACCOUNT, "severity": "ok", "saved": 1, "skipped": 2,
                    "flags_updated": 0, "pruned": 0, "prunes_deferred": 0, "uid_rebound": 0,
                    "uidvalidity_resets": 0, "bodies_truncated": 0, "non_converging": [],
                    "failed_mutations": 0, "error": null, "new_inbox_mail": [],
                }},
            }),
        );
        match landed {
            crate::tui::app::BgResult::Fetch { result, .. } => assert_eq!(
                result.expect("a settled pass"),
                "Synced: 1 new, 2 existing",
                "the line the polled answer produced, from the same pure function"
            ),
            other => panic!("a quick pass lands as a Fetch, got {other:?}"),
        }
    }

    /// A refused operation never starts, and the daemon's own sentence is what
    /// the status line says.
    ///
    /// An account with no server configured is refused by `sync.quick` before
    /// an operation is created at all, which is the branch a sync over a
    /// local-only account takes.
    #[test]
    fn a_refused_sync_is_the_sentence_the_daemon_gave() {
        let daemon = Daemon::new();
        let mut app = App::default_for_tests();

        assert!(
            !start_operation(
                &mut app,
                &daemon,
                "sync.quick",
                json!({"account": ACCOUNT}),
                Awaited::Quick {
                    account_index: 0,
                    account: ACCOUNT.to_string(),
                },
            ),
            "an account with no server has nothing to sync"
        );
        let line = app.status_message.clone().expect("a refusal is shown");
        assert!(
            line.contains("configures no server"),
            "the daemon's own refusal, verbatim: {line}"
        );
        assert_eq!(app.bg_count, 0, "nothing started, so nothing is pending");
    }

    /// The local search pass answers the rows the index holds, addressed by
    /// the query rendered back into the grammar `message.search` parses.
    ///
    /// The round trip is what this pins beyond `search::to_query_string`'s own
    /// tests: the overlay's AST, rendered, sent, re-parsed daemon-side and run
    /// against the FTS index, finds the row a store-backed `search_ast` found.
    /// The body travels with the hit, because the overlay renders it from the
    /// `fetched` payload rather than from a second read.
    #[test]
    fn the_local_pass_finds_the_row_the_index_holds() {
        let daemon = Daemon::new();
        let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
        let blobs = crate::store::BlobStore::for_account(ACCOUNT);
        let email = crate::parse::FetchedEmail {
            from: "Sender <s@example.com>".into(),
            to: "me@example.com".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "Quarterly zolvertrix".into(),
            date: "Mon, 20 Jul 2026 09:00:00 +0000".into(),
            body_text: "The zolvertrix is in the ledger.".into(),
            html_body: None,
            has_attachments: false,
            message_id: Some("<hit-1@example.com>".into()),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        crate::ingest::ingest_message(
            &store,
            &blobs,
            &crate::ingest::IngestInput {
                account: ACCOUNT,
                mailbox: "inbox",
                uid: 1,
                email: &email,
                raw: None,
            },
        )
        .unwrap();
        drop(store);

        let query = crate::search::parse("zolvertrix").unwrap();
        let hits = local_search(&daemon, ACCOUNT, &query, Some("inbox"), 50);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].entry.subject, "Quarterly zolvertrix");
        assert_eq!(hits[0].source_label, "inbox");
        assert!(
            hits[0].fetched.body_text.contains("zolvertrix"),
            "the hit carries its body: {:?}",
            hits[0].fetched.body_text
        );

        // A query nothing matches is an empty pass, not a failure: the server
        // leg is what answers next either way.
        let miss = crate::search::parse("nothingmatchesthis").unwrap();
        assert!(local_search(&daemon, ACCOUNT, &miss, None, 50).is_empty());
    }

    /// True when the draft file `id` names contains `needle`.
    fn draft_file_says(id: &str, needle: &str) -> bool {
        let path = crate::config::drafts_dir(ACCOUNT).join(format!("{id}.md"));
        std::fs::read_to_string(path)
            .map(|text| text.contains(needle))
            .unwrap_or(false)
    }
}
