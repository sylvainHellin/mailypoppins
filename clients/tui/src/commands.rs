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
//! `message.search_server`) that still owns a background thread and a
//! [`BgResult`](crate::app::BgResult) channel in `handle_action`,
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
//! change and the owed op in one transaction and answers, which is why `u` over
//! a thousand-message selection costs no network on the keystroke and why the
//! mark-read of an explicit open cannot stall a frame. The daemon drains the
//! queued ops itself once the account's mutations have been quiet for 1.5 s,
//! one drain for the whole selection, and publishes `mutations.rolled_back`
//! for any the server refused (#0133). The parameter defaults to `true` on the
//! wire so the CLI does not move.
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
use mp_protocol::send::SendOutcome;

pub use super::app::MessageAddress;
use super::app::{
    mailbox_key, Action, App, BgResult, MailboxKind, MessageRef, RsvpChoice, SearchOverlayFocus,
    SearchTarget, StatusLevel,
};
use super::events::Awaited;
use super::helpers::SYNC_SKIPPED_MARKER;
use super::queries::Queries;
use mp_core::notify::NewMailMeta;
use mp_core::selector::Selector;

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
        // Validated and approved first, the desktop's sequence (#0089).
        Action::Send => ActionRoute::Daemon(&["draft.validate", "draft.approve", "send.draft"]),
        Action::SendApproved => ActionRoute::Daemon(&["send.approved"]),
        Action::CancelHeldSend => ActionRoute::Daemon(&["send.cancel_hold"]),
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
        // A received mailbox streams (#0138), as conversations when the
        // account lists them, whose expanded members are `message.thread`
        // (#0142); the Drafts mailbox is the drafts directory's own listing.
        Action::LoadMailbox { .. } => ActionRoute::Daemon(&[
            "message.list_stream",
            "message.list_conversations",
            "message.thread",
            "draft.list",
        ]),
        Action::Sync => ActionRoute::Daemon(&["sync.full"]),
        // The local pass first, then the server leg (LST-08).
        Action::ServerSearch { .. } => {
            ActionRoute::Daemon(&["message.search", "message.search_server"])
        }
        Action::SearchResultOpen => {
            ActionRoute::ClientOnly("$EDITOR, over a rendition this process opens")
        }
        Action::SearchResultJump => ActionRoute::Local,
        Action::SearchResultYankPath => ActionRoute::ClientOnly("the system clipboard"),
        Action::SearchResultFetch => ActionRoute::Daemon(&["message.fetch"]),
        Action::SearchResultReply(_) => ActionRoute::Daemon(&["draft.reply"]),
        Action::SearchResultForward | Action::SearchResultForwardAsAttachment => {
            ActionRoute::Daemon(&["draft.forward"])
        }
        Action::SearchResultExport => ActionRoute::Daemon(&["message.materialise_eml"]),
        Action::SearchResultArchive => ActionRoute::Daemon(&["message.archive"]),
        Action::SearchResultOpenInBrowser => ActionRoute::Daemon(&["message.materialise_html"]),
        Action::OpenHtmlInBrowser(_) => {
            ActionRoute::ClientOnly("the system browser, over an already-materialised path")
        }
        Action::OpenComposeWizard(_) => ActionRoute::Local,
        Action::ComposeWizardSubmit => ActionRoute::Daemon(&["draft.create"]),
        Action::ComposeWizardCancel => ActionRoute::Local,
        // The daemon names the file (and creates it when the selection went
        // stale); the editor session is this process's.
        Action::ComposeEditSignature => {
            ActionRoute::Daemon(&["signature.read", "signature.create"])
        }
        Action::Rsvp { .. } => ActionRoute::Daemon(&["calendar.rsvp"]),
        Action::ComposeToContact { .. } => ActionRoute::Local,
        Action::SendContactVcard { .. } => ActionRoute::Daemon(&["contact.vcard_draft"]),
        Action::CopyContactEmail { .. } => ActionRoute::ClientOnly("the system clipboard"),
        Action::LoadContacts { .. } => ActionRoute::Daemon(&["contact.search"]),
        Action::RefreshContacts => ActionRoute::Daemon(&["contact.rebuild"]),
        Action::OpenEventSource { .. } => {
            ActionRoute::ClientOnly("$EDITOR, over the invite the agenda row came from")
        }
        Action::EditSignatureFile { .. } => {
            ActionRoute::Daemon(&["signature.read", "signature.create"])
        }
        Action::AttachFileToDraft { .. } => ActionRoute::Daemon(&["draft.attach"]),
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
            // A conversation header reaches its members in the listed
            // mailbox (#0142); any other row is itself.
            let msgs = app
                .selected_email()
                .map(|e| e.action_refs())
                .unwrap_or_default();
            if !msgs.is_empty() {
                // The status line already carries the outcome.
                let _ = archive_msgs(app, commands, msgs, false);
            }
            true
        }
        Action::BatchArchive(msgs) => {
            let _ = archive_msgs(app, commands, msgs.clone(), true);
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
                delete_skip_file(app, std::path::Path::new(&path));
            } else if let Some(id) = draft_id {
                delete_draft(app, commands, &id);
            } else if let Some(msgs) = app
                .selected_email()
                .map(|e| e.action_refs())
                .filter(|msgs| !msgs.is_empty())
            {
                delete_msgs(app, commands, msgs, false);
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
                let msgs = email.action_refs();
                if !msgs.is_empty() {
                    let label = if new_read {
                        "Marked as read"
                    } else {
                        "Marked as unread"
                    };
                    if set_read_flag(app, commands, &msgs, new_read) {
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
                let msgs = email.action_refs();
                if !msgs.is_empty() {
                    let label = if new_flag { "Flagged" } else { "Unflagged" };
                    if set_flag(app, commands, &msgs, new_flag) {
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
        Action::CancelHeldSend => {
            cancel_hold(app, commands);
            true
        }
        Action::Rsvp { msg, choice } => {
            rsvp(app, commands, *msg, *choice);
            true
        }
        Action::RefreshContacts => {
            rebuild_contacts(app, commands);
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
pub fn start_operation(
    app: &mut App,
    commands: &dyn Queries,
    method: &str,
    params: Value,
    awaited: Awaited,
) -> bool {
    start_operation_id(app, commands, method, params, awaited).is_some()
}

/// [`start_operation`], answering the id it started rather than whether it
/// did.
///
/// One caller wants the id: the overlay's server leg has to tell one search's
/// streamed hits from a faster retype's, and the `message.server_hit` events
/// carry the operation id and no generation.
pub(super) fn start_operation_id(
    app: &mut App,
    commands: &dyn Queries,
    method: &str,
    params: Value,
    awaited: Awaited,
) -> Option<String> {
    let started = match commands.call(method, params) {
        Ok(started) => started,
        Err(e) => {
            app.set_status_level(format!("{method}: {e:#}"), StatusLevel::Error);
            return None;
        }
    };
    let Some(id) = started["operation_id"].as_str().map(str::to_string) else {
        app.set_status_level(
            format!("{method} answered no operation id"),
            StatusLevel::Error,
        );
        return None;
    };
    app.bg_count += 1;
    app.events.started(id.clone(), awaited);
    Some(id)
}

/// One finished operation as the [`BgResult`] its arm used to post.
///
/// The event's payload carries `result` exclusive-or `error`
/// (`src/daemon/operations.rs`), and the four shapes below are the four the
/// arms already knew how to present, so nothing above this changed when the
/// poll went away.
pub fn settled(awaited: &Awaited, payload: &Value) -> BgResult {
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
        Awaited::Send { account_index } => BgResult::Send {
            account_index: *account_index,
            result: result.and_then(|settled| sent_line(&settled)),
        },
        Awaited::SendApproved { account_index } => BgResult::SendApproved {
            account_index: *account_index,
            result: result.map(|settled| approved_line(&settled)),
        },
        Awaited::Rsvp { account_index } => BgResult::Rsvp {
            account_index: *account_index,
            result: result.and_then(|settled| rsvp_line(&settled)),
        },
        Awaited::ServerSearch { generation } => BgResult::ServerSearch {
            generation: *generation,
            result,
        },
        Awaited::ContactRebuild { account_index } => BgResult::ContactsRebuilt {
            account_index: *account_index,
            result,
        },
        Awaited::SearchHitFetch {
            generation,
            message_id,
        } => BgResult::SearchHitFetched {
            generation: *generation,
            message_id: message_id.clone(),
            result: result.and_then(|settled| {
                settled["row_id"]
                    .as_i64()
                    .ok_or_else(|| "the fetch settled without naming a row".to_string())
            }),
        },
    }
}

/// The overlay's server leg, as the operation it is since P5-U10c.
///
/// `false` means no search started and the caller has already been told why,
/// which is what lets the overlay clear its spinner: a refusal at the call is
/// an account with no server or a query the grammar could not parse, and both
/// are sentences `start_operation` has already put on the status line.
///
/// The in-flight id is remembered on the `App`, because the hits arrive as
/// `message.server_hit` events carrying it and a fast retype leaves two
/// searches running: the generation counter cannot tell them apart, since the
/// events do not carry one.
pub(super) fn start_server_search(
    app: &mut App,
    commands: &dyn Queries,
    params: Value,
    generation: u64,
) -> bool {
    app.server_search_operation = start_operation_id(
        app,
        commands,
        "message.search_server",
        params,
        Awaited::ServerSearch { generation },
    );
    app.server_search_operation.is_some()
}

/// The overlay's `f`, as the operation it is since P5-U10c.
pub(super) fn start_hit_fetch(
    app: &mut App,
    commands: &dyn Queries,
    params: Value,
    generation: u64,
    message_id: String,
) {
    start_operation(
        app,
        commands,
        "message.fetch",
        params,
        Awaited::SearchHitFetch {
            generation,
            message_id,
        },
    );
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
    if !app.is_graph() && !app.has_imap() {
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
    if graph && !account.has_graph() {
        return;
    }
    if !graph && !account.has_imap() {
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

/// `send.draft` for one draft of the open account, behind the daemon's hold.
///
/// The approve and the two refusals ahead of it are the send key's (see
/// `actions::handle_action`); what is here is the call and the operation it
/// starts. `hold: true` and not a number of seconds: the window is
/// `email.send_hold_secs`, the daemon reads it, and a client that computed it
/// would be the hold living in the client again under a longer name.
pub(super) fn start_draft_send(
    app: &mut App,
    commands: &dyn Queries,
    account: &str,
    account_index: usize,
    id: &str,
) {
    start_operation(
        app,
        commands,
        "send.draft",
        json!({"account": account, "id": id, "hold": true}),
        Awaited::Send { account_index },
    );
}

/// `send.cancel_hold` for the countdown this client is showing.
///
/// The operation id and nothing else: a hold is addressed by the operation the
/// send answered with, which is what makes cancelling one from a window that
/// did not send it possible at all. A client with no countdown has no id to
/// name, so it makes no call.
fn cancel_hold(app: &mut App, commands: &dyn Queries) {
    let Some(operation) = app.hold.as_ref().map(|hold| hold.operation_id.clone()) else {
        return;
    };
    if let Err(e) = commands.call("send.cancel_hold", json!({"operation_id": operation})) {
        // The countdown stays up: the daemon refused to stop it, so the send
        // is still coming and saying otherwise would be a lie.
        app.set_status_level(format!("send.cancel_hold: {e:#}"), StatusLevel::Error);
    }
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
        app.is_graph(),
        app.has_smtp(),
    ) {
        Ok(graph) => graph,
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
        json!({"account": account, "hold": true}),
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
    if app.is_graph() {
        return app.set_status_level(
            "RSVP is not supported for Graph accounts yet (#0036)".to_string(),
            StatusLevel::Error,
        );
    }
    if !app.has_smtp() {
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

/// The Contacts view's `r`, as the operation it is since #0126.
///
/// The pre-daemon key handler walked every row of every mailbox on the UI
/// thread and rendered the verdict in place; the walk is
/// [`contact.rebuild`](../../daemon/methods/contact/index.html) now, and the
/// verdict arrives as [`BgResult::ContactsRebuilt`].
///
/// Nothing is put on the status line here. The pre-daemon rebuild showed no
/// line while it ran either, and the spinner `start_operation` bumps is what
/// says work is in flight.
fn rebuild_contacts(app: &mut App, commands: &dyn Queries) {
    let account = app.account_config.name.clone();
    let account_index = app.active_account;
    start_operation(
        app,
        commands,
        "contact.rebuild",
        json!({"account": account}),
        Awaited::ContactRebuild { account_index },
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

/// `send.draft`, as the line the send key has always posted: how many
/// recipients took it, and where the message actually is (#0037).
///
/// The three sentences are `actions::send_status_line`'s, built from the
/// `SendOutcome` the daemon settled with rather than from an engine report the
/// client no longer has. A send that reached nobody is an error and not a
/// green line: the message is parked in the outbox for a human.
pub fn sent_line(settled: &Value) -> Result<String, String> {
    let outcome: SendOutcome = match serde_json::from_value(settled.clone()) {
        Ok(outcome) => outcome,
        Err(e) => return Err(format!("the send outcome did not decode: {e}")),
    };
    let total = outcome.recipients.len();
    let failed: Vec<String> = outcome
        .recipients
        .iter()
        .filter(|recipient| !recipient.delivered)
        .map(|recipient| recipient.address.clone())
        .collect();
    if failed.is_empty() {
        Ok(format!(
            "Sent to {total} recipient(s) [{}]",
            outcome.status_line
        ))
    } else if failed.len() < total {
        Ok(format!(
            "Partial: {}/{total} succeeded -- failed: {} [{}]",
            total - failed.len(),
            failed.join(", "),
            outcome.status_line
        ))
    } else {
        Err(format!("Failed to send to all {total} recipient(s)"))
    }
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

/// The daemon's spelling of a [`MessageAddress`] (PERSO-155).
impl MessageAddress {
    /// The address keys of a `message.*` call: `row_id`, or `hit`.
    fn params(&self, account: &str) -> Value {
        let mut params = json!({"account": account});
        let (key, value) = self.source();
        params[key] = value;
        params
    }

    /// The address as one key and its value, which is also a draft's
    /// `source` object.
    fn source(&self) -> (&'static str, Value) {
        match self {
            MessageAddress::Row(row_id) => ("row_id", json!(row_id)),
            MessageAddress::Hit {
                message_id,
                mailbox,
            } => ("hit", json!({"message_id": message_id, "mailbox": mailbox})),
        }
    }
}

/// The attachments of one message, materialised into files a picker can name
/// (`RD-06`'s neighbour, `#0052` scope item 8).
///
/// Two calls rather than one: `message.get` for the part list the row carries,
/// then `message.materialise_attachment` per part. The files land in the
/// daemon's handle directory instead of the per-row materialisation directory
/// `mp open` writes to, which is the one visible consequence of the move: a
/// handle expires (ten minutes by default) where a materialised copy survived
/// until the temp directory was swept.
///
/// The real attachments come first and the body's inline images after them
/// (PERSO-165), each group in the row's own order; `part` stays the index in
/// the daemon's full list, which is what addresses it.
///
/// A server-only hit is listed and cut by the daemon from the server's copy,
/// with the same two calls and the same `part` order (PERSO-155).
///
/// An empty list is a message with no attachments, which is the caller's
/// status line to write; `Err` is a sentence for it to print.
pub(super) fn attachment_files(
    commands: &dyn Queries,
    account: &str,
    address: &MessageAddress,
) -> Result<crate::app::AttachmentFiles, String> {
    let mut get = address.params(account);
    get["body"] = json!(false);
    let shown = commands
        .call("message.get", get)
        .map_err(|e| format!("{e:#}"))?;
    let parts = shown["attachments"].as_array().cloned().unwrap_or_default();
    let is_inline = |part: &Value| part["inline"].as_bool().unwrap_or(false);
    let real: Vec<usize> = (0..parts.len())
        .filter(|&p| !is_inline(&parts[p]))
        .collect();
    let inline_from = real.len();
    let order = real
        .into_iter()
        .chain((0..parts.len()).filter(|&p| is_inline(&parts[p])));
    let files = order
        .map(|part| {
            let mut params = address.params(account);
            params["part"] = json!(part);
            commands
                .call("message.materialise_attachment", params)
                .map_err(|e| format!("{e:#}"))
                .and_then(|handle| {
                    handle["path"]
                        .as_str()
                        .map(PathBuf::from)
                        .ok_or_else(|| format!("part {part} was materialised without a path"))
                })
        })
        .collect::<Result<Vec<PathBuf>, String>>()?;
    Ok(crate::app::AttachmentFiles { files, inline_from })
}

/// One message as the RFC 822 file `mp export` writes, `<date>_<subject>.eml`,
/// materialised for the directory picker `te` opens (PERSO-174).
///
/// `message.materialise_eml` answers the stored raw message, or the server's
/// copy for a row stored without one (every Graph row) and for a server-only
/// hit, which it ingests nothing for, so this one can be a round trip. The
/// handle is left to its lifetime, as the parts `ts` saves are: the copy into
/// the picked directory reads the file after this returns.
pub(super) fn eml_file(
    commands: &dyn Queries,
    account: &str,
    address: &MessageAddress,
) -> Result<PathBuf, String> {
    let handle = commands
        .call("message.materialise_eml", address.params(account))
        .map_err(|e| format!("{e:#}"))?;
    handle["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "the message was materialised without a path".to_string())
}

/// The `FW:` subject the forward wizard opens with, built from the row's own
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
    mp_core::draft::fwd_subject(&subject)
}

/// The browser rendition of one row, as a file a `file://` URL can name.
///
/// `message.materialise_html` is the daemon's own copy of what the TUI built
/// inline: the charset meta, the CSP tag and the `cid:` inlining, all three of
/// them #0037 fixes that a second renderer would have had to keep in step. A
/// message whose sender wrote no markup is refused, which is not an error and
/// is the caller's "No HTML version available".
pub fn html_rendition(
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

/// The HTML part `send.draft` would send for one draft, as a browser
/// rendition (PERSO-136).
///
/// `draft.html` builds it with the send path's own renderer, signature and
/// quoted original included, and hardens it the way `message.html` is, so the
/// caller only writes it to a file. A draft has no handle method: the answer
/// is the string, inline.
pub fn draft_html(commands: &dyn Queries, account: &str, id: &str) -> Result<String, String> {
    let answer = commands
        .call(
            mp_protocol::draft::METHOD_DRAFT_HTML,
            json!({"account": account, "id": id}),
        )
        .map_err(|e| format!("{e:#}"))?;
    serde_json::from_value::<mp_protocol::draft::DraftHtml>(answer)
        .map(|rendition| rendition.html)
        .map_err(|e| format!("draft.html answered something else: {e}"))
}

/// One materialised handle: the id that releases it and the file it wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendition {
    /// The opaque handle, which is what `message.release_handle` takes.
    pub handle: String,
    /// The file the client opens.
    pub path: PathBuf,
}

/// The store's own Markdown view of one message, as a file `$EDITOR` can open
/// (`RD-06`, #0126, #0075); a server-only hit's is rendered by the daemon from
/// the server's copy in the same shape (PERSO-155).
///
/// `message.materialise_markdown` is the third member of the handle family and
/// writes exactly what the TUI used to build inline: `render_markdown` over
/// the row, at mode 0444, named after the subject. The handle comes back with
/// the path because the view is scratch and the caller owes it a release the
/// moment the editor exits; a handle nobody releases dies of its own lifetime
/// ten minutes later, which is the family's rule and not this caller's.
pub(super) fn markdown_rendition(
    commands: &dyn Queries,
    account: &str,
    address: &MessageAddress,
) -> Result<Rendition, String> {
    let handle = commands
        .call("message.materialise_markdown", address.params(account))
        .map_err(|e| format!("{e:#}"))?;
    match (handle["handle"].as_str(), handle["path"].as_str()) {
        (Some(id), Some(path)) => Ok(Rendition {
            handle: id.to_string(),
            path: PathBuf::from(path),
        }),
        _ => Err("the rendition was materialised without a handle".to_string()),
    }
}

/// Release one materialised handle, which unlinks the directory it wrote.
///
/// A failure is a log line and no more: an expired handle is already gone, and
/// there is nothing a user can do about either.
pub(super) fn release_rendition(commands: &dyn Queries, handle: &str) {
    if let Err(e) = commands.call("message.release_handle", json!({"handle": handle})) {
        log::warn!("[commands] releasing handle {handle}: {e:#}");
    }
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

/// The draft a reply or a forward of `address` writes, as its path and its
/// selector.
///
/// `draft.reply` and `draft.forward` are the whole of what the client used to
/// do in three steps (read the row, build the `SourceMessage`, run
/// `create_draft_from_source`), and the daemon runs them through the very same
/// library functions, so the file is byte-identical to the one `mp reply` and
/// `mp forward` write. `headers` is the compose wizard's override of the
/// recipients and the subject it asked for before the draft existed, and a
/// forward as attachment (`cF`) is `draft.forward`'s `as_attachment`, the
/// call `mp forward --as-attachment` makes (PERSO-174).
///
/// A forward takes a server-only hit as its source too, inline or as
/// attachment, which the daemon builds from the server's copy (PERSO-155);
/// a reply to one is [`draft_from_message`]'s.
pub(super) fn draft_from_source(
    commands: &dyn Queries,
    account: &str,
    address: &MessageAddress,
    kind: mp_core::draft::DraftFromSource,
    headers: Option<&mp_core::draft::DraftRecipientEdit>,
) -> Result<(PathBuf, String), String> {
    let (key, value) = address.source();
    let mut source = serde_json::Map::new();
    source.insert(key.to_string(), value);
    let mut params = json!({"account": account, "source": source});
    let method = match kind {
        mp_core::draft::DraftFromSource::Reply { all } => {
            params["all"] = json!(all);
            "draft.reply"
        }
        mp_core::draft::DraftFromSource::Forward { as_attachment } => {
            if as_attachment {
                params["as_attachment"] = json!(true);
            }
            "draft.forward"
        }
    };
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

/// The draft a reply or a reply-all to a message the client *holds* writes,
/// as its path and its selector (`DFT-08`, #0126).
///
/// `draft.create_from_message`, which is the one draft `draft.reply` cannot
/// build: every form of its `source` is an address into the store, and this is
/// the server-search hit that resolved to no local row. The message travels
/// instead of the address, and the daemon runs the same `mp_core::draft`
/// builder over it, so the file is the one the resolved hit's reply writes.
/// A forward of the same hit is [`draft_from_source`]'s, whose `hit` source
/// carries the server copy's parts (PERSO-155).
///
/// The payload is the envelope and the two body renditions the overlay is
/// already rendering, under the hit's own field names.
pub(super) fn draft_from_message(
    commands: &dyn Queries,
    account: &str,
    all: bool,
    fetched: &mp_core::parse::FetchedEmail,
) -> Result<(PathBuf, String), String> {
    let params = json!({
        "account": account,
        "kind": if all { "reply_all" } else { "reply" },
        "message": {
            "from": fetched.from,
            "to": fetched.to,
            "cc": fetched.cc,
            "reply_to": fetched.reply_to,
            "subject": fetched.subject,
            "message_id": fetched.message_id,
            "date_display": fetched.date,
            "body_text": fetched.body_text,
            "html_body": fetched.html_body,
        },
    });
    let created = commands
        .call("draft.create_from_message", params)
        .map_err(|e| format!("{e:#}"))?;
    let path = created["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "draft.create_from_message wrote a draft it did not name".to_string())?;
    let selector = created["selector"].as_str().unwrap_or_default().to_string();
    Ok((path, selector))
}

/// The rows of the local index that answer `query`, as the search overlay's
/// own row type (#0105).
///
/// `message.search` with `body: true`: the overlay renders a hit's body out of
/// its `fetched` payload, so the body travels with the row rather than costing
/// a `message.get` per hit. The query is rendered back into the grammar the
/// method parses ([`mp_core::search::to_query_string`]), which is what keeps the
/// AST off the wire.
/// One streamed `message.server_hit` as the overlay's row (`LST-08`, #0126).
///
/// The [`FetchedEmail`](mp_core::parse::FetchedEmail) is rebuilt here because
/// the overlay renders a hit out of one: the preview pane reads `body_text`,
/// the `b` key hands `html_body` to a browser, and a reply to a server-only
/// hit is built from it. The list row is then
/// `helpers::fetched_to_email_entry` exactly as it was, with the daemon's
/// resolution handed in rather than looked up in the store.
pub(super) fn hit_entry(
    hit: &mp_protocol::listing::ServerSearchHit,
) -> super::app::SearchResultEntry {
    let fetched = mp_core::parse::FetchedEmail {
        from: hit.from.clone(),
        to: hit.to.clone(),
        cc: hit.cc.clone(),
        reply_to: hit.reply_to.clone(),
        bcc: hit.bcc.clone(),
        subject: hit.subject.clone(),
        date: hit.date_display.clone(),
        body_text: hit.body_text.clone(),
        html_body: hit.html_body.clone(),
        has_attachments: hit.has_attachments,
        server_has_attachments: None,
        message_id: hit.message_id.clone(),
        attachments: Vec::new(),
        flags: mp_core::types::MessageFlags {
            seen: hit.flags.seen,
            answered: hit.flags.answered,
            forwarded: hit.flags.forwarded,
            flagged: hit.flags.flagged,
        },
        calendar_ics: None,
        event: None,
    };
    let entry = super::helpers::fetched_to_email_entry(
        hit.row_id.map(super::app::MessageRef::new),
        hit.selector.clone(),
        &fetched,
    );
    super::app::SearchResultEntry {
        entry,
        fetched,
        source_label: hit.mailbox.clone(),
    }
}

/// How many local hits the immediate FTS pass of the search overlay shows
/// before the server leg answers (#0105).
const LOCAL_SEARCH_LIMIT: usize = 50;

/// The overlay's search: the local pass, painted at once, then the server leg.
///
/// The server leg is `message.search_server` since P5-U10c (`LST-08`,
/// #0126): one durable operation on the daemon, which opens the session, keeps
/// the newest hits across the mailboxes (PERSO-175) and streams one
/// `message.server_hit` per hit. Its parameters are the desktop's for the same
/// search: `targets` of `None` names no mailbox, so the daemon runs its
/// account-wide search (one query across every folder on Graph) instead of a
/// query per mailbox the TUI happens to list, and the Message-IDs the local
/// pass is already showing travel as `exclude_message_ids` only when there
/// are some, so the dedup is the daemon's and the count it settles with is a
/// fact any client reproduces.
pub fn start_search(
    app: &mut App,
    commands: &dyn Queries,
    query: &mp_core::search::Query,
    targets: Option<&[SearchTarget]>,
    local_mailbox: Option<&str>,
) {
    // The account name travels with the search so each hit can be resolved
    // against that account's store (#0038).
    let account = app.account_config.name.clone();
    let generation = app.server_search_generation;

    // Local-first pass (#0105): the FTS index answers in milliseconds, so its
    // hits render before the server round trip starts. A query the local
    // grammar cannot lower (to:/cc:, mixed OR groups) skips the pass
    // silently; the server covers those terms.
    app.server_search_index = 0;
    app.server_search_scroll = 0;
    app.server_search_headers_scroll = 0;
    app.server_search_results =
        local_search(commands, &account, query, local_mailbox, LOCAL_SEARCH_LIMIT);
    let local_count = app.server_search_results.len();
    app.server_search_status = Some(if local_count > 0 {
        format!(
            "{local_count} local hit{}; searching server...",
            if local_count == 1 { "" } else { "s" }
        )
    } else {
        "Searching server...".to_string()
    });
    if local_count > 0 {
        app.server_search_focus = SearchOverlayFocus::List;
    }

    app.server_search_loading = true;
    let excluded: Vec<String> = app
        .server_search_results
        .iter()
        .filter_map(|hit| hit.fetched.message_id.clone())
        .collect();
    let mut params = json!({
        "account": account,
        "query": mp_core::search::to_query_string(query),
    });
    if let Some(targets) = targets {
        let mailboxes: Vec<&str> = targets
            .iter()
            .map(|target| target.server_name.as_str())
            .collect();
        params["mailboxes"] = json!(mailboxes);
    }
    if !excluded.is_empty() {
        params["exclude_message_ids"] = json!(excluded);
    }
    if !start_server_search(app, commands, params, generation) {
        app.server_search_loading = false;
    }
}

pub fn local_search(
    commands: &dyn Queries,
    account: &str,
    query: &mp_core::search::Query,
    mailbox: Option<&str>,
    limit: usize,
) -> Vec<super::app::SearchResultEntry> {
    let params = json!({
        "account": account,
        "query": mp_core::search::to_query_string(query),
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
    // daemon drain it about 1.5 s after the account's last mutation (#0133).
    // See the module header.
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
/// The daemon drains them to the server about 1.5 s after the account's last
/// mutation (#0133), or at a sync tick if one comes first, and rolls a refusal
/// back, so there is no per-op thread and no status to wait on:
/// the local move is instant and confirmed. `batch` says whether the selection
/// should be cleared afterwards, the only difference between the single and the
/// batch arm.
///
/// Returns the refs that were archived, or the error it put on the status
/// line when nothing was: the server-search overlay acts on the outcome and
/// shows the error on its own line.
pub(super) fn archive_msgs(
    app: &mut App,
    commands: &dyn Queries,
    msgs: Vec<MessageRef>,
    batch: bool,
) -> Result<Vec<MessageRef>, String> {
    let Some(dest_idx) = app.find_mailbox_by_kind(MailboxKind::Archive) else {
        let error = "Archive mailbox not configured".to_string();
        app.set_status_level(error.clone(), StatusLevel::Error);
        return Err(error);
    };
    let account = app.account_config.name.clone();
    let touched_invite = any_invite(app, &msgs);
    let archived_refs = mutate_each(commands, "message.archive", &account, &msgs, &[]);
    if archived_refs.is_empty() {
        let error = "Archive failed: nothing to archive".to_string();
        app.set_status_level(error.clone(), StatusLevel::Error);
        return Err(error);
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
    Ok(archived_refs)
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
    // more: the daemon resolves the folder behind a mailbox key from the
    // account's mailbox catalogue (#0141), and the sidebar's `server_name` was
    // a second copy of it that refused moves the daemon can name a folder for.

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
    app.set_rows_read(&flagged, read);
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
    app.set_rows_flagged(&starred, flagged);
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
pub fn mark_open_read(app: &mut App, commands: &dyn Queries, msg: MessageRef) -> bool {
    let Some(email) = app.emails.iter().find(|e| e.msg == Some(msg)) else {
        return false;
    };
    // A conversation header stands for its `latest`, whose own read state
    // is what an open of it changes (#0142).
    if email.header().map_or(email.read, |h| h.latest_read) {
        return false;
    }
    set_read_flag(app, commands, &[msg], true)
}

/// True when any of `msgs` is an invite row in the current list, read *before*
/// the mutation removes them.
pub fn any_invite(app: &App, msgs: &[MessageRef]) -> bool {
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
pub enum Flip {
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
pub fn status_flip(app: &mut App, commands: &dyn Queries, flip: Flip) {
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
pub fn status_flip_batch(app: &mut App, commands: &dyn Queries, ids: &[String], flip: Flip) {
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

/// Put the list, the sidebar counts and the cached Drafts mailbox back in step
/// with the files after a status flip.
///
/// The index refresh this used to open with is the daemon's since P5-U10d
/// (#0126): `draft.list` answers from a fresh scan of the directory and
/// `mailbox.list` refreshes the table it counts from, so the two reads below
/// already see the flip.
pub(super) fn refresh_drafts_after_flip(app: &mut App) {
    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }
    app.recount_all_mailboxes();
    app.reload_current_mailbox();
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use mp_core::draft::DraftFromSource;
    use serde_json::{json, Value};

    use super::{
        attachment_files, draft_from_message, draft_from_source, eml_file, markdown_rendition,
        MessageAddress,
    };
    use crate::queries::Queries;

    /// A door that records every call and answers each with `answer`.
    struct Recorder {
        answer: Value,
        calls: RefCell<Vec<(String, Value)>>,
    }

    impl Recorder {
        fn answering(answer: Value) -> Recorder {
            Recorder {
                answer,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Queries for Recorder {
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            self.calls.borrow_mut().push((method.to_string(), params));
            Ok(self.answer.clone())
        }
    }

    /// `cF` makes the call `mp forward --as-attachment` makes: `draft.forward`
    /// with `as_attachment`, which `cf` leaves out (PERSO-174).
    #[test]
    fn a_forward_as_attachment_is_draft_forward_with_as_attachment() {
        let door =
            Recorder::answering(json!({"path": "/d/fwd.md", "selector": "mp://alice/drafts/x"}));
        for as_attachment in [false, true] {
            let (path, selector) = draft_from_source(
                &door,
                "alice",
                &MessageAddress::Row(7),
                DraftFromSource::Forward { as_attachment },
                None,
            )
            .unwrap();
            assert_eq!(path.to_str(), Some("/d/fwd.md"));
            assert_eq!(selector, "mp://alice/drafts/x");
        }
        let calls = door.calls.borrow();
        assert_eq!(
            *calls,
            vec![
                (
                    "draft.forward".to_string(),
                    json!({"account": "alice", "source": {"row_id": 7}})
                ),
                (
                    "draft.forward".to_string(),
                    json!({"account": "alice", "source": {"row_id": 7}, "as_attachment": true})
                ),
            ]
        );
    }

    /// A hit with no row forwards through `draft.forward` with the hit as
    /// its source, inline with the wizard's headers or as attachment, which
    /// the daemon builds from the server's copy (PERSO-155).
    #[test]
    fn a_hit_with_no_row_forwards_through_its_message_id() {
        let door = Recorder::answering(json!({
            "path": "/d/fwd.md", "selector": "mp://alice/drafts/x",
        }));
        let hit = MessageAddress::Hit {
            message_id: "<hit@example.com>".to_string(),
            mailbox: "Archive".to_string(),
        };
        let headers = mp_core::draft::DraftRecipientEdit {
            to: "bob@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Fwd: x".to_string(),
        };
        for as_attachment in [false, true] {
            let (path, _) = draft_from_source(
                &door,
                "alice",
                &hit,
                DraftFromSource::Forward { as_attachment },
                Some(&headers),
            )
            .unwrap();
            assert_eq!(path.to_str(), Some("/d/fwd.md"));
        }
        let source = json!({"hit": {"message_id": "<hit@example.com>", "mailbox": "Archive"}});
        let headers = json!({"to": "bob@example.com", "cc": "", "bcc": "", "subject": "Fwd: x"});
        assert_eq!(
            *door.calls.borrow(),
            vec![
                (
                    "draft.forward".to_string(),
                    json!({"account": "alice", "source": source, "headers": headers}),
                ),
                (
                    "draft.forward".to_string(),
                    json!({
                        "account": "alice", "source": source, "headers": headers,
                        "as_attachment": true,
                    }),
                ),
            ]
        );
    }

    /// A reply to a hit with no row travels as the message the overlay holds.
    #[test]
    fn a_reply_to_a_hit_with_no_row_is_draft_create_from_message() {
        let door = Recorder::answering(json!({
            "path": "/d/re.md", "selector": "mp://alice/drafts/x",
        }));
        let fetched = mp_core::parse::parse_rfc822_to_fetched_email(
            b"Message-ID: <hit@example.com>\r\nSubject: x\r\n\r\nbody\r\n",
        )
        .expect("a message parses");
        draft_from_message(&door, "alice", true, &fetched).unwrap();
        let calls = door.calls.borrow();
        assert_eq!(calls[0].0, "draft.create_from_message");
        assert_eq!(calls[0].1["kind"], json!("reply_all"));
        assert_eq!(
            calls[0].1["message"]["message_id"],
            json!("<hit@example.com>")
        );
    }

    /// `o`, `O`, `e` and `y` on a server-only hit make the calls they make on
    /// a row, with the hit's address where the row id goes (PERSO-155).
    #[test]
    fn a_hits_attachments_and_view_are_the_rows_calls_with_its_address() {
        let door = Recorder::answering(json!({
            "handle": "h", "path": "/run/handles/h/x",
            "attachments": [{"name": "a.pdf", "size": 3}],
        }));
        let hit = MessageAddress::Hit {
            message_id: "<hit@example.com>".to_string(),
            mailbox: "Sent".to_string(),
        };
        let files = attachment_files(&door, "alice", &hit).unwrap();
        assert_eq!(files.files.len(), 1);
        markdown_rendition(&door, "alice", &hit).unwrap();
        let address = json!({"message_id": "<hit@example.com>", "mailbox": "Sent"});
        assert_eq!(
            *door.calls.borrow(),
            vec![
                (
                    "message.get".to_string(),
                    json!({"account": "alice", "hit": address, "body": false}),
                ),
                (
                    "message.materialise_attachment".to_string(),
                    json!({"account": "alice", "hit": address, "part": 0}),
                ),
                (
                    "message.materialise_markdown".to_string(),
                    json!({"account": "alice", "hit": address}),
                ),
            ]
        );
    }

    /// `te` reads the file `message.materialise_eml` wrote, as `mp export`.
    #[test]
    fn the_eml_is_the_file_message_materialise_eml_wrote() {
        let door = Recorder::answering(json!({
            "handle": "h1", "path": "/run/handles/h1/2024-01-01_Report.eml",
            "name": "2024-01-01_Report.eml", "bytes": 10, "expires_at": "x",
        }));
        let path = eml_file(&door, "alice", &MessageAddress::Row(7)).unwrap();
        assert_eq!(path.to_str(), Some("/run/handles/h1/2024-01-01_Report.eml"));
        assert_eq!(
            *door.calls.borrow(),
            vec![(
                "message.materialise_eml".to_string(),
                json!({"account": "alice", "row_id": 7})
            )]
        );
        let pathless = Recorder::answering(json!({}));
        assert!(eml_file(&pathless, "alice", &MessageAddress::Row(7)).is_err());

        // The overlay's `te` on a server-only hit addresses it as
        // `message.fetch` does (PERSO-174).
        let hit = Recorder::answering(json!({
            "handle": "h2", "path": "/run/handles/h2/2025-03-03_Old.eml",
            "name": "2025-03-03_Old.eml", "bytes": 10, "expires_at": "x",
        }));
        let address = MessageAddress::Hit {
            message_id: "<old@example.com>".to_string(),
            mailbox: "Archive".to_string(),
        };
        let path = eml_file(&hit, "alice", &address).unwrap();
        assert_eq!(path.to_str(), Some("/run/handles/h2/2025-03-03_Old.eml"));
        assert_eq!(
            *hit.calls.borrow(),
            vec![(
                "message.materialise_eml".to_string(),
                json!({
                    "account": "alice",
                    "hit": {"message_id": "<old@example.com>", "mailbox": "Archive"},
                })
            )]
        );
    }
}
