//! The Tauri commands: narrow, typed, and the only way the frontend reaches
//! the daemon. The frontend never sends a method string; each command names
//! the one method it calls, with its own budget.
//!
//! Every argument and every field is snake_case on the wire (the commands use
//! `rename_all = "snake_case"`), matching the protocol types the results
//! embed. Every command answers `Result<T, GuiError>`.
//!
//! The bodies are plain functions over a [`Door`] (the `*_on` functions), so
//! the tests run them over the fixture; the `#[tauri::command]` wrappers only
//! fetch the door and move the blocking call off the async runtime.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

use mp_client::queries;
use mp_core::draft::DraftRecipientEdit;
use mp_core::selector::DRAFTS_MAILBOX;
use mp_protocol::draft::{
    DraftCreated, DraftKind, DraftListing, DraftLocation, DraftMessage, DraftPreview,
    DraftValidation,
};
use mp_protocol::events::{Diagnostic, DraftInvalid};
use mp_protocol::listing::MessageListRow;
use mp_protocol::send::{HoldListing, OutboxListing};
use mp_protocol::state::{AccountState, Bootstrap, OutboxCounts, SyncHealthState};
use mp_protocol::{PROTOCOL_MAX, PROTOCOL_MIN};

use crate::connector;
use crate::error::{Addressing, GuiError};
use crate::navigation::InterceptLog;
use crate::reader;
use crate::session::{
    Budgeted, ConnectionStatus, Door, GuiEvent, InterceptSource, InterceptedUrl, PendingKind,
    SessionHandle, CONNECT_WAIT,
};

const LIST_BUDGET: Duration = Duration::from_secs(15);
const MESSAGE_BUDGET: Duration = Duration::from_secs(10);
const SEARCH_BUDGET: Duration = Duration::from_secs(20);
const START_BUDGET: Duration = Duration::from_secs(10);
const CANCEL_BUDGET: Duration = Duration::from_secs(5);
/// One message of a mutation batch: a local commit, no server round trip.
const MUTATION_BUDGET: Duration = Duration::from_secs(10);
const HOLD_BUDGET: Duration = Duration::from_secs(5);
/// A draft written from a stored message reads the store and, for a forward,
/// materialises the original attachments.
const DRAFT_BUDGET: Duration = Duration::from_secs(20);
/// A draft query, or one status line rewritten.
const DRAFT_QUERY_BUDGET: Duration = Duration::from_secs(10);

/// `MP_DESKTOP_STUB_OPENER=1`: `open_external` records instead of opening.
pub const STUB_OPENER_ENV: &str = "MP_DESKTOP_STUB_OPENER";

fn call(
    door: &Door,
    method: &str,
    params: Value,
    budget: Duration,
    how: Addressing,
) -> Result<Value, GuiError> {
    door.call_within(method, params, budget)
        .map_err(|e| GuiError::from_call(&e, how))
}

/// Run a blocking body off the async runtime.
async fn blocking<T, F>(f: F) -> Result<T, GuiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, GuiError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| GuiError::internal(format!("the command task failed: {e}")))?
}

/// Fetch the door, then run `f` with it off the async runtime.
async fn with_door<T, F>(session: &SessionHandle, f: F) -> Result<T, GuiError>
where
    T: Send + 'static,
    F: FnOnce(&SessionHandle, &Door) -> Result<T, GuiError> + Send + 'static,
{
    let session = session.clone();
    blocking(move || {
        let door = session.door(CONNECT_WAIT)?;
        f(&session, &door)
    })
    .await
}

// ---------------------------------------------------------------------------
// Result shapes
// ---------------------------------------------------------------------------

/// One configured account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct AccountInfo {
    pub name: String,
    /// The first configured account.
    pub default: bool,
    /// `imap` or `graph`.
    pub backend: String,
    /// `account.list`'s store state: `ready` or `blocked`.
    pub store_state: String,
    /// The runtime state at the last bootstrap (live changes arrive as
    /// `account.state_changed` events).
    pub runtime_state: AccountState,
    /// The last completed sync at the last bootstrap (live changes arrive as
    /// `sync.completed` events).
    pub sync_health: SyncHealthState,
    pub outbox: OutboxCounts,
}

/// What a sidebar row is, from its role (the TUI's `MailboxKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum MailboxKind {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Extra,
}

impl MailboxKind {
    fn from_role(role: &str) -> MailboxKind {
        match role {
            "inbox" => MailboxKind::Inbox,
            "drafts" => MailboxKind::Drafts,
            "sent" => MailboxKind::Sent,
            "archive" => MailboxKind::Archive,
            _ => MailboxKind::Extra,
        }
    }
}

/// One sidebar mailbox: the TUI's `MailboxInfo` projection plus its counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MailboxInfo {
    /// The store key `list_messages` takes.
    pub slug: String,
    pub label: String,
    pub role: String,
    pub kind: MailboxKind,
    pub total: u64,
    pub unread: u64,
    pub badge: u64,
}

/// One account's sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MailboxListing {
    pub account: String,
    pub mailboxes: Vec<MailboxInfo>,
    /// Sums over the mailboxes, Drafts excluded from `unread` (it has none).
    pub total: u64,
    pub unread: u64,
    pub runtime_state: AccountState,
    pub sync_health: SyncHealthState,
}

/// A mailbox's contents: messages, or the Drafts listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageList {
    Messages {
        account: String,
        /// The resolved mailbox slug.
        mailbox: String,
        total: u64,
        rows: Vec<MessageListRow>,
    },
    Drafts {
        account: String,
        listing: DraftListing,
    },
}

/// The stored plain text of one message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MessageText {
    pub account: String,
    pub row_id: i64,
    /// `None` when the store holds no readable body.
    pub body: Option<String>,
}

/// One attachment of a message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct Attachment {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// The headers of one message (the `message.get` record without its body)
/// and the reader URL its HTML is served at.
///
/// A header the message did not carry is `null` on the wire
/// (`read_cmd::ShownMessage`), which `#[serde(default)]` alone does not
/// accept, so every header is an `Option`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MessageMeta {
    #[serde(default)]
    pub row_id: i64,
    /// `mpmsg://localhost/<account>/<row_id>`, for the reader iframe.
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub mailbox: String,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// The `Date:` header as stored.
    #[serde(default)]
    pub date: Option<String>,
    /// `read`, `answered`, `flagged`, ... as `mp show --json` prints them.
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub invite: bool,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// `message.search`'s params, mirroring `mp search --local`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct LocalSearchParams {
    pub account: String,
    #[serde(default)]
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The `--body` filter (`body` on the wire is the "send bodies" switch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// One local hit: a listing row plus the mailbox it was found in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct LocalSearchHit {
    pub mailbox: String,
    #[serde(flatten)]
    pub row: MessageListRow,
}

/// `message.search_server`'s params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ServerSearchParams {
    pub account: String,
    pub query: String,
    /// Sidebar labels or server names; every mailbox with a server name when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailboxes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// The Message-IDs the local pass already shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_message_ids: Option<Vec<String>>,
}

/// A started server search. Hits arrive as `message.server_hit` events and
/// the end as `operation.finished`, both carrying this id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct OperationStarted {
    pub operation_id: String,
}

/// What a cancel came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    /// It had already finished; nothing to cancel.
    AlreadySettled,
}

/// Where a move or an archive put the message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MovedTo {
    pub mailbox: String,
    pub selector: String,
}

/// The daemon's answer to one message mutation (`message.archive`,
/// `message.delete`, `message.move`, `message.set_flag`, `message.set_read`),
/// which the daemon builds inline, plus the `row_id` the GUI named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MutationAck {
    /// The row the call named; not on the wire.
    #[serde(default)]
    pub row_id: i64,
    pub account: String,
    /// `<mailbox>/<uid>` before the mutation.
    pub id: String,
    /// The selector before the mutation.
    pub selector: String,
    /// The mailbox the message was in.
    pub mailbox: String,
    /// Archive and move only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<MovedTo>,
    /// `message.set_read` only: the state set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
    /// `message.set_flag` only: the state set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged: Option<bool>,
}

/// One message of a batch the daemon refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MutationFailure {
    pub row_id: i64,
    pub error: GuiError,
}

/// What a mutation over a list of rows came to, one call per row in the
/// list's order, as the TUI does it. A row the daemon refused (gone, or
/// never there) is in `failed` and the rest go ahead.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct MutationBatch {
    pub done: Vec<MutationAck>,
    pub failed: Vec<MutationFailure>,
}

/// The daemon's answer to one `draft.discard`, which it builds inline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftDiscarded {
    pub account: String,
    pub id: String,
    pub selector: String,
    /// The status the draft was in (`draft`, `approved`, ...).
    pub status: String,
}

/// One draft of a batch the daemon refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftDiscardFailure {
    pub id: String,
    pub error: GuiError,
}

/// What a `draft.discard` over a list of ids came to, one call per id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftDiscardBatch {
    pub done: Vec<DraftDiscarded>,
    pub failed: Vec<DraftDiscardFailure>,
}

/// The compose wizard's recipients and subject, which replace what the
/// builder derived. An empty string clears the field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftHeaders {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
}

/// A recipient field as the TUI's wizard submits it: trimmed, and without the
/// trailing separators an autocompleted address leaves.
fn recipient_field(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
        .to_string()
}

impl DraftHeaders {
    fn edit(&self) -> DraftRecipientEdit {
        DraftRecipientEdit {
            to: recipient_field(&self.to),
            cc: recipient_field(&self.cc),
            bcc: recipient_field(&self.bcc),
            subject: self.subject.trim().to_string(),
        }
    }

    fn wire(&self) -> Value {
        let edit = self.edit();
        json!({"to": edit.to, "cc": edit.cc, "bcc": edit.bcc, "subject": edit.subject})
    }
}

/// The daemon's answer to one `draft.approve` or `draft.demote`, which it
/// builds inline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftStatusChanged {
    pub account: String,
    pub id: String,
    /// The status now: `approved` or `draft`.
    pub status: String,
    pub path: String,
}

/// One draft of an approve or demote batch the daemon refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftStatusFailure {
    pub id: String,
    pub error: GuiError,
    /// For a file that does not parse (`-32010` `draft_invalid`): the file
    /// and why, as `draft.invalid` carries them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<DraftInvalid>,
}

/// What `draft.approve` or `draft.demote` over a list of ids came to, one call
/// per id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftStatusBatch {
    pub done: Vec<DraftStatusChanged>,
    pub failed: Vec<DraftStatusFailure>,
}

/// The signatures a draft can carry, and the account's default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SignatureListing {
    pub account: String,
    /// The signature names, sorted.
    pub names: Vec<String>,
    /// The account's default, `null` when it has none.
    pub default: Option<String>,
}

/// The daemon's answer to `send.cancel_hold`, which it builds inline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct HoldCancelled {
    pub cancelled: bool,
    pub operation_id: String,
    /// The revision the cancel moved the daemon to.
    pub revision: u64,
}

/// A send `send_draft` or `send_approved` started. It ends with the
/// operation's `operation.finished`, `operation_settled` or
/// `operation_dropped` (kind `send` or `send_approved`), whose `result` is
/// a `SendOutcome` or an `ApprovedOutcome`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SendStarted {
    pub operation_id: String,
    /// Whether the daemon armed its undo hold; `false` when the call asked
    /// for none or `email.send_hold_secs` is 0, and then no `send.hold_*`
    /// event follows.
    pub held: bool,
    /// `send_draft` approved the draft first, which a cancelled or failed
    /// send leaves approved.
    pub approved: bool,
}

/// The daemon's answer to `send.outbox_discard`, which it builds inline:
/// the row and the message it carried, so the client can name what it
/// dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct OutboxDiscarded {
    pub discarded: bool,
    pub row_id: i64,
    pub message_id: String,
    /// The revision the discard moved the daemon to.
    pub revision: u64,
}

/// Why `send_draft` did not start a send: a `GuiError`, and for a draft
/// whose file does not parse (`-32010` `draft_invalid`), the file and why,
/// as `draft.invalid` carries them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SendRefusal {
    #[serde(flatten)]
    #[cfg_attr(test, ts(flatten))]
    pub error: GuiError,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub invalid: Option<Box<DraftInvalid>>,
}

impl From<GuiError> for SendRefusal {
    fn from(error: GuiError) -> SendRefusal {
        SendRefusal {
            error,
            invalid: None,
        }
    }
}

/// Which sync pass `sync_trigger` starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// `sync.quick`: the newest messages of each mailbox.
    Quick,
    /// `sync.full`: every message.
    Full,
}

impl SyncMode {
    fn method(self) -> &'static str {
        match self {
            SyncMode::Quick => "sync.quick",
            SyncMode::Full => "sync.full",
        }
    }
}

/// Versions on both sides of the socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct VersionInfo {
    pub app_version: String,
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub daemon: Option<connector::Hello>,
    pub fixture: bool,
}

// ---------------------------------------------------------------------------
// Bodies over a door
// ---------------------------------------------------------------------------

pub fn list_accounts_on(
    door: &Door,
    snapshot: Option<&Bootstrap>,
) -> Result<Vec<AccountInfo>, GuiError> {
    let answer = call(
        door,
        "account.list",
        json!({}),
        LIST_BUDGET,
        Addressing::Params,
    )?;
    let rows = answer["accounts"].as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .map(|row| {
            let name = row["name"].as_str().unwrap_or_default().to_string();
            let snap = snapshot.and_then(|b| b.snapshot.accounts.iter().find(|a| a.name == name));
            AccountInfo {
                default: row["default"].as_bool().unwrap_or(false),
                backend: row["backend"].as_str().unwrap_or_default().to_string(),
                store_state: row["state"].as_str().unwrap_or_default().to_string(),
                runtime_state: snap.map(|a| a.state).unwrap_or_default(),
                sync_health: snap.map(|a| a.sync_health.state).unwrap_or_default(),
                outbox: snapshot
                    .map(|b| b.snapshot.outbox_of(&name))
                    .unwrap_or_default(),
                name,
            }
        })
        .collect())
}

pub fn list_mailboxes_on(
    door: &Door,
    snapshot: Option<&Bootstrap>,
    account: &str,
) -> Result<MailboxListing, GuiError> {
    let q = Budgeted {
        door,
        budget: LIST_BUDGET,
    };
    let rows = queries::mailbox_rows(&q, account)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?;
    let mailboxes: Vec<MailboxInfo> = rows
        .into_iter()
        .map(|row| MailboxInfo {
            kind: MailboxKind::from_role(&row.role),
            slug: row.slug,
            label: row.label,
            role: row.role,
            total: row.total,
            unread: row.unread,
            badge: row.badge,
        })
        .collect();
    let snap = snapshot.and_then(|b| b.snapshot.accounts.iter().find(|a| a.name == account));
    Ok(MailboxListing {
        account: account.to_string(),
        total: mailboxes.iter().map(|m| m.total).sum(),
        unread: mailboxes.iter().map(|m| m.unread).sum(),
        runtime_state: snap.map(|a| a.state).unwrap_or_default(),
        sync_health: snap.map(|a| a.sync_health.state).unwrap_or_default(),
        mailboxes,
    })
}

pub fn list_messages_on(
    door: &Door,
    account: &str,
    mailbox: &str,
) -> Result<MessageList, GuiError> {
    let q = Budgeted {
        door,
        budget: LIST_BUDGET,
    };
    if mailbox.eq_ignore_ascii_case(DRAFTS_MAILBOX) {
        let listing = queries::list_drafts(&q, account)
            .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?;
        return Ok(MessageList::Drafts {
            account: account.to_string(),
            listing,
        });
    }
    let (method, params) = queries::message_list_request(account, mailbox);
    let answer = call(door, method, params, LIST_BUDGET, Addressing::Resource)?;
    let rows = queries::decode_message_rows(&answer);
    Ok(MessageList::Messages {
        account: account.to_string(),
        mailbox: answer["mailbox"].as_str().unwrap_or(mailbox).to_string(),
        total: answer["total"].as_u64().unwrap_or(rows.len() as u64),
        rows,
    })
}

pub fn message_text_on(door: &Door, account: &str, row_id: i64) -> Result<MessageText, GuiError> {
    let q = Budgeted {
        door,
        budget: MESSAGE_BUDGET,
    };
    let body = queries::message_body(&q, account, row_id)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?;
    Ok(MessageText {
        account: account.to_string(),
        row_id,
        body,
    })
}

pub fn message_html_meta_on(
    door: &Door,
    account: &str,
    row_id: i64,
) -> Result<MessageMeta, GuiError> {
    let answer = call(
        door,
        "message.get",
        json!({"account": account, "row_id": row_id, "body": false}),
        MESSAGE_BUDGET,
        Addressing::Resource,
    )?;
    let mut meta: MessageMeta = serde_json::from_value(answer)
        .map_err(|e| GuiError::protocol(format!("message.get did not decode: {e}")))?;
    meta.row_id = row_id;
    meta.html_url = reader::message_url(account, row_id);
    Ok(meta)
}

pub fn search_local_on(
    door: &Door,
    params: &LocalSearchParams,
) -> Result<Vec<LocalSearchHit>, GuiError> {
    let mut wire = serde_json::to_value(params).map_err(GuiError::internal)?;
    wire["body"] = json!(false);
    let answer = call(
        door,
        "message.search",
        wire,
        SEARCH_BUDGET,
        Addressing::Params,
    )?;
    Ok(queries::decode_search_hits(&answer)
        .into_iter()
        .map(|hit| LocalSearchHit {
            mailbox: hit.mailbox,
            row: hit.row,
        })
        .collect())
}

pub fn search_server_start_on(
    session: &SessionHandle,
    door: &Door,
    params: &ServerSearchParams,
) -> Result<OperationStarted, GuiError> {
    let wire = serde_json::to_value(params).map_err(GuiError::internal)?;
    let operation_id = session.start_operation(
        door,
        "message.search_server",
        wire,
        PendingKind::ServerSearch,
        START_BUDGET,
    )?;
    Ok(OperationStarted { operation_id })
}

pub fn search_server_cancel_on(
    session: &SessionHandle,
    door: &Door,
    operation_id: &str,
) -> Result<CancelOutcome, GuiError> {
    let result = call(
        door,
        "operation.cancel",
        json!({"operation_id": operation_id}),
        CANCEL_BUDGET,
        Addressing::Resource,
    );
    session.forget_operation(operation_id);
    match result {
        Ok(_) => Ok(CancelOutcome::Cancelled),
        // Already finished, or forgotten: either way it is not running.
        Err(GuiError::NotFound { .. }) => Ok(CancelOutcome::AlreadySettled),
        Err(e) => Err(e),
    }
}

/// A refusal that is about one row of a batch (it is gone, or a parameter
/// the row made wrong), after which the next row still goes. Anything else
/// (no daemon, an unknown account, a timeout) is about the whole batch.
fn about_one_row(error: &GuiError) -> bool {
    // Every per-row refusal of the daemon's `message.*` mutations and of
    // `draft.discard` is `-32602` (no such row, a destination it cannot move
    // to, an approved draft), which a resource call reads as `NotFound`. An
    // account that is not ready (`-32006`) or a store failure (`-32603`) is
    // about the whole batch.
    matches!(
        error,
        GuiError::NotFound {
            code: Some(-32602),
            ..
        }
    )
}

/// What a batch came to: the answers, and the items refused with why.
type Outcomes<I, T> = (Vec<T>, Vec<(I, GuiError)>);

/// Run `each` over `items` in order, one call per item.
///
/// A per-row refusal lands in `failed` and the batch goes on. An error about
/// the whole batch stops it: it is the command's error when nothing was done
/// yet, and otherwise the error of every item not done, so the answer still
/// says which rows changed.
fn each_of<I: Clone, T>(
    items: &[I],
    each: impl FnMut(&I) -> Result<T, GuiError>,
) -> Result<Outcomes<I, T>, GuiError> {
    each_of_by(items, about_one_row, each)
}

/// [`each_of`], with `per_row` deciding which refusals are about one row.
fn each_of_by<I: Clone, T>(
    items: &[I],
    per_row: fn(&GuiError) -> bool,
    mut each: impl FnMut(&I) -> Result<T, GuiError>,
) -> Result<Outcomes<I, T>, GuiError> {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match each(item) {
            Ok(answer) => done.push(answer),
            Err(e) if per_row(&e) => failed.push((item.clone(), e)),
            Err(e) if done.is_empty() => return Err(e),
            Err(e) => {
                failed.extend(items[i..].iter().map(|rest| (rest.clone(), e.clone())));
                break;
            }
        }
    }
    Ok((done, failed))
}

/// One message mutation over `row_ids`, always `settle: false`: the daemon
/// commits each row and its owed server op and drains them once the
/// account's mutations have been quiet for 1.5 s (#0133).
fn mutate_on(
    door: &Door,
    method: &str,
    account: &str,
    row_ids: &[i64],
    extra: &[(&str, Value)],
) -> Result<MutationBatch, GuiError> {
    let (done, failed) = each_of(row_ids, |&row_id| {
        let mut params = json!({"account": account, "row_id": row_id, "settle": false});
        for (key, value) in extra {
            params[*key] = value.clone();
        }
        let answer = call(door, method, params, MUTATION_BUDGET, Addressing::Resource)?;
        let mut ack: MutationAck = serde_json::from_value(answer)
            .map_err(|e| GuiError::protocol(format!("{method} did not decode: {e}")))?;
        ack.row_id = row_id;
        Ok(ack)
    })?;
    Ok(MutationBatch {
        done,
        failed: failed
            .into_iter()
            .map(|(row_id, error)| MutationFailure { row_id, error })
            .collect(),
    })
}

pub fn message_archive_on(
    door: &Door,
    account: &str,
    row_ids: &[i64],
) -> Result<MutationBatch, GuiError> {
    mutate_on(door, "message.archive", account, row_ids, &[])
}

pub fn message_delete_on(
    door: &Door,
    account: &str,
    row_ids: &[i64],
) -> Result<MutationBatch, GuiError> {
    mutate_on(door, "message.delete", account, row_ids, &[])
}

pub fn message_move_on(
    door: &Door,
    account: &str,
    row_ids: &[i64],
    destination: &str,
) -> Result<MutationBatch, GuiError> {
    mutate_on(
        door,
        "message.move",
        account,
        row_ids,
        &[("destination", json!(destination))],
    )
}

pub fn message_set_flag_on(
    door: &Door,
    account: &str,
    row_ids: &[i64],
    flagged: bool,
) -> Result<MutationBatch, GuiError> {
    mutate_on(
        door,
        "message.set_flag",
        account,
        row_ids,
        &[("flagged", json!(flagged))],
    )
}

pub fn message_set_read_on(
    door: &Door,
    account: &str,
    row_ids: &[i64],
    read: bool,
) -> Result<MutationBatch, GuiError> {
    mutate_on(
        door,
        "message.set_read",
        account,
        row_ids,
        &[("read", json!(read))],
    )
}

/// `draft.discard` over `ids`, never forced: an approved draft is refused,
/// the way the TUI refuses it.
pub fn draft_discard_on(
    door: &Door,
    account: &str,
    ids: &[String],
) -> Result<DraftDiscardBatch, GuiError> {
    let (done, failed) = each_of(ids, |id| {
        let answer = call(
            door,
            "draft.discard",
            json!({"account": account, "id": id}),
            MUTATION_BUDGET,
            Addressing::Resource,
        )?;
        serde_json::from_value::<DraftDiscarded>(answer)
            .map_err(|e| GuiError::protocol(format!("draft.discard did not decode: {e}")))
    })?;
    Ok(DraftDiscardBatch {
        done,
        failed: failed
            .into_iter()
            .map(|(id, error)| DraftDiscardFailure { id, error })
            .collect(),
    })
}

/// Decode a daemon answer into `T`.
fn decode<T: serde::de::DeserializeOwned>(method: &str, answer: Value) -> Result<T, GuiError> {
    serde_json::from_value(answer)
        .map_err(|e| GuiError::protocol(format!("{method} did not decode: {e}")))
}

/// Tell a fixture door that a client wrote a draft file, which the daemon's
/// watcher would notice on its own.
fn written(door: &Door, path: &str) {
    if let Door::Fixture(fixture) = door {
        fixture.file_written(std::path::Path::new(path));
    }
}

/// Rewrite the recipients and the subject of the draft file at `path` in
/// place, through the helper the TUI's `ce` uses: the body and every other
/// frontmatter field stay byte for byte.
fn rewrite_recipients(door: &Door, path: &str, edit: &DraftRecipientEdit) -> Result<(), GuiError> {
    mp_core::draft::rewrite_draft_recipients(std::path::Path::new(path), edit).map_err(|e| {
        GuiError::protocol(format!("could not rewrite the recipients of {path}: {e:#}"))
    })?;
    written(door, path);
    Ok(())
}

/// `draft.create`: a skeleton named `name` in the account's drafts
/// directory. A name already taken is the daemon's `-32602`, a `protocol`
/// error whose message names the path. `headers` are written into the new
/// file client-side, since `draft.create` takes none.
pub fn draft_create_on(
    door: &Door,
    account: &str,
    name: &str,
    signature: Option<&str>,
    no_signature: bool,
    headers: Option<&DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    let mut params = json!({"account": account, "name": name});
    if let Some(signature) = signature {
        params["signature"] = json!(signature);
    }
    if no_signature {
        params["no_signature"] = json!(true);
    }
    let answer = call(
        door,
        "draft.create",
        params,
        DRAFT_QUERY_BUDGET,
        Addressing::Params,
    )?;
    let created: DraftCreated = decode("draft.create", answer)?;
    if let Some(headers) = headers {
        rewrite_recipients(door, &created.path, &headers.edit())?;
    }
    Ok(created)
}

/// `draft.reply` or `draft.forward` of the stored message `row_id`.
fn draft_from_row_on(
    door: &Door,
    method: &str,
    account: &str,
    row_id: i64,
    all: Option<bool>,
    headers: Option<&DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    let mut params = json!({"account": account, "source": {"row_id": row_id}});
    if let Some(all) = all {
        params["all"] = json!(all);
    }
    if let Some(headers) = headers {
        params["headers"] = headers.wire();
    }
    let answer = call(door, method, params, DRAFT_BUDGET, Addressing::Resource)?;
    decode(method, answer)
}

/// A reply (or, with `all`, a reply-all) to the stored message `row_id`.
pub fn draft_reply_on(
    door: &Door,
    account: &str,
    row_id: i64,
    all: bool,
    headers: Option<&DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    draft_from_row_on(door, "draft.reply", account, row_id, Some(all), headers)
}

/// A forward of the stored message `row_id`, carrying its attachments.
pub fn draft_forward_on(
    door: &Door,
    account: &str,
    row_id: i64,
    headers: Option<&DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    draft_from_row_on(door, "draft.forward", account, row_id, None, headers)
}

/// A reply, reply-all or forward of a message the store holds no row for (a
/// server-only search hit), built from the hit itself. It carries no
/// attachments.
pub fn draft_from_message_on(
    door: &Door,
    account: &str,
    kind: DraftKind,
    message: &DraftMessage,
) -> Result<DraftCreated, GuiError> {
    let params = json!({"account": account, "kind": kind, "message": message});
    let answer = call(
        door,
        "draft.create_from_message",
        params,
        DRAFT_BUDGET,
        Addressing::Params,
    )?;
    decode("draft.create_from_message", answer)
}

/// Where the draft `id` is now, from a fresh scan of the drafts directory.
pub fn draft_path_on(door: &Door, account: &str, id: &str) -> Result<DraftLocation, GuiError> {
    let answer = call(
        door,
        "draft.path",
        json!({"account": account, "id": id}),
        DRAFT_QUERY_BUDGET,
        Addressing::Resource,
    )?;
    decode("draft.path", answer)
}

/// One draft's diagnostics: an invalid draft is an answer, not a refusal.
pub fn draft_validate_on(
    door: &Door,
    account: &str,
    id: &str,
) -> Result<DraftValidation, GuiError> {
    let answer = call(
        door,
        "draft.validate",
        json!({"account": account, "id": id}),
        DRAFT_QUERY_BUDGET,
        Addressing::Resource,
    )?;
    decode("draft.validate", answer)
}

/// What a send of the draft `id` would send.
pub fn draft_preview_on(door: &Door, account: &str, id: &str) -> Result<DraftPreview, GuiError> {
    let answer = call(
        door,
        "draft.preview",
        json!({"account": account, "id": id}),
        DRAFT_QUERY_BUDGET,
        Addressing::Resource,
    )?;
    decode("draft.preview", answer)
}

/// A per-draft refusal of `draft.approve` or `draft.demote`: an id nothing
/// resolves to or a sent draft (`-32602`), or a file that does not parse
/// (`-32010`).
fn about_one_draft(error: &GuiError) -> bool {
    about_one_row(error)
        || matches!(
            error,
            GuiError::Protocol {
                code: Some(-32010),
                ..
            }
        )
}

/// The daemon's own sentence out of a refusal's text.
fn refusal_message(text: &str) -> &str {
    const MARK: &str = "the daemon refused the call: ";
    let tail = text.find(MARK).map_or(text, |at| &text[at + MARK.len()..]);
    tail.rfind(" (").map_or(tail, |at| &tail[..at])
}

/// The `draft.invalid` payload of a `-32010` refusal. The session keeps the
/// refusal's text but not its `data`, so the path comes from the listing's
/// skipped files, where an unparseable draft sits under its file stem.
fn invalid_payload(door: &Door, account: &str, id: &str, error: &GuiError) -> Option<DraftInvalid> {
    if !matches!(
        error,
        GuiError::Protocol {
            code: Some(-32010),
            ..
        }
    ) {
        return None;
    }
    let listing = call(
        door,
        "draft.list",
        json!({"account": account}),
        DRAFT_QUERY_BUDGET,
        Addressing::Resource,
    )
    .ok()
    .and_then(|answer| decode::<DraftListing>("draft.list", answer).ok())?;
    let skip = listing.skipped.into_iter().find(|skip| {
        std::path::Path::new(&skip.path)
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy() == id)
    })?;
    Some(DraftInvalid {
        account: account.to_string(),
        id: id.to_string(),
        path: skip.path,
        diagnostics: vec![Diagnostic {
            line: None,
            message: refusal_message(error.message()).to_string(),
        }],
    })
}

/// `draft.approve` or `draft.demote` over `ids`, one call per id in order.
fn draft_status_on(
    door: &Door,
    method: &str,
    account: &str,
    ids: &[String],
) -> Result<DraftStatusBatch, GuiError> {
    let (done, failed) = each_of_by(ids, about_one_draft, |id| {
        let answer = call(
            door,
            method,
            json!({"account": account, "id": id}),
            DRAFT_QUERY_BUDGET,
            Addressing::Resource,
        )?;
        decode::<DraftStatusChanged>(method, answer)
    })?;
    Ok(DraftStatusBatch {
        done,
        failed: failed
            .into_iter()
            .map(|(id, error)| DraftStatusFailure {
                invalid: invalid_payload(door, account, &id, &error),
                id,
                error,
            })
            .collect(),
    })
}

/// Approve every draft of `ids`: a queued send for `send.approved`.
pub fn draft_approve_on(
    door: &Door,
    account: &str,
    ids: &[String],
) -> Result<DraftStatusBatch, GuiError> {
    draft_status_on(door, "draft.approve", account, ids)
}

/// Put every draft of `ids` back to `draft`.
pub fn draft_demote_on(
    door: &Door,
    account: &str,
    ids: &[String],
) -> Result<DraftStatusBatch, GuiError> {
    draft_status_on(door, "draft.demote", account, ids)
}

/// The signatures a draft of `account` can carry.
///
/// The daemon serves no `signature.list`: the TUI reads the signatures
/// directory itself (`mp_core::signatures`), and so does this over a daemon.
/// The fixture answers a `signature.list` of its own.
pub fn signature_list_on(door: &Door, account: &str) -> Result<SignatureListing, GuiError> {
    match door {
        Door::Fixture(_) => {
            let answer = call(
                door,
                "signature.list",
                json!({"account": account}),
                DRAFT_QUERY_BUDGET,
                Addressing::Resource,
            )?;
            decode("signature.list", answer)
        }
        Door::Daemon(_) => Ok(SignatureListing {
            account: account.to_string(),
            names: mp_core::signatures::list(),
            default: mp_core::signatures::default_signature_name(account),
        }),
    }
}

/// Rewrite the recipients (and, when given, the subject) of the draft `id`
/// in place, client-side as the TUI's `ce` does; the body is not touched and
/// the signature is not re-spliced. The daemon's watcher publishes the
/// change as `draft.changed`.
pub fn draft_set_recipients_on(
    door: &Door,
    account: &str,
    id: &str,
    headers: &DraftHeaders,
    keep_subject: bool,
) -> Result<DraftLocation, GuiError> {
    let location = draft_path_on(door, account, id)?;
    let mut edit = headers.edit();
    if keep_subject {
        let draft = mp_core::draft::parse_email_draft(std::path::Path::new(&location.path))
            .map_err(|e| GuiError::protocol(format!("{} does not parse: {e:#}", location.path)))?;
        edit.subject = draft.frontmatter.subject;
    }
    rewrite_recipients(door, &location.path, &edit)?;
    Ok(location)
}

/// Every hold the daemon carries, or `account`'s.
pub fn send_hold_status_on(door: &Door, account: Option<&str>) -> Result<HoldListing, GuiError> {
    let params = match account {
        Some(account) => json!({"account": account}),
        None => json!({}),
    };
    let answer = call(
        door,
        "send.hold_status",
        params,
        HOLD_BUDGET,
        Addressing::Params,
    )?;
    serde_json::from_value(answer)
        .map_err(|e| GuiError::protocol(format!("send.hold_status did not decode: {e}")))
}

/// Stop one hold, from whichever client armed it. A hold that already fired
/// or never was is `not_found`: a cancel is not a recall.
pub fn send_cancel_hold_on(door: &Door, operation_id: &str) -> Result<HoldCancelled, GuiError> {
    let answer = call(
        door,
        "send.cancel_hold",
        json!({"operation_id": operation_id}),
        HOLD_BUDGET,
        Addressing::Resource,
    )?;
    serde_json::from_value(answer)
        .map_err(|e| GuiError::protocol(format!("send.cancel_hold did not decode: {e}")))
}

/// Start a sync pass of `account` and await it like a server search: it ends
/// with `operation.finished`, `operation_settled` or `operation_dropped`, and
/// the pass itself publishes `sync.completed` to every client.
pub fn sync_trigger_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
    mode: SyncMode,
) -> Result<OperationStarted, GuiError> {
    let operation_id = session.start_operation(
        door,
        mode.method(),
        json!({"account": account}),
        PendingKind::Sync,
        START_BUDGET,
    )?;
    Ok(OperationStarted { operation_id })
}

/// Send the draft `id` of `account`, the TUI's `x` (SND-03): the draft is
/// validated first, and a `draft` status is approved before `send.draft`,
/// so a draft that fails validation keeps its status. A refused approve
/// stops here, with the `draft.invalid` payload for a file that does not
/// parse; a send the daemon refuses to start leaves the approval in place,
/// as the TUI's does.
pub fn send_draft_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
    id: &str,
    hold: bool,
) -> Result<SendStarted, SendRefusal> {
    let answer = call(
        door,
        "draft.list",
        json!({"account": account}),
        DRAFT_QUERY_BUDGET,
        Addressing::Resource,
    )?;
    let listing: DraftListing = decode("draft.list", answer)?;
    let entry = listing.drafts.into_iter().find(|d| d.id == id);
    if let Some(entry) = &entry {
        let validation = draft_validate_on(door, account, id)?;
        if let Some(report) = validation.reports.iter().find(|r| r.id == id && !r.valid) {
            return Err(GuiError::Protocol {
                message: format!(
                    "{} does not validate: {}",
                    entry.selector,
                    report.error.as_deref().unwrap_or("no reason given")
                ),
                code: None,
            }
            .into());
        }
    }
    // A draft the listing does not show is approved too: the daemon's
    // refusal says whether it does not parse (`-32010`) or does not exist.
    let approve = entry.as_ref().is_none_or(|e| e.status != "approved");
    if approve {
        let approved = call(
            door,
            "draft.approve",
            json!({"account": account, "id": id}),
            DRAFT_QUERY_BUDGET,
            Addressing::Resource,
        );
        if let Err(error) = approved {
            return Err(SendRefusal {
                invalid: invalid_payload(door, account, id, &error).map(Box::new),
                error,
            });
        }
    }
    let (operation_id, answer) = session.start_operation_answer(
        door,
        "send.draft",
        json!({"account": account, "id": id, "hold": hold}),
        PendingKind::Send,
        START_BUDGET,
    )?;
    Ok(SendStarted {
        operation_id,
        held: answer["held"] == true,
        approved: approve,
    })
}

/// The unfinished rows of `account`'s outbox, `mp outbox list`. An account
/// with no store answers `ever_used: false`, which is not an error.
pub fn outbox_list_on(door: &Door, account: &str) -> Result<OutboxListing, GuiError> {
    let answer = call(
        door,
        "send.outbox_list",
        json!({"account": account}),
        DRAFT_QUERY_BUDGET,
        Addressing::Resource,
    )?;
    decode("send.outbox_list", answer)
}

/// Retry the outbox row `row_id`, `mp outbox retry`: an operation the GUI
/// awaits as `outbox_retry`, whose `result` is an `OutboxRetryOutcome`. The
/// daemon admits a `failed` row and a `sent_pending_append` row whose APPEND
/// was attempted, and refuses anything else with `-32602`.
pub fn outbox_retry_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
    row_id: i64,
) -> Result<OperationStarted, GuiError> {
    let operation_id = session.start_operation(
        door,
        "send.outbox_retry",
        json!({"account": account, "row_id": row_id}),
        PendingKind::OutboxRetry,
        START_BUDGET,
    )?;
    Ok(OperationStarted { operation_id })
}

/// Drop the outbox row `row_id` and release its bytes, `mp outbox discard`:
/// one committed command. A row that is gone is `not_found`.
pub fn outbox_discard_on(
    door: &Door,
    account: &str,
    row_id: i64,
) -> Result<OutboxDiscarded, GuiError> {
    let answer = call(
        door,
        "send.outbox_discard",
        json!({"account": account, "row_id": row_id}),
        MUTATION_BUDGET,
        Addressing::Resource,
    )?;
    decode("send.outbox_discard", answer)
}

/// Send every approved draft of `account`, the TUI's `cX`: one operation,
/// whose hold names the first draft it would send.
pub fn send_approved_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
    hold: bool,
) -> Result<SendStarted, GuiError> {
    let (operation_id, answer) = session.start_operation_answer(
        door,
        "send.approved",
        json!({"account": account, "hold": hold}),
        PendingKind::SendApproved,
        START_BUDGET,
    )?;
    Ok(SendStarted {
        operation_id,
        held: answer["held"] == true,
        approved: false,
    })
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// Register the ordered event channel; see `session::GuiEvent`.
#[tauri::command(rename_all = "snake_case")]
pub fn subscribe_events(session: State<'_, SessionHandle>, on_event: Channel<GuiEvent>) {
    let session = session.inner().clone();
    // The claim is taken here, on the IPC thread and so in call order; the
    // subscription itself runs off it, since it waits for the pump lock and
    // takes a bootstrap. Under React StrictMode two calls race to that lock,
    // and the claim is what makes the later one the sink.
    let claim = session.claim_subscription();
    tauri::async_runtime::spawn_blocking(move || {
        session.subscribe_claimed(claim, Box::new(move |event| on_event.send(event).is_ok()));
    });
}

#[tauri::command(rename_all = "snake_case")]
pub fn connection_status(session: State<'_, SessionHandle>) -> ConnectionStatus {
    session.status()
}

/// Try the connection again after a failure (no-op while connected).
#[tauri::command(rename_all = "snake_case")]
pub fn retry_connect(session: State<'_, SessionHandle>) {
    session.start();
}

/// A fresh `state.bootstrap`, also sent on the event channel as
/// `rebootstrapped` with `cause: "requested"`.
#[tauri::command(rename_all = "snake_case")]
pub async fn bootstrap(session: State<'_, SessionHandle>) -> Result<Bootstrap, GuiError> {
    with_door(&session, |s, door| s.bootstrap(door)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_accounts(
    session: State<'_, SessionHandle>,
) -> Result<Vec<AccountInfo>, GuiError> {
    with_door(&session, |s, door| {
        list_accounts_on(door, s.last_bootstrap().as_ref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_mailboxes(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<MailboxListing, GuiError> {
    with_door(&session, move |s, door| {
        list_mailboxes_on(door, s.last_bootstrap().as_ref(), &account)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn list_messages(
    session: State<'_, SessionHandle>,
    account: String,
    mailbox: String,
) -> Result<MessageList, GuiError> {
    with_door(&session, move |_, door| {
        list_messages_on(door, &account, &mailbox)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_text(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<MessageText, GuiError> {
    with_door(&session, move |_, door| {
        message_text_on(door, &account, row_id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_html_meta(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<MessageMeta, GuiError> {
    with_door(&session, move |_, door| {
        message_html_meta_on(door, &account, row_id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn search_local(
    session: State<'_, SessionHandle>,
    params: LocalSearchParams,
) -> Result<Vec<LocalSearchHit>, GuiError> {
    with_door(&session, move |_, door| search_local_on(door, &params)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn search_server_start(
    session: State<'_, SessionHandle>,
    params: ServerSearchParams,
) -> Result<OperationStarted, GuiError> {
    with_door(&session, move |s, door| {
        search_server_start_on(s, door, &params)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn search_server_cancel(
    session: State<'_, SessionHandle>,
    operation_id: String,
) -> Result<CancelOutcome, GuiError> {
    with_door(&session, move |s, door| {
        search_server_cancel_on(s, door, &operation_id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_archive(
    session: State<'_, SessionHandle>,
    account: String,
    row_ids: Vec<i64>,
) -> Result<MutationBatch, GuiError> {
    with_door(&session, move |_, door| {
        message_archive_on(door, &account, &row_ids)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_delete(
    session: State<'_, SessionHandle>,
    account: String,
    row_ids: Vec<i64>,
) -> Result<MutationBatch, GuiError> {
    with_door(&session, move |_, door| {
        message_delete_on(door, &account, &row_ids)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_move(
    session: State<'_, SessionHandle>,
    account: String,
    row_ids: Vec<i64>,
    destination: String,
) -> Result<MutationBatch, GuiError> {
    with_door(&session, move |_, door| {
        message_move_on(door, &account, &row_ids, &destination)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_set_flag(
    session: State<'_, SessionHandle>,
    account: String,
    row_ids: Vec<i64>,
    flagged: bool,
) -> Result<MutationBatch, GuiError> {
    with_door(&session, move |_, door| {
        message_set_flag_on(door, &account, &row_ids, flagged)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn message_set_read(
    session: State<'_, SessionHandle>,
    account: String,
    row_ids: Vec<i64>,
    read: bool,
) -> Result<MutationBatch, GuiError> {
    with_door(&session, move |_, door| {
        message_set_read_on(door, &account, &row_ids, read)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_discard(
    session: State<'_, SessionHandle>,
    account: String,
    ids: Vec<String>,
) -> Result<DraftDiscardBatch, GuiError> {
    with_door(&session, move |_, door| {
        draft_discard_on(door, &account, &ids)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_create(
    session: State<'_, SessionHandle>,
    account: String,
    name: String,
    signature: Option<String>,
    no_signature: Option<bool>,
    headers: Option<DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    with_door(&session, move |_, door| {
        draft_create_on(
            door,
            &account,
            &name,
            signature.as_deref(),
            no_signature.unwrap_or(false),
            headers.as_ref(),
        )
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_reply(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
    all: bool,
    headers: Option<DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    with_door(&session, move |_, door| {
        draft_reply_on(door, &account, row_id, all, headers.as_ref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_forward(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
    headers: Option<DraftHeaders>,
) -> Result<DraftCreated, GuiError> {
    with_door(&session, move |_, door| {
        draft_forward_on(door, &account, row_id, headers.as_ref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_from_message(
    session: State<'_, SessionHandle>,
    account: String,
    kind: DraftKind,
    message: DraftMessage,
) -> Result<DraftCreated, GuiError> {
    with_door(&session, move |_, door| {
        draft_from_message_on(door, &account, kind, &message)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_path(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
) -> Result<DraftLocation, GuiError> {
    with_door(&session, move |_, door| draft_path_on(door, &account, &id)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_approve(
    session: State<'_, SessionHandle>,
    account: String,
    ids: Vec<String>,
) -> Result<DraftStatusBatch, GuiError> {
    with_door(&session, move |_, door| {
        draft_approve_on(door, &account, &ids)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_demote(
    session: State<'_, SessionHandle>,
    account: String,
    ids: Vec<String>,
) -> Result<DraftStatusBatch, GuiError> {
    with_door(&session, move |_, door| {
        draft_demote_on(door, &account, &ids)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_validate(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
) -> Result<DraftValidation, GuiError> {
    with_door(&session, move |_, door| {
        draft_validate_on(door, &account, &id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_preview(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
) -> Result<DraftPreview, GuiError> {
    with_door(&session, move |_, door| {
        draft_preview_on(door, &account, &id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn signature_list(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<SignatureListing, GuiError> {
    with_door(&session, move |_, door| signature_list_on(door, &account)).await
}

/// `subject` absent keeps the draft's subject.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn draft_set_recipients(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
    to: String,
    cc: String,
    bcc: String,
    subject: Option<String>,
) -> Result<DraftLocation, GuiError> {
    with_door(&session, move |_, door| {
        let keep_subject = subject.is_none();
        let headers = DraftHeaders {
            to,
            cc,
            bcc,
            subject: subject.unwrap_or_default(),
        };
        draft_set_recipients_on(door, &account, &id, &headers, keep_subject)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn send_hold_status(
    session: State<'_, SessionHandle>,
    account: Option<String>,
) -> Result<HoldListing, GuiError> {
    with_door(&session, move |_, door| {
        send_hold_status_on(door, account.as_deref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn send_cancel_hold(
    session: State<'_, SessionHandle>,
    operation_id: String,
) -> Result<HoldCancelled, GuiError> {
    with_door(&session, move |_, door| {
        send_cancel_hold_on(door, &operation_id)
    })
    .await
}

/// `hold: true` is what the frontend sends, as the TUI does: the daemon's
/// `email.send_hold_secs` decides the window, `0` meaning none.
#[tauri::command(rename_all = "snake_case")]
pub async fn send_draft(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
    hold: bool,
) -> Result<SendStarted, SendRefusal> {
    let session = session.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let door = session.door(CONNECT_WAIT)?;
        send_draft_on(&session, &door, &account, &id, hold)
    })
    .await
    .map_err(|e| GuiError::internal(format!("the command task failed: {e}")))?
}

#[tauri::command(rename_all = "snake_case")]
pub async fn send_approved(
    session: State<'_, SessionHandle>,
    account: String,
    hold: bool,
) -> Result<SendStarted, GuiError> {
    with_door(&session, move |s, door| {
        send_approved_on(s, door, &account, hold)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn outbox_list(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<OutboxListing, GuiError> {
    with_door(&session, move |_, door| outbox_list_on(door, &account)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn outbox_retry(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<OperationStarted, GuiError> {
    with_door(&session, move |s, door| {
        outbox_retry_on(s, door, &account, row_id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn outbox_discard(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<OutboxDiscarded, GuiError> {
    with_door(&session, move |_, door| {
        outbox_discard_on(door, &account, row_id)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn sync_trigger(
    session: State<'_, SessionHandle>,
    account: String,
    mode: SyncMode,
) -> Result<OperationStarted, GuiError> {
    with_door(&session, move |s, door| {
        sync_trigger_on(s, door, &account, mode)
    })
    .await
}

/// `mp daemon restart`. The frontend asks the user first; this just does it.
/// In fixture mode it simulates a daemon restart instead.
#[tauri::command(rename_all = "snake_case")]
pub async fn restart_daemon(session: State<'_, SessionHandle>) -> Result<(), GuiError> {
    let session = session.inner().clone();
    blocking(move || {
        if let Some(fixture) = session.fixture() {
            return fixture.simulate("restart").map_err(GuiError::internal);
        }
        connector::restart_daemon_blocking()?;
        // A session that never came up (version mismatch, no daemon) starts
        // now; a live one reconnects on its own.
        session.start();
        Ok(())
    })
    .await
}

/// Every URL the webview refused since the last call; the log is cleared.
#[tauri::command(rename_all = "snake_case")]
pub fn intercepted_urls(log: State<'_, InterceptLog>) -> Vec<InterceptedUrl> {
    log.drain()
}

/// Open an http, https or mailto URL in the user's default handler. Only on
/// an explicit user action; `MP_DESKTOP_STUB_OPENER=1` records it instead.
#[tauri::command(rename_all = "snake_case")]
pub fn open_external(
    app: AppHandle,
    log: State<'_, InterceptLog>,
    url: String,
) -> Result<(), GuiError> {
    let parsed =
        tauri::Url::parse(&url).map_err(|e| GuiError::protocol(format!("not a URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https" | "mailto") {
        return Err(GuiError::protocol(format!(
            "refusing to open a `{}` URL",
            parsed.scheme()
        )));
    }
    if std::env::var_os(STUB_OPENER_ENV).is_some_and(|v| !v.is_empty() && v != "0") {
        log.push(InterceptedUrl::now(
            parsed.to_string(),
            InterceptSource::OpenExternalStub,
        ));
        tracing::info!("[open] stubbed: {parsed}");
        return Ok(());
    }
    tracing::info!("[open] {parsed}");
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|e| GuiError::internal(format!("could not open {parsed}: {e}")))
}

#[tauri::command(rename_all = "snake_case")]
pub fn version_info(session: State<'_, SessionHandle>) -> VersionInfo {
    VersionInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_min: PROTOCOL_MIN,
        protocol_max: PROTOCOL_MAX,
        daemon: if session.is_fixture() {
            None
        } else {
            connector::last_hello()
        },
        fixture: session.is_fixture(),
    }
}

/// Fixture mode only: drive a connection state (`disconnect`, `reconnect`,
/// `restart`, `resync`, `new_mail`, `shutdown`), a rolled-back drain
/// (`rollback`, `rollback:<n>`), a send hold (`hold`) or a save in the
/// editor of the newest opened draft (`editor_save`, `editor_invalid`).
#[tauri::command(rename_all = "snake_case")]
pub fn fixture_simulate(session: State<'_, SessionHandle>, what: String) -> Result<(), GuiError> {
    let fixture = session
        .fixture()
        .ok_or_else(|| GuiError::protocol("fixture_simulate works in fixture mode only"))?;
    fixture.simulate(&what).map_err(GuiError::protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::sync::Arc;

    fn door() -> Door {
        let (tx, _rx) = std::sync::mpsc::channel();
        Door::Fixture(Arc::new(Fixture::load(tx).expect("fixture")))
    }

    /// A fixture door and the fixture behind it, its events dropped.
    fn fixture_door() -> (Door, Arc<Fixture>) {
        let (tx, rx) = std::sync::mpsc::channel();
        // Keep the receiver alive so the fixture's sends do not fail.
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture)
    }

    fn rows_of(d: &Door, account: &str, mailbox: &str) -> Vec<MessageListRow> {
        match list_messages_on(d, account, mailbox).expect("list") {
            MessageList::Messages { rows, .. } => rows,
            other => panic!("expected messages, got {other:?}"),
        }
    }

    fn ids_of(d: &Door, account: &str, mailbox: &str) -> Vec<i64> {
        rows_of(d, account, mailbox).iter().map(|r| r.id).collect()
    }

    /// Every mutation call the fixture saw went out with `settle: false`.
    fn assert_queued(fixture: &Fixture, method: &str, count: usize) {
        let calls: Vec<Value> = fixture
            .calls()
            .into_iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p)
            .collect();
        assert_eq!(calls.len(), count, "{method} calls");
        assert!(
            calls.iter().all(|p| p["settle"] == false),
            "{method} went out without settle: false: {calls:?}"
        );
    }

    fn not_found_row(failure: &MutationFailure) -> bool {
        matches!(
            failure.error,
            GuiError::NotFound {
                code: Some(-32602),
                ..
            }
        )
    }

    #[test]
    fn an_archive_moves_every_row_in_order_and_queues() {
        let (d, f) = fixture_door();
        let batch = message_archive_on(&d, "work", &[1002, 1001]).expect("archived");
        assert!(batch.failed.is_empty());
        let ids: Vec<i64> = batch.done.iter().map(|a| a.row_id).collect();
        assert_eq!(ids, [1002, 1001], "the list's order");
        let ack = &batch.done[1];
        assert_eq!(
            (ack.account.as_str(), ack.mailbox.as_str()),
            ("work", "inbox")
        );
        assert_eq!(
            ack.moved_to.as_ref().map(|m| m.mailbox.as_str()),
            Some("archive")
        );
        assert_eq!(ack.read, None);
        assert_queued(&f, "message.archive", 2);
        let inbox = ids_of(&d, "work", "inbox");
        assert!(!inbox.contains(&1001) && !inbox.contains(&1002));
        assert_eq!(ids_of(&d, "work", "archive").len(), 5);
        let value = serde_json::to_value(&batch).expect("json");
        assert!(value["done"][0].get("read").is_none(), "absent, not null");
    }

    #[test]
    fn an_unknown_row_fails_alone_and_the_rest_go_ahead() {
        let (d, _f) = fixture_door();
        let batch = message_archive_on(&d, "work", &[1001, 424242, 1002]).expect("batch");
        assert_eq!(batch.done.len(), 2);
        assert_eq!(batch.failed.len(), 1);
        assert_eq!(batch.failed[0].row_id, 424242);
        assert!(not_found_row(&batch.failed[0]));
        let value = serde_json::to_value(&batch.failed[0]).expect("json");
        assert_eq!(value["error"]["kind"], "not_found");
    }

    #[test]
    fn an_unknown_id_of_the_fixture_account_is_not_found_and_an_unknown_account_fails_the_batch() {
        // 1015 is a home row, which the fixture scopes away from work. A live
        // daemon does not: row ids are per-store, and it would act on work's
        // row 1015 if there were one (rust-layer.md, Mutations).
        let (d, _f) = fixture_door();
        let batch = message_delete_on(&d, "work", &[1015]).expect("batch");
        assert!(batch.done.is_empty());
        assert!(not_found_row(&batch.failed[0]));
        assert_eq!(ids_of(&d, "home", "inbox").len(), 4, "home is untouched");
        assert!(matches!(
            message_delete_on(&d, "nobody", &[1001]),
            Err(GuiError::NotFound {
                code: Some(-32005),
                ..
            })
        ));
        let empty = message_delete_on(&d, "work", &[]).expect("nothing to do");
        assert_eq!(empty, MutationBatch::default());
    }

    #[test]
    fn a_delete_removes_the_row() {
        let (d, f) = fixture_door();
        let batch = message_delete_on(&d, "work", &[1009]).expect("deleted");
        assert_eq!(batch.done[0].mailbox, "sent");
        assert_eq!(batch.done[0].moved_to, None);
        assert_queued(&f, "message.delete", 1);
        assert!(!ids_of(&d, "work", "sent").contains(&1009));
        assert!(matches!(
            message_text_on(&d, "work", 1009),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn a_move_lands_in_the_destination_by_slug_or_label() {
        let (d, f) = fixture_door();
        let batch = message_move_on(&d, "home", &[1015], "Newsletters").expect("moved");
        let moved = batch.done[0].moved_to.clone().expect("moved_to");
        assert_eq!(moved.mailbox, "newsletters");
        assert_eq!(
            moved.selector, "mp://home/newsletters/%3Cdinner-on-saturday-1015@fixture.example%3E",
            "the stored Message-ID, brackets percent-encoded, as the daemon answers"
        );
        assert!(ids_of(&d, "home", "newsletters").contains(&1015));
        let meta = message_html_meta_on(&d, "home", 1015).expect("meta");
        assert_eq!(meta.mailbox, "newsletters");
        assert_eq!(
            meta.selector, "mp://home/newsletters/dinner-on-saturday-1015@fixture.example",
            "the row's own selector is the bare Message-ID, unlike moved_to's"
        );
        assert_queued(&f, "message.move", 1);
        let calls = f.calls();
        let call = calls
            .iter()
            .find(|(m, _)| m == "message.move")
            .expect("call");
        assert_eq!(call.1["destination"], "Newsletters");

        let refused = message_move_on(&d, "home", &[1016], "drafts").expect("batch");
        assert!(refused.done.is_empty() && not_found_row(&refused.failed[0]));
        let refused = message_move_on(&d, "home", &[1016], "nowhere").expect("batch");
        assert!(refused.failed[0].error.message().contains("not a mailbox"));
    }

    #[test]
    fn flag_and_read_set_the_state_they_name() {
        let (d, f) = fixture_door();
        let unread_before = list_mailboxes_on(&d, None, "work")
            .expect("mailboxes")
            .mailboxes[0]
            .unread;
        let flagged = message_set_flag_on(&d, "work", &[1001, 1002], true).expect("flagged");
        assert!(flagged.done.iter().all(|a| a.flagged == Some(true)));
        assert!(message_html_meta_on(&d, "work", 1002)
            .expect("meta")
            .flags
            .contains(&"flagged".to_string()));
        let unread = message_set_read_on(&d, "work", &[1001], false).expect("unread");
        assert_eq!(unread.done[0].read, Some(false));
        let row = rows_of(&d, "work", "inbox")
            .into_iter()
            .find(|r| r.id == 1001)
            .expect("1001");
        assert!(!row.flags.seen && row.flags.flagged);
        let calls = f.calls();
        assert_queued(&f, "message.set_flag", 2);
        assert_queued(&f, "message.set_read", 1);
        let read_call = calls
            .iter()
            .find(|(m, _)| m == "message.set_read")
            .expect("call");
        assert_eq!(read_call.1["read"], false);
        let listing = list_mailboxes_on(&d, None, "work").expect("mailboxes");
        assert_eq!(
            listing.mailboxes[0].unread,
            unread_before + 1,
            "1001 joins the unread"
        );
    }

    #[test]
    fn a_batch_stops_at_an_error_about_the_whole_batch() {
        let unavailable = || GuiError::Timeout {
            message: "slow".into(),
        };
        let (done, failed) = each_of(
            &[1, 2, 3],
            |&i| {
                if i == 1 {
                    Ok(i)
                } else {
                    Err(unavailable())
                }
            },
        )
        .expect("one went");
        assert_eq!(done, [1]);
        let failed: Vec<i32> = failed.into_iter().map(|(i, _)| i).collect();
        assert_eq!(failed, [2, 3], "the rest are reported, not dropped");
        assert!(each_of(&[1, 2], |_| Err::<i32, _>(unavailable())).is_err());
    }

    #[test]
    fn an_account_that_is_not_ready_rejects_the_batch_after_one_call() {
        let mut calls = 0;
        let result = each_of(&[1, 2, 3], |_| {
            calls += 1;
            Err::<i32, _>(GuiError::from_call_text(
                "message.archive: the daemon refused the call: account_not_ready: work (-32006)",
                Addressing::Resource,
            ))
        });
        assert!(
            matches!(
                result,
                Err(GuiError::Protocol {
                    code: Some(-32006),
                    ..
                })
            ),
            "{result:?}"
        );
        assert_eq!(calls, 1, "no round trip for the rows after it");
    }

    #[test]
    fn drafts_are_discarded_one_by_one() {
        let (d, _f) = fixture_door();
        let batch = draft_discard_on(
            &d,
            "work",
            &["offsite-note".to_string(), "no-such-draft".to_string()],
        )
        .expect("batch");
        assert_eq!(batch.done.len(), 1);
        assert_eq!(batch.done[0].selector, "mp://work/drafts/offsite-note");
        assert_eq!(batch.failed[0].id, "no-such-draft");
        match list_messages_on(&d, "work", "drafts").expect("drafts") {
            MessageList::Drafts { listing, .. } => assert_eq!(listing.drafts.len(), 1),
            other => panic!("expected drafts, got {other:?}"),
        }
        assert!(matches!(
            draft_discard_on(&d, "nobody", &["x".to_string()]),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn holds_are_listed_and_cancelled_once() {
        let (d, _f) = fixture_door();
        let all = send_hold_status_on(&d, None).expect("status");
        assert_eq!(all.holds.len(), 1);
        assert!(send_hold_status_on(&d, Some("home"))
            .expect("home")
            .holds
            .is_empty());
        let id = all.holds[0].operation_id.clone();
        let cancelled = send_cancel_hold_on(&d, &id).expect("cancelled");
        assert!(cancelled.cancelled);
        assert_eq!(cancelled.operation_id, id);
        assert!(send_hold_status_on(&d, None)
            .expect("status")
            .holds
            .is_empty());
        assert!(matches!(
            send_cancel_hold_on(&d, &id),
            Err(GuiError::NotFound {
                code: Some(-32602),
                ..
            })
        ));
    }

    #[test]
    fn a_sync_is_awaited_as_a_sync() {
        let (d, f) = fixture_door();
        let session = SessionHandle::new(true);
        let started = sync_trigger_on(&session, &d, "work", SyncMode::Full).expect("started");
        assert_eq!(session.pending(), vec![started.operation_id.clone()]);
        let (method, params) = f.calls().last().cloned().expect("call");
        assert_eq!(
            (method.as_str(), params),
            ("sync.full", json!({"account": "work"}))
        );
        assert!(matches!(
            sync_trigger_on(&session, &d, "nobody", SyncMode::Quick),
            Err(GuiError::NotFound { .. })
        ));
        assert_eq!(
            serde_json::to_value(SyncMode::Quick).expect("json"),
            json!("quick")
        );
    }

    #[test]
    fn accounts_merge_the_config_and_the_snapshot() {
        let d = door();
        let b: Bootstrap = serde_json::from_value(
            d.call_within("state.bootstrap", json!({}), LIST_BUDGET)
                .expect("b"),
        )
        .expect("decodes");
        let accounts = list_accounts_on(&d, Some(&b)).expect("accounts");
        assert_eq!(accounts.len(), 2);
        assert!(accounts[0].default);
        assert_eq!(accounts[1].sync_health, SyncHealthState::Failed);
        assert_eq!(accounts[1].outbox.queued, 1);
    }

    #[test]
    fn mailboxes_carry_kind_and_totals() {
        let listing = list_mailboxes_on(&door(), None, "work").expect("mailboxes");
        assert_eq!(listing.mailboxes.len(), 4);
        assert_eq!(listing.mailboxes[1].kind, MailboxKind::Drafts);
        assert_eq!(listing.total, 8 + 2 + 3 + 3);
        assert!(matches!(
            list_mailboxes_on(&door(), None, "nobody"),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn the_drafts_mailbox_branches_to_the_draft_listing() {
        let d = door();
        match list_messages_on(&d, "work", "drafts").expect("drafts") {
            MessageList::Drafts { listing, .. } => assert_eq!(listing.drafts.len(), 2),
            other => panic!("expected drafts, got {other:?}"),
        }
        match list_messages_on(&d, "work", "inbox").expect("inbox") {
            MessageList::Messages { rows, total, .. } => {
                assert_eq!(rows.len(), 8);
                assert_eq!(total, 8);
            }
            other => panic!("expected messages, got {other:?}"),
        }
        let value =
            serde_json::to_value(list_messages_on(&d, "work", "drafts").expect("d")).expect("json");
        assert_eq!(value["kind"], "drafts");
    }

    #[test]
    fn a_message_reads_as_text_and_as_meta() {
        let d = door();
        let text = message_text_on(&d, "work", 1001).expect("text");
        assert!(text.body.as_deref().unwrap_or_default().contains("ledger"));
        let meta = message_html_meta_on(&d, "work", 1001).expect("meta");
        assert_eq!(meta.html_url, "mpmsg://localhost/work/1001");
        assert_eq!(meta.attachments.len(), 1);
        assert!(meta.flags.contains(&"read".to_string()));
        assert!(matches!(
            message_text_on(&d, "work", 424242),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn absent_headers_decode_as_none() {
        let meta = message_html_meta_on(&door(), "home", 1021).expect("meta");
        assert_eq!(meta.subject, None);
        assert_eq!(meta.date, None);
        assert_eq!(meta.cc, None);
        assert!(meta.from.is_some());
        let value = serde_json::to_value(&meta).expect("json");
        assert!(value["subject"].is_null() && value["date"].is_null());
        // The listing sends the same absent headers as `""`.
        match list_messages_on(&door(), "home", "newsletters").expect("list") {
            MessageList::Messages { rows, .. } => {
                let row = rows.iter().find(|r| r.id == 1021).expect("row 1021");
                assert_eq!((row.subject.as_str(), row.date_display.as_str()), ("", ""));
            }
            other => panic!("expected messages, got {other:?}"),
        }
    }

    #[test]
    fn a_null_header_on_the_wire_decodes() {
        let wire = json!({
            "selector": "mp://work/inbox/x", "account": "work", "mailbox": "inbox",
            "message_id": "<x@example>", "from": null, "to": null, "cc": null,
            "subject": null, "date": null, "flags": [], "invite": false, "attachments": []
        });
        let meta: MessageMeta = serde_json::from_value(wire).expect("decodes");
        assert_eq!((meta.from, meta.subject, meta.date), (None, None, None));
    }

    #[test]
    fn local_search_flattens_the_row_beside_the_mailbox() {
        let hits = search_local_on(
            &door(),
            &LocalSearchParams {
                account: "work".into(),
                query: "ledger".into(),
                ..Default::default()
            },
        )
        .expect("hits");
        assert!(hits.len() >= 2);
        let value = serde_json::to_value(&hits[0]).expect("json");
        assert!(value["mailbox"].is_string());
        assert!(value["id"].is_i64());
        assert!(value.get("row").is_none());
    }

    /// A fixture door, the fixture, and its event stream.
    fn fixture_with_events() -> (
        Door,
        Arc<Fixture>,
        std::sync::mpsc::Receiver<mp_client::events::Incoming>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture, rx)
    }

    /// The `(kind, payload)` of every event posted so far.
    fn drained(
        rx: &std::sync::mpsc::Receiver<mp_client::events::Incoming>,
    ) -> Vec<(String, Value)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|i| match i {
                mp_client::events::Incoming::Event(e) => Some((e.kind, e.payload)),
                _ => None,
            })
            .collect()
    }

    fn parsed(path: &str) -> mp_core::types::EmailDraft {
        mp_core::draft::parse_email_draft(std::path::Path::new(path)).expect("the draft parses")
    }

    /// Open `path` with the fixture's stubbed editor, `zed --wait`.
    fn open_in_editor(f: &Fixture, path: &str) -> crate::editor::EditorLaunch {
        let env =
            |name: &str| (name == crate::editor::EDITOR_ENV).then(|| "zed --wait".to_string());
        let none = |_: &std::path::Path| false;
        let lookup = crate::editor::Lookup {
            env: &env,
            setting: None,
            is_file: &none,
            macos: true,
        };
        crate::editor::open_on(Some(f), &lookup, path, crate::editor::EXIT_WINDOW)
            .expect("journaled")
    }

    fn headers(to: &str, cc: &str, subject: &str) -> DraftHeaders {
        DraftHeaders {
            to: to.into(),
            cc: cc.into(),
            bcc: String::new(),
            subject: subject.into(),
        }
    }

    #[test]
    fn a_created_draft_is_a_file_with_frontmatter_and_a_taken_name_is_refused() {
        let (d, _f, rx) = fixture_with_events();
        let created = draft_create_on(&d, "work", "note", None, false, None).expect("created");
        assert!(
            created.path.ends_with("/drafts/work/note.md"),
            "{}",
            created.path
        );
        assert_eq!(created.selector, format!("mp://work/drafts/{}", created.id));
        assert_eq!(created.source, None);
        let draft = parsed(&created.path);
        assert_eq!(draft.frontmatter.id.as_deref(), Some(created.id.as_str()));
        assert_eq!(draft.frontmatter.status.to_string(), "draft");
        assert!(
            draft.body_markdown.contains("Fixture GmbH"),
            "the account's default signature: {}",
            draft.body_markdown
        );
        let events = drained(&rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "draft.changed");
        assert_eq!(events[0].1["id"], created.id.as_str());

        match draft_create_on(&d, "work", "note", None, false, None) {
            Err(GuiError::Protocol {
                code: Some(-32602),
                message,
            }) => assert!(message.contains(&created.path), "{message}"),
            other => panic!("expected the collision, got {other:?}"),
        }
        assert!(matches!(
            draft_create_on(&d, "nobody", "x", None, false, None),
            Err(GuiError::NotFound {
                code: Some(-32005),
                ..
            })
        ));

        let wizard = headers("robin@example.com, ", "", " Hello ");
        let bare = draft_create_on(&d, "work", "bare", None, true, Some(&wizard)).expect("bare");
        let draft = parsed(&bare.path);
        assert_eq!(draft.frontmatter.to.as_deref(), Some("robin@example.com"));
        assert_eq!(draft.frontmatter.subject, "Hello");
        assert!(
            !draft.body_markdown.contains("Fixture GmbH"),
            "no_signature"
        );
        let short = draft_create_on(&d, "work", "short", Some("short"), false, None).expect("s");
        assert!(!parsed(&short.path).body_markdown.contains("Fixture GmbH"));
        match list_messages_on(&d, "work", "drafts").expect("drafts") {
            MessageList::Drafts { listing, .. } => {
                assert_eq!(listing.drafts.len(), 5);
                assert!(listing.drafts.iter().any(|r| r.id == bare.id && r.ready));
            }
            other => panic!("expected drafts, got {other:?}"),
        }
    }

    #[test]
    fn a_reply_carries_in_reply_to_and_the_subject() {
        let (d, f) = fixture_door();
        let created = draft_reply_on(&d, "work", 1001, false, None).expect("reply");
        let (method, params) = f.calls().last().cloned().expect("call");
        assert_eq!(method, "draft.reply");
        assert_eq!(
            params,
            json!({"account": "work", "source": {"row_id": 1001}, "all": false})
        );
        let source = created.source.clone().expect("a reply names its source");
        assert_eq!(source.id, "inbox/1");
        assert_eq!(
            source.selector,
            "mp://work/inbox/quarterly-ledger-review-1001@fixture.example"
        );
        let draft = parsed(&created.path);
        assert!(draft
            .frontmatter
            .in_reply_to
            .as_deref()
            .is_some_and(|v| v.contains("quarterly-ledger-review-1001@fixture.example")));
        assert_eq!(draft.frontmatter.subject, "Re: Quarterly ledger review");
        assert_eq!(draft.frontmatter.to.as_deref(), Some("ivana@example.com"));
        assert!(draft.body_markdown.contains("quarterly ledger is attached"));
        assert!(
            draft.frontmatter.attachments.unwrap_or_default().is_empty(),
            "a reply carries no attachments"
        );

        let all = draft_reply_on(
            &d,
            "work",
            1001,
            true,
            Some(&headers("x@example.com", "y@example.com", "Custom")),
        )
        .expect("reply-all");
        let (_, params) = f.calls().last().cloned().expect("call");
        assert_eq!(params["all"], true);
        assert_eq!(
            params["headers"],
            json!({"to": "x@example.com", "cc": "y@example.com", "bcc": "", "subject": "Custom"})
        );
        let draft = parsed(&all.path);
        assert_eq!(
            (
                draft.frontmatter.to.as_deref(),
                draft.frontmatter.subject.as_str()
            ),
            (Some("x@example.com"), "Custom")
        );
        assert!(
            draft.frontmatter.in_reply_to.is_some(),
            "the override keeps the thread"
        );

        assert!(matches!(
            draft_reply_on(&d, "work", 424242, false, None),
            Err(GuiError::NotFound {
                code: Some(-32602),
                ..
            })
        ));
    }

    #[test]
    fn a_forward_carries_the_attachments_and_a_hit_is_built_from_itself() {
        let (d, _f) = fixture_door();
        let forward = draft_forward_on(&d, "work", 1001, None).expect("forward");
        let draft = parsed(&forward.path);
        assert!(draft.frontmatter.subject.starts_with("Fwd:"));
        assert!(draft.frontmatter.forwarded_from.is_some());
        let attachments = draft.frontmatter.attachments.unwrap_or_default();
        assert_eq!(attachments.len(), 1);
        assert!(attachments[0].ends_with("ledger-q3.pdf"));
        assert!(std::path::Path::new(&attachments[0]).is_file());

        let hit = DraftMessage {
            from: "Old Colleague <old@example.com>".into(),
            to: "me@example.com".into(),
            subject: "Old thread".into(),
            message_id: Some("<server-only@fixture.example>".into()),
            date_display: "Mon, 3 Mar 2025 09:00:00 +0100".into(),
            body_text: "A message only the server holds.\n".into(),
            ..Default::default()
        };
        let reply = draft_from_message_on(&d, "work", DraftKind::Reply, &hit).expect("reply");
        assert_eq!(reply.source, None);
        let draft = parsed(&reply.path);
        assert_eq!(draft.frontmatter.subject, "Re: Old thread");
        assert!(draft
            .frontmatter
            .in_reply_to
            .as_deref()
            .is_some_and(|v| v.contains("server-only@fixture.example")));
        let fwd = draft_from_message_on(&d, "work", DraftKind::Forward, &hit).expect("fwd");
        assert!(parsed(&fwd.path)
            .frontmatter
            .attachments
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn an_editor_save_publishes_draft_changed_and_bumps_the_row() {
        let (d, f, rx) = fixture_with_events();
        let offsite = draft_path_on(&d, "work", "offsite-note").expect("path");
        let launch = open_in_editor(&f, &offsite.path);
        assert_eq!(launch.pid, None, "the fixture spawns nothing");
        assert_eq!(launch.source, crate::editor::EditorSource::Env);
        assert_eq!(launch.editor, format!("zed --wait {}", offsite.path));
        let opens = f.editor_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].path, offsite.path);
        assert_eq!(opens[0].command, ["zed", "--wait", offsite.path.as_str()]);
        assert!(drained(&rx).is_empty(), "opening publishes nothing");

        f.simulate("editor_save").expect("saved");
        let events = drained(&rx);
        assert_eq!(events.len(), 1);
        let (kind, payload) = &events[0];
        assert_eq!(kind, "draft.changed");
        assert_eq!(
            (&payload["account"], &payload["id"], &payload["path"]),
            (&json!("work"), &json!("offsite-note"), &json!(offsite.path))
        );
        let text = std::fs::read_to_string(&offsite.path).expect("file");
        assert!(text.trim_end().ends_with(crate::fixture::EDITOR_SAVE_LINE));
        match list_messages_on(&d, "work", "drafts").expect("drafts") {
            MessageList::Drafts { listing, .. } => {
                assert_eq!(listing.drafts[0].id, "offsite-note", "the save bumped it")
            }
            other => panic!("expected drafts, got {other:?}"),
        }
    }

    #[test]
    fn approve_refuses_an_invalid_draft_with_its_payload_and_goes_on() {
        let (d, f, rx) = fixture_with_events();
        let angebot = draft_path_on(&d, "work", "angebot-antwort").expect("path");
        open_in_editor(&f, &angebot.path);
        f.simulate("editor_invalid").expect("broken");
        let events = drained(&rx);
        assert_eq!(events[0].0, "draft.invalid");
        assert_eq!(events[0].1["id"], "angebot-antwort");

        let ids = ["angebot-antwort", "offsite-note", "missing"].map(String::from);
        let batch = draft_approve_on(&d, "work", &ids).expect("batch");
        assert_eq!(batch.done.len(), 1);
        assert_eq!(
            (batch.done[0].id.as_str(), batch.done[0].status.as_str()),
            ("offsite-note", "approved")
        );
        let offsite = parsed(&batch.done[0].path);
        assert_eq!(offsite.frontmatter.status.to_string(), "approved");
        assert_eq!(batch.failed.len(), 2);
        let refused = &batch.failed[0];
        assert_eq!(refused.id, "angebot-antwort");
        assert!(matches!(
            refused.error,
            GuiError::Protocol {
                code: Some(-32010),
                ..
            }
        ));
        let invalid = refused.invalid.as_ref().expect("the validation payload");
        assert_eq!(invalid.path, angebot.path);
        assert!(!invalid.diagnostics[0].message.is_empty());
        assert!(!invalid.diagnostics[0].message.contains("-32010"));
        assert_eq!(batch.failed[1].id, "missing");
        assert!(batch.failed[1].invalid.is_none());
        let value = serde_json::to_value(&batch).expect("json");
        assert!(
            value["failed"][1].get("invalid").is_none(),
            "absent, not null"
        );

        let demoted = draft_demote_on(&d, "work", &["offsite-note".to_string()]).expect("d");
        assert_eq!(demoted.done[0].status, "draft");
        assert!(matches!(
            draft_approve_on(&d, "nobody", &ids),
            Err(GuiError::NotFound {
                code: Some(-32005),
                ..
            })
        ));
    }

    #[test]
    fn set_recipients_rewrites_the_header_and_keeps_the_body() {
        let (d, _f, rx) = fixture_with_events();
        let angebot = draft_path_on(&d, "work", "angebot-antwort").expect("path");
        let body_of = |text: &str| text.splitn(3, "---\n").nth(2).map(str::to_string);
        let before = std::fs::read_to_string(&angebot.path).expect("file");
        let wizard = headers("a@example.com, b@example.com,", "c@example.com", "");
        let location =
            draft_set_recipients_on(&d, "work", "angebot-antwort", &wizard, true).expect("set");
        assert_eq!(location.path, angebot.path);
        let after = std::fs::read_to_string(&angebot.path).expect("file");
        assert_eq!(body_of(&after), body_of(&before), "the body is untouched");
        let draft = parsed(&angebot.path);
        assert_eq!(
            draft.frontmatter.to.as_deref(),
            Some("a@example.com, b@example.com")
        );
        assert_eq!(draft.frontmatter.cc.as_deref(), Some("c@example.com"));
        assert_eq!(
            draft.frontmatter.subject, "Re: Angebot Dachsanierung",
            "kept"
        );
        let events = drained(&rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "draft.changed");
        assert_eq!(events[0].1["to"], "a@example.com, b@example.com");

        draft_set_recipients_on(
            &d,
            "work",
            "angebot-antwort",
            &headers("a@example.com", "", "Neu"),
            false,
        )
        .expect("set");
        let draft = parsed(&angebot.path);
        assert_eq!(draft.frontmatter.subject, "Neu");
        assert_eq!(draft.frontmatter.cc, None);
        assert!(matches!(
            draft_set_recipients_on(&d, "work", "missing", &wizard, true),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn validate_preview_and_signatures_answer_from_the_files() {
        let (d, _f) = fixture_door();
        let offsite = draft_validate_on(&d, "work", "offsite-note").expect("validate");
        assert_eq!(offsite.reports.len(), 1);
        assert!(!offsite.reports[0].valid);
        assert!(offsite.reports[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("No recipients")));
        let preview = draft_preview_on(&d, "work", "angebot-antwort").expect("preview");
        assert_eq!(preview.to.as_deref(), Some("robin@example.com"));
        assert_eq!(preview.subject, "Re: Angebot Dachsanierung");
        assert!(preview.body.contains("Angebot") && preview.valid && !preview.body_truncated);
        assert!(matches!(
            draft_preview_on(&d, "work", "missing"),
            Err(GuiError::NotFound { .. })
        ));
        let signatures = signature_list_on(&d, "work").expect("signatures");
        assert_eq!(signatures.names, ["short", "work"]);
        assert_eq!(signatures.default.as_deref(), Some("work"));
        assert_eq!(signature_list_on(&d, "home").expect("home").default, None);
    }

    #[test]
    fn a_server_search_is_awaited_until_cancelled() {
        let d = door();
        let session = SessionHandle::new(true);
        let started = search_server_start_on(
            &session,
            &d,
            &ServerSearchParams {
                account: "work".into(),
                query: "ledger".into(),
                ..Default::default()
            },
        )
        .expect("started");
        assert_eq!(session.pending(), vec![started.operation_id.clone()]);
        assert_eq!(
            search_server_cancel_on(&session, &d, &started.operation_id).expect("cancel"),
            CancelOutcome::Cancelled
        );
        assert!(session.pending().is_empty());
        assert_eq!(
            search_server_cancel_on(&session, &d, &started.operation_id).expect("again"),
            CancelOutcome::AlreadySettled
        );
    }

    /// The send path's calls, in order, out of everything the fixture saw.
    fn send_calls(f: &Fixture) -> Vec<(String, Value)> {
        f.calls()
            .into_iter()
            .filter(|(m, _)| {
                matches!(
                    m.as_str(),
                    "draft.list"
                        | "draft.validate"
                        | "draft.approve"
                        | "send.draft"
                        | "send.approved"
                )
            })
            .collect()
    }

    #[test]
    fn send_draft_validates_approves_then_sends_with_the_hold() {
        let (d, f, rx) = fixture_with_events();
        let session = SessionHandle::new(true);
        let started =
            send_draft_on(&session, &d, "work", "angebot-antwort", true).expect("started");
        assert!(started.held);
        assert!(started.approved);
        let calls = send_calls(&f);
        let methods: Vec<&str> = calls.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            [
                "draft.list",
                "draft.validate",
                "draft.approve",
                "send.draft"
            ]
        );
        assert_eq!(
            calls[3].1,
            json!({"account": "work", "id": "angebot-antwort", "hold": true})
        );
        assert_eq!(session.pending(), vec![started.operation_id.clone()]);
        let kinds: Vec<String> = drained(&rx).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, ["draft.changed", "send.hold_started"]);
        let value = serde_json::to_value(&started).expect("json");
        assert_eq!(
            value,
            json!({"operation_id": started.operation_id, "held": true, "approved": true})
        );
    }

    #[test]
    fn an_approved_draft_is_sent_without_a_second_approve() {
        let (d, f) = fixture_door();
        let session = SessionHandle::new(true);
        draft_approve_on(&d, "work", &["angebot-antwort".to_string()]).expect("approved");
        let before = send_calls(&f).len();
        let started =
            send_draft_on(&session, &d, "work", "angebot-antwort", false).expect("started");
        assert!(!started.held);
        assert!(!started.approved);
        let methods: Vec<String> = send_calls(&f)[before..]
            .iter()
            .map(|(m, _)| m.clone())
            .collect();
        assert_eq!(methods, ["draft.list", "draft.validate", "send.draft"]);
    }

    #[test]
    fn a_draft_that_does_not_validate_keeps_its_status_and_is_not_sent() {
        let (d, f) = fixture_door();
        let session = SessionHandle::new(true);
        let refused =
            send_draft_on(&session, &d, "work", "offsite-note", true).expect_err("refused");
        assert!(refused.invalid.is_none());
        assert!(matches!(
            refused.error,
            GuiError::Protocol { code: None, .. }
        ));
        assert!(
            refused.error.message().contains("does not validate"),
            "{refused:?}"
        );
        let methods: Vec<String> = send_calls(&f).into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, ["draft.list", "draft.validate"]);
        assert!(session.pending().is_empty());
    }

    #[test]
    fn a_refused_approve_stops_the_send_with_the_invalid_payload() {
        let (d, f, _rx) = fixture_with_events();
        let session = SessionHandle::new(true);
        let angebot = draft_path_on(&d, "work", "angebot-antwort").expect("path");
        open_in_editor(&f, &angebot.path);
        f.simulate("editor_invalid").expect("broken");
        let refused =
            send_draft_on(&session, &d, "work", "angebot-antwort", true).expect_err("refused");
        assert!(matches!(
            refused.error,
            GuiError::Protocol {
                code: Some(-32010),
                ..
            }
        ));
        let invalid = refused.invalid.as_ref().expect("the draft.invalid payload");
        assert_eq!(invalid.path, angebot.path);
        let methods: Vec<String> = send_calls(&f).into_iter().map(|(m, _)| m).collect();
        assert!(!methods.contains(&"send.draft".to_string()), "{methods:?}");
        assert!(session.pending().is_empty());
        // The refusal is a GuiError with `invalid` beside its fields.
        let value = serde_json::to_value(&refused).expect("json");
        assert_eq!(value["kind"], "protocol");
        assert_eq!(value["code"], -32010);
        assert_eq!(value["invalid"]["id"], "angebot-antwort");
        let missing = send_draft_on(&session, &d, "work", "missing", true).expect_err("missing");
        assert!(matches!(missing.error, GuiError::NotFound { .. }));
        let plain = serde_json::to_value(&missing).expect("json");
        assert!(plain.get("invalid").is_none(), "absent, not null");
    }

    #[test]
    fn send_approved_is_awaited_as_send_approved() {
        let (d, f) = fixture_door();
        let session = SessionHandle::new(true);
        let started = send_approved_on(&session, &d, "work", true).expect("started");
        assert!(!started.approved);
        assert_eq!(session.pending(), vec![started.operation_id.clone()]);
        assert_eq!(
            send_calls(&f).last().cloned().expect("call"),
            (
                "send.approved".to_string(),
                json!({"account": "work", "hold": true})
            )
        );
        assert!(matches!(
            send_approved_on(&session, &d, "nobody", true),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn outbox_list_reads_the_listing_and_ever_used() {
        let (d, _f) = fixture_door();
        let home = outbox_list_on(&d, "home").expect("home");
        assert!(home.ever_used);
        assert_eq!(home.rows.len(), 1);
        assert_eq!(home.rows[0].state, "pending_send");
        assert!(home.rows[0].never_submitted);
        assert_eq!(home.counts.open, 1);
        let work = outbox_list_on(&d, "work").expect("work");
        assert!(!work.ever_used, "never queued is a fact, not an error");
        assert!(matches!(
            outbox_list_on(&d, "nobody"),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn outbox_retry_is_awaited_as_outbox_retry_and_a_refusal_awaits_nothing() {
        let (d, f, rx) = fixture_with_events();
        f.set_send_delay(Duration::ZERO);
        f.simulate("send_fail").expect("armed");
        let session = SessionHandle::new(true);
        let sent = send_draft_on(&session, &d, "work", "angebot-antwort", false).expect("sent");
        session.forget_operation(&sent.operation_id);
        let failed = loop {
            let listing = outbox_list_on(&d, "work").expect("listing");
            if let Some(row) = listing.rows.into_iter().find(|r| r.state == "failed") {
                break row;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        drained(&rx);
        let started = outbox_retry_on(&session, &d, "work", failed.id).expect("started");
        assert_eq!(
            session.pending_kind(&started.operation_id),
            Some(PendingKind::OutboxRetry)
        );
        assert_eq!(
            f.calls().last().cloned().expect("call"),
            (
                "send.outbox_retry".to_string(),
                json!({"account": "work", "row_id": failed.id})
            )
        );
        let queued = outbox_list_on(&d, "home").expect("home").rows[0].id;
        let refused = outbox_retry_on(&session, &d, "home", queued).expect_err("refused");
        assert!(
            refused.message().contains("only a failed row"),
            "{refused:?}"
        );
        assert_eq!(session.pending(), vec![started.operation_id]);
        assert_eq!(
            serde_json::to_value(PendingKind::OutboxRetry).expect("json"),
            json!("outbox_retry")
        );
    }

    #[test]
    fn outbox_discard_answers_the_row_and_a_second_discard_is_not_found() {
        let (d, _f) = fixture_door();
        let row = outbox_list_on(&d, "home").expect("home").rows[0].clone();
        let discarded = outbox_discard_on(&d, "home", row.id).expect("discarded");
        assert!(discarded.discarded);
        assert_eq!(discarded.row_id, row.id);
        assert_eq!(discarded.message_id, row.message_id);
        assert!(outbox_list_on(&d, "home").expect("home").rows.is_empty());
        assert!(matches!(
            outbox_discard_on(&d, "home", row.id),
            Err(GuiError::NotFound {
                code: Some(-32602),
                ..
            })
        ));
    }
}
