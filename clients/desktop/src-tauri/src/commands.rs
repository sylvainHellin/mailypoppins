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
pub struct MessageText {
    pub account: String,
    pub row_id: i64,
    /// `None` when the store holds no readable body.
    pub body: Option<String>,
}

/// One attachment of a message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// The headers of one message (the `message.get` record without its body)
/// and the reader URL its HTML is served at.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub subject: String,
    /// The `Date:` header as stored.
    #[serde(default)]
    pub date: String,
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
pub struct LocalSearchHit {
    pub mailbox: String,
    #[serde(flatten)]
    pub row: MessageListRow,
}

/// `message.search_server`'s params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct OperationStarted {
    pub operation_id: String,
}

/// What a cancel came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    /// It had already finished; nothing to cancel.
    AlreadySettled,
}

/// Versions on both sides of the socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// Register the ordered event channel; see `session::GuiEvent`.
#[tauri::command(rename_all = "snake_case")]
pub fn subscribe_events(session: State<'_, SessionHandle>, on_event: Channel<GuiEvent>) {
    let session = session.inner().clone();
    // Off the IPC thread: subscribing takes a bootstrap.
    tauri::async_runtime::spawn_blocking(move || {
        session.subscribe(Box::new(move |event| on_event.send(event).is_ok()));
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
/// `restart`, `resync`, `new_mail`, `shutdown`).
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
