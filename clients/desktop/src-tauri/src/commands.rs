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
use mp_core::selector::DRAFTS_MAILBOX;
use mp_protocol::draft::DraftListing;
use mp_protocol::listing::MessageListRow;
use mp_protocol::send::HoldListing;
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
    matches!(
        error,
        GuiError::NotFound {
            code: Some(-32602),
            ..
        } | GuiError::Protocol { .. }
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
    mut each: impl FnMut(&I) -> Result<T, GuiError>,
) -> Result<Outcomes<I, T>, GuiError> {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match each(item) {
            Ok(answer) => done.push(answer),
            Err(e) if about_one_row(&e) => failed.push((item.clone(), e)),
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
/// (`rollback`, `rollback:<n>`) or a send hold (`hold`).
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
    fn a_row_of_another_account_is_not_found_and_an_unknown_account_fails_the_batch() {
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
        assert!(moved.selector.starts_with("mp://home/newsletters/"));
        assert!(ids_of(&d, "home", "newsletters").contains(&1015));
        let meta = message_html_meta_on(&d, "home", 1015).expect("meta");
        assert_eq!(meta.mailbox, "newsletters");
        assert_eq!(meta.selector, moved.selector);
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
}
