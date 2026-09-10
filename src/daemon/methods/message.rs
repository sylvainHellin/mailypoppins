//! The read slice (`message.get`, `message.list`, `message.search`, P4-U4), and
//! the three methods that turn a stored message into a file a client can open:
//! `message.materialise_attachment`, `message.materialise_html` and
//! `message.release_handle` (P3b-U12).
//!
//! `message.get`: one message, addressed by `id` (`"<mailbox>/<uid>"`) or by the
//! selector grammar `mp show` takes from a user, which the daemon resolves
//! because resolving one needs the store the client no longer has. The result is
//! the record `mp show --json` prints ([`crate::read_cmd::ShownMessage`]), so the
//! client renders either answer from the payload alone.
//!
//! `message.search`: the ranked hits of the local index, with the params of
//! `mp search --local`'s flags. `body_query` is the wire name of `--body`,
//! because `body` is already the "send me the bodies" switch (`--full`).
//!
//! `message.list`: one mailbox of one account, newest first, or - under
//! `projection: "envelope"` - the whole account as the envelope records
//! `mp dump-mailbox --json` prints.
//!
//! The rows are [`crate::store::read::list_mailbox`]'s, so the daemon answers
//! from the same query, in the same order (`date_sort DESC, id DESC`), as
//! `mp list-messages` and the TUI list. `date_sort` is
//! [`crate::tui::app::resolve_date`]'s sort key, so all three stacks derive a
//! date the same way rather than each parsing the header again, and
//! `date_display` is the `Date:` header as the store holds it, which is the
//! column a listing prints.
//!
//! `total` is how many messages the mailbox holds and ignores `limit`, which is
//! the "In the store: N" of `mp list-messages`. `limit: null` and an absent
//! `limit` both mean "all"; `0` means none, since `null` already spells "all"
//! and a number may not mean the opposite of itself.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::RpcError;

use crate::config::AccountConfig;
use crate::ops::{Backend, ServerOp};
use crate::pending_ops;
use crate::selector::{Namespace, Selector, DRAFTS_MAILBOX};
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};
use crate::tui::app::{build_mailboxes, resolve_date};

use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::handles::{
    handle_dir, reap, remove_handle_dir, HandleId, HandleKind, HandleTable,
};
use super::{internal, invalid_params, string_param};

/// The three read methods, in method-name order.
///
/// All three are queries: they read the store and change nothing, so their
/// answers carry no revision and invalidate no resource. All three are durable,
/// which is what a method that never thought about cancellation means; a read
/// that finishes in milliseconds has no reason to be torn down.
pub const MESSAGE_READ_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("message.get", MethodKind::Query, 1),
    MethodSpec::new("message.list", MethodKind::Query, 1),
    MethodSpec::new("message.search", MethodKind::Query, 1),
];

/// One of the three, selected by its own [`MethodSpec`].
///
/// One type for three methods because they share their one dependency and
/// differ only in which of the three bodies below they run; the dispatcher
/// registers three instances, so each still declares itself separately.
pub struct MessageReadMethod {
    /// Which of [`MESSAGE_READ_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
}

impl Method for MessageReadMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        // The store read is synchronous, as it was when the server called this
        // method directly. Moving it onto a blocking thread is a change to how
        // the daemon schedules work, not to how it dispatches, so it belongs
        // with the account runtimes of Phase 5.
        Box::pin(async move {
            let accounts = self.config.accounts();
            let result = match self.spec.name {
                "message.get" => get(&params, &accounts),
                "message.list" => list(&params, &accounts),
                _ => search(&params, &accounts),
            };
            result.map(Outcome::query).map_err(DomainError::from)
        })
    }
}

/// Register the three read methods on `dispatcher`.
pub fn register_reads(dispatcher: &mut Dispatcher, config: Arc<super::super::config::ConfigStore>) {
    for spec in MESSAGE_READ_METHOD_SPECS {
        dispatcher.register(Arc::new(MessageReadMethod {
            spec,
            config: Arc::clone(&config),
        }));
    }
}

/// The `result` of `message.list`, in whichever projection was asked for.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    match params.get("projection") {
        None | Some(Value::Null) => list_rows(params, accounts),
        Some(Value::String(name)) if name == "list" => list_rows(params, accounts),
        Some(Value::String(name)) if name == "envelope" => envelopes(params, accounts),
        Some(other) => Err(invalid_params(format!(
            "projection {other} is neither \"list\" nor \"envelope\""
        ))),
    }
}

/// The `list` projection: one mailbox of one account, newest first.
fn list_rows(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let wanted = string_param(params, "mailbox")?;
    let limit = limit_param(params)?;

    let account = super::account::ready_account(accounts, &name)?;
    let mailbox = resolve_mailbox(account, &wanted)?;

    let path = crate::config::store_path(&name);
    let store =
        Store::open(&path).map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let rows = read::list_mailbox(&store, &name, &mailbox)
        .map_err(|e| internal(format!("listing {name}/{mailbox}: {e:#}")))?;

    let total = rows.len();
    let messages: Vec<Value> = rows
        .iter()
        .take(limit.unwrap_or(total))
        .map(to_json)
        .collect();
    Ok(json!({
        "account": name,
        "mailbox": mailbox,
        "total": total,
        "messages": messages,
    }))
}

/// One stored row on the wire.
///
/// `from`, `to`, `subject` and `date_display` travel as `""` rather than
/// `null`, because the shape says `str`. `cc`, `reply_to` and `bcc` are
/// `str|null` instead: the header pane prints each of them only when the
/// message carried one (#0096), so a client's row holds an option and a
/// flattened `""` would make an absent Cc indistinguishable from an empty one.
///
/// `flags` carries the four axes of [`crate::types::MessageFlags`], `flagged`
/// (`\Flagged`, the star of #0007) included since P5-U4: it is what the TUI
/// list renders as its own marker, and the version that adds it is this one.
///
/// Both dates are here because neither can be derived from the other:
/// `date_sort` is `resolve_date`'s UTC sort key, and `date_display` is the
/// `Date:` header as the store holds it, which is the column a listing prints.
/// A client renders from the wire alone rather than reading the store beside
/// the daemon.
///
/// `id` is `messages.id`, the synthetic row key. It is here because it is the
/// identity a TUI client holds for a listed row (`MessageRef`, #0050) and the
/// address it hands back to [`get`]; it is per-store and per-session, it
/// survives no rebuild (`docs/plans/preview-latency.md`, "hole 1"), and a
/// client that persisted one would be naming a row that may since have become
/// another message.
pub fn to_json(row: &MessageRow) -> Value {
    let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
    let flags = row.flags();
    json!({
        "id": row.id,
        "uid": row.uid,
        "message_id": row.message_id,
        "from": row.from.clone().unwrap_or_default(),
        "to": row.to.clone().unwrap_or_default(),
        "cc": row.cc,
        "reply_to": row.reply_to,
        "bcc": row.bcc,
        "subject": row.subject.clone().unwrap_or_default(),
        "date_sort": date_sort,
        "date_display": row.date_display.clone().unwrap_or_default(),
        "flags": {
            "seen": flags.seen,
            "answered": flags.answered,
            "forwarded": flags.forwarded,
            "flagged": flags.flagged,
        },
        "has_attachments": row.has_attachments,
        "is_invite": row.is_invite,
    })
}

/// The `envelope` projection: `mp dump-mailbox --json` for one account.
///
/// Per account and across every selected mailbox in one answer, because the
/// dump's sort key is `(account, mailbox, date_sort, message_id, subject, uid)`
/// and a client that merged per-mailbox answers would be re-implementing it.
/// The records are [`crate::dump::EnvelopeRecord`], so the client re-serialises
/// them with `dump::to_ndjson` and the NDJSON ordering contract, the field order
/// and the null handling stay in the one place that already owns them.
///
/// A mailbox name that is not one of the account's selects nothing rather than
/// failing, exactly as [`crate::dump::collect_records`] treats an unmatched
/// filter: the dump answers about what it found, and a filter is a narrowing
/// rather than an assertion.
fn envelopes(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;
    let filter = mailbox_filter(params)?;
    let records = crate::dump::collect_records(std::slice::from_ref(account), &filter);
    Ok(json!({"account": name, "records": records}))
}

/// The `result` of `message.get`: the record `mp show --json` prints.
///
/// `body` defaults to `true` and its absence is not its nullity: with
/// `body: false` the key is gone, and with the default it is present and `null`
/// when the store holds no readable body for the row. `mp show` prints its "no
/// stored body" sentence for exactly that `null`, so collapsing the two would
/// make a bodyless answer indistinguishable from an evicted blob.
pub fn get(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    super::account::ready_account(accounts, &name)?;
    let wants_body = flag_param(params, "body", true)?;

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let row = address(params, &store, &name)?;
    let blobs = BlobStore::new(crate::config::blobs_dir(&name));
    let shown = crate::read_cmd::shown_message(&store, &blobs, &name, &row);

    let mut result = serde_json::to_value(&shown).map_err(|e| {
        internal(format!(
            "serialising {name}:{}/{}: {e}",
            row.mailbox, row.uid
        ))
    })?;
    if !wants_body {
        if let Some(object) = result.as_object_mut() {
            object.remove("body");
        }
    }
    Ok(result)
}

/// The row `id`, `row_id` or `selector` names: exactly one of them, always.
///
/// None is a caller who forgot and more than one is a caller who may disagree
/// with themselves, so both are `-32602`. So is every way of naming nothing: an
/// unknown uid, a malformed id, a mailbox the account does not have and a
/// selector that resolves to no message are all the caller's parameter being
/// wrong rather than the store failing.
///
/// `row_id` is the `id` a `message.list` row carries, which is what a client
/// holding a listed row already has (P5-U4): the TUI's preview names the row it
/// is on by `messages.id` and nothing else, and resolving it back through
/// `"<mailbox>/<uid>"` would make the client carry a second identity for the
/// same row and re-derive it on every cursor move.
fn address(params: &Value, store: &Store, account: &str) -> Result<MessageRow, RpcError> {
    let addressed = |key: &str| !matches!(params.get(key), None | Some(Value::Null));
    if addressed("row_id") {
        if addressed("id") || addressed("selector") {
            return Err(invalid_params(
                "row_id, id and selector are three addresses; send exactly one",
            ));
        }
        let row_id = params
            .get("row_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid_params("row_id is a messages.id, which is an integer"))?;
        return read::find_by_id(store, row_id)
            .map_err(|e| internal(format!("reading message {row_id}: {e:#}")))?
            .ok_or_else(|| {
                invalid_params(format!("{account} holds no message with row id {row_id}"))
            });
    }
    match (addressed("id"), addressed("selector")) {
        (true, true) => Err(invalid_params(
            "id and selector are two addresses; send exactly one",
        )),
        (false, false) => Err(invalid_params(
            "a message is addressed by id, by row_id or by selector; send exactly one",
        )),
        (true, false) => {
            let (mailbox, uid) = message_param(params)?;
            let id = read::find_row_by_uid(store, account, &mailbox, uid)
                .map_err(|e| internal(format!("looking up {account}:{mailbox}/{uid}: {e:#}")))?
                .ok_or_else(|| {
                    invalid_params(format!("{account} holds no message {mailbox}/{uid}"))
                })?;
            read::find_by_id(store, id)
                .map_err(|e| internal(format!("reading message {id}: {e:#}")))?
                .ok_or_else(|| {
                    invalid_params(format!("{account} holds no message {mailbox}/{uid}"))
                })
        }
        (false, true) => {
            // The grammar `mp show` takes from a user, resolved the way
            // `resolve_received_arg` resolves it, with `mailbox` narrowing it
            // exactly as `mp show --mailbox` does. The refusals are that
            // resolution's own sentences, so a routed `mp show` reports what the
            // pre-daemon one reported.
            let selector = string_param(params, "selector")?;
            let mailbox = params.get("mailbox").and_then(Value::as_str);
            let query = crate::selector::parse_in(&selector, Namespace::Received, account, mailbox)
                .map_err(|e| invalid_params(format!("{e:#}")))?;
            crate::selector::resolve_received(store, &query)
                .map(|(row, _)| row)
                .map_err(|e| invalid_params(format!("{e:#}")))
        }
    }
}

/// The `result` of `message.search`: the ranked hits of the local index.
///
/// The params mirror `mp search --local`'s flags and the query is built with
/// [`crate::search::from_cli`], so one parser serves every backend and the
/// client sends what the user typed. A query the search layer cannot use is
/// `-32602`, not `-32603`: the parameter is wrong, not the store.
pub fn search(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;
    let query = string_param(params, "query")?;

    let flags = crate::search::Flags {
        from: opt_string_param(params, "from")?,
        to: opt_string_param(params, "to")?,
        cc: opt_string_param(params, "cc")?,
        subject: opt_string_param(params, "subject")?,
        // `body_query` is the wire name of `--body`: `body` is already the
        // "send me the bodies" switch, and one key may not mean two things.
        body: opt_string_param(params, "body_query")?,
        filename: opt_string_param(params, "filename")?,
        has_attachment: flag_param(params, "has_attachment", false)?,
        after: opt_string_param(params, "after")?,
        before: opt_string_param(params, "before")?,
    };
    let ast = crate::search::from_cli(&query, &flags).map_err(invalid_params)?;

    // `--mailbox` first, then the query's own `in:` directive, which is the
    // precedence `mp search` applies.
    let wanted = opt_string_param(params, "mailbox")?.or_else(|| ast.in_mailbox.clone());
    let mailbox = match wanted {
        Some(wanted) => Some(resolve_mailbox(account, &wanted)?),
        None => None,
    };
    // Absent means every hit, spelled as a number the store binds rather than
    // as a sentinel the SQL would have to know about.
    let limit = limit_param(params)?.unwrap_or(i64::MAX as usize);

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let hits = crate::store::search::search_ast(&store, &name, &ast, mailbox.as_deref(), limit)
        .map_err(|e| invalid_params(format!("{e:#}")))?;

    let wants_body = flag_param(params, "body", false)?;
    let blobs = BlobStore::new(crate::config::blobs_dir(&name));
    let hits: Vec<Value> = hits
        .iter()
        .map(|hit| {
            // A listing row plus the mailbox it was found in, which is what
            // `Selector::for_message` needs to render the line.
            let mut wire = to_json(&hit.row);
            wire["mailbox"] = json!(hit.row.mailbox);
            if wants_body {
                wire["body"] = json!(read::load_body(&store, &blobs, hit.row.id));
            }
            wire
        })
        .collect();
    Ok(json!({"account": name, "query": query, "hits": hits}))
}

// ---------------------------------------------------------------------------
// The server-side query
// ---------------------------------------------------------------------------

/// `mp fetch`'s method, in its own array (P4-U10).
///
/// A query, because `mp fetch` prints what the server has and writes nothing
/// (#0037): messages enter the store through `mp sync`, which is the only path
/// that fetches by UID and can key a row. It is not in
/// [`MESSAGE_READ_METHOD_SPECS`] because those three read the *store* and this
/// one opens a session, and because that array is pinned at three.
pub const MESSAGE_SERVER_METHOD_SPECS: [MethodSpec; 1] =
    [MethodSpec::new("message.list_server", MethodKind::Query, 1)];

/// `message.list_server` as the dispatcher serves it.
pub struct MessageListServer {
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
}

impl Method for MessageListServer {
    fn spec(&self) -> MethodSpec {
        MESSAGE_SERVER_METHOD_SPECS[0]
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let name = string_param(&params, "account")?;
            let account = super::account::configured_account(&snapshot.accounts, &name)?;
            // `mp fetch --mailbox` defaults to INBOX in the client, so the wire
            // always carries one: a server query with no mailbox names no
            // mailbox at all rather than a default this daemon invented.
            let mailbox = string_param(&params, "mailbox")?;
            let limit = limit_param(&params)?.unwrap_or(10);
            let criteria = fetch_criteria(&params)?;
            super::open_secrets(&name, snapshot.config.secrets_backend)?;
            list_server(account, &mailbox, limit, criteria)
                .await
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// The IMAP search terms `mp fetch`'s filters carry, or the empty set.
///
/// `in_mailbox` and `text` are deliberately not read: the mailbox is a
/// parameter of its own and `--text` belongs to `mp search`, which searches the
/// local index.
fn fetch_criteria(params: &Value) -> Result<crate::imap_client::FetchCriteria, RpcError> {
    let criteria = match params.get("criteria") {
        None | Some(Value::Null) => return Ok(Default::default()),
        Some(Value::Object(_)) => &params["criteria"],
        Some(_) => return Err(invalid_params("criteria is an object of search terms")),
    };
    let term = |key: &str| {
        criteria
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Ok(crate::imap_client::FetchCriteria {
        from: term("from"),
        to: term("to"),
        cc: term("cc"),
        subject: term("subject"),
        body: term("body"),
        since: term("since"),
        before: term("before"),
        text: None,
        message_id: term("message_id"),
        in_mailbox: None,
    })
}

/// The `result` of `message.list_server`: the envelopes and the body text the
/// listing prints, and no path of any kind.
///
/// `--full` never crosses the socket: it selects how much of `body` the client
/// prints, which is a rendering decision over an answer that already carries it.
async fn list_server(
    account: &AccountConfig,
    mailbox: &str,
    limit: usize,
    criteria: crate::imap_client::FetchCriteria,
) -> Result<Value, RpcError> {
    let name = account.name.clone();
    let fetched = if account.auth_method == crate::config::AuthMethod::Graph {
        let config = crate::config::GraphConfig::load(account)
            .map_err(|e| super::server_error(&name, &e))?;
        let client = crate::graph::GraphClient::new_async(&config)
            .await
            .map_err(|e| super::server_error(&name, &e))?;
        client
            .fetch_messages(mailbox, limit)
            .await
            .map_err(|e| super::server_error(&name, &e))?
    } else {
        let imap =
            crate::config::ImapConfig::load(account).map_err(|e| super::server_error(&name, &e))?;
        crate::imap_client::fetch_emails(&imap, &criteria, mailbox, Some(limit))
            .await
            .map_err(|e| super::server_error(&name, &e))?
    };

    let messages: Vec<Value> = fetched.iter().map(fetched_to_json).collect();
    Ok(json!({"account": name, "mailbox": mailbox, "messages": messages}))
}

/// One fetched message on the wire: the six header fields the listing prints,
/// the attachment bit, and the body text it previews.
///
/// The parsed attachments, the HTML alternative and the calendar part stay in
/// the daemon: `mp fetch` prints none of them, and a fetch that writes nothing
/// has nowhere to put them.
fn fetched_to_json(email: &crate::parse::FetchedEmail) -> Value {
    json!({
        "from": email.from,
        "to": email.to,
        "cc": email.cc,
        "subject": email.subject,
        "date": email.date,
        "has_attachments": email.has_attachments,
        "body": email.body_text,
    })
}

// ---------------------------------------------------------------------------
// The mutations
// ---------------------------------------------------------------------------

/// The mailbox an archive moves a message into, the role id `mp archive` has
/// always moved to. Named here rather than imported from the binary, because
/// the daemon is the process that performs the move from P4-U8 on.
const ARCHIVE_MAILBOX: &str = "archive";

/// The two mutations, in method-name order.
///
/// Two methods rather than two flavours of one: archiving moves a row and owes
/// the server a `Move`, deleting drops a row and owes it a `Delete`, and only
/// the first has anything to roll back when the server refuses.
///
/// Both are `Command`: each moves the daemon's revision and invalidates
/// `message:<account>/<mailbox>/<uid>`. Both are `Durable`, which is the whole
/// reason they are not `ClientScoped`: the pre-daemon commands commit the row
/// change and the owed server op in one transaction and then drain that op
/// synchronously (`pending_ops::run_and_settle`, #0039), and a drain torn down
/// because the calling socket went away would leave the op queued while its
/// caller was told nothing.
pub const MESSAGE_MUTATION_METHOD_SPECS: [MethodSpec; 2] = [
    MethodSpec::new("message.archive", MethodKind::Command, 1),
    MethodSpec::new("message.delete", MethodKind::Command, 1),
];

/// One of the two, selected by its own [`MethodSpec`].
pub struct MessageMutationMethod {
    /// Which of [`MESSAGE_MUTATION_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
    /// The state whose revision a command reports.
    pub canonical: Arc<super::super::state::CanonicalState>,
}

impl Method for MessageMutationMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let archive = self.spec.name == "message.archive";
            // On a blocking thread, because a [`Store`] is not `Sync` and this
            // method holds one across the drain's `await`: the local commit and
            // the server op are one unit, and splitting them to satisfy the
            // scheduler would be a change to what the command promises. The
            // drain's own I/O still runs on the daemon's runtime, through the
            // handle this thread blocks on.
            let done = tokio::task::spawn_blocking(move || {
                tokio::runtime::Handle::current().block_on(mutate(&params, &snapshot, archive))
            })
            .await;
            let (result, resource) = match done {
                Ok(result) => result.map_err(DomainError::from)?,
                Err(e) => {
                    return Err(DomainError::internal(format!(
                        "the mutation worker did not finish: {e}"
                    )))
                }
            };
            Ok(Outcome::command(
                result,
                self.canonical.revision().get(),
                vec![ResourceId::new(resource)],
            ))
        })
    }
}

/// Register the two mutations on `dispatcher`.
pub fn register_mutations(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
    canonical: Arc<super::super::state::CanonicalState>,
) {
    for spec in MESSAGE_MUTATION_METHOD_SPECS {
        dispatcher.register(Arc::new(MessageMutationMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
        }));
    }
}

/// The `result` of `message.archive` and `message.delete`, plus the resource
/// the call invalidated.
///
/// The order of the four steps is the pre-daemon command's, and it is the whole
/// safety property of this slice: resolve the account, resolve the message,
/// **resolve the backend**, and only then commit the row change and drain the
/// op it owes. An account with no credentials therefore refuses with the
/// secret store's own sentence and leaves the row exactly where it was, rather
/// than archiving it locally and queueing a move behind a password the user has
/// not entered yet.
async fn mutate(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
    archive: bool,
) -> Result<(Value, String), RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(&snapshot.accounts, &name)?;

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let row = address(params, &store, &name)?;
    let selector = Selector::for_message(&name, &row).to_string();
    let id = format!("{}/{}", row.mailbox, row.uid);
    let resource = format!("message:{name}/{}/{}", row.mailbox, row.uid);
    let source_server = crate::config::find_server_name_for_role(account, &row.mailbox);

    // The secrets backend is opened on first use rather than at startup, the
    // same rule `config.set_password` follows: a first run has no configuration
    // to select one from, and the opener is idempotent.
    if let Err(e) = crate::secrets::init(snapshot.config.secrets_backend) {
        return Err(credentials(&name, &anyhow::anyhow!("{e}")));
    }
    let backend = Backend::resolve(account).map_err(|e| credentials(&name, &e))?;
    let blobs = BlobStore::for_account(&name);
    let gone = || invalid_params(format!("{selector} is no longer in the store"));

    let result = if archive {
        // Through the durable queue, the same seam the TUI drains (#0039): the
        // row moves and the owed server op commit in one transaction, then the
        // op runs synchronously so the caller keeps its blocking UX. On a
        // server refusal `run_and_settle` rolls the row home and propagates the
        // error verbatim.
        let op = ServerOp::Move {
            message_id: row.message_id.clone(),
            source_mailbox: source_server,
            dest_mailbox: crate::config::find_server_name_for_role(account, ARCHIVE_MAILBOX),
        };
        let Some((_previous, op_id)) =
            pending_ops::apply_move(&store, &name, row.id, ARCHIVE_MAILBOX, op)
                .map_err(|e| internal(format!("{e:#}")))?
        else {
            return Err(gone());
        };
        settle(&store, &blobs, op_id, &backend).await?;
        json!({
            "account": name,
            "id": id,
            "selector": selector,
            "mailbox": row.mailbox,
            "moved_to": {
                "mailbox": ARCHIVE_MAILBOX,
                // The Message-ID as the store holds it, which is what the
                // pre-daemon "now" line printed.
                "selector": Selector::new(&name, ARCHIVE_MAILBOX, &row.message_id).to_string(),
            },
        })
    } else {
        // A delete has nothing to roll back (the row is gone and the server
        // still holds the message), so a refusal propagates verbatim and the
        // next sync refetches the UID.
        let op = ServerOp::Delete {
            message_id: row.message_id.clone(),
            source_mailbox: source_server,
        };
        let Some((_previous, op_id)) = pending_ops::apply_delete(&store, &blobs, &name, row.id, op)
            .map_err(|e| internal(format!("{e:#}")))?
        else {
            return Err(gone());
        };
        settle(&store, &blobs, op_id, &backend).await?;
        json!({
            "account": name,
            "id": id,
            "selector": selector,
            "mailbox": row.mailbox,
        })
    };
    Ok((result, resource))
}

/// Run the owed op and settle it, reporting the server's refusal verbatim.
async fn settle(
    store: &Store,
    blobs: &BlobStore,
    op_id: i64,
    backend: &Backend,
) -> Result<(), RpcError> {
    pending_ops::run_and_settle(store, blobs, op_id, backend)
        .await
        .map_err(|e| internal(format!("{e:#}")))
}

/// `-32603` for an account whose credentials cannot be loaded, carrying the
/// account so a client can act on it without reading English.
///
/// Not `-32005` and not `-32006`: the account is configured and its store is
/// readable, so either of those would contradict what `account.list` says about
/// the same account. Not `-32602` either: the caller's parameters were right.
/// Protocol 1 has no credentials code, and inventing one would take a
/// protocol-changelog entry without moving a single user-visible byte.
fn credentials(account: &str, error: &anyhow::Error) -> RpcError {
    RpcError {
        code: super::INTERNAL_ERROR,
        message: format!("{error}"),
        data: Some(json!({ "account": account })),
    }
}

// ---------------------------------------------------------------------------
// Materialised handles
// ---------------------------------------------------------------------------

/// The three handle methods, in method-name order.
///
/// The two materialisers are `ClientIntegration`, because the daemon prepares
/// the file and only the client's own process can open it; the release is a
/// `Query`, because it moves no revision and invalidates no resource, handles
/// being per-client scratch that appears in no snapshot. All three are
/// `Durable`: a viewer holding an open file may not lose it because the socket
/// that asked for it went away, and expiry is what ends a handle.
pub const MESSAGE_HANDLE_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new(
        "message.materialise_attachment",
        MethodKind::ClientIntegration,
        1,
    ),
    MethodSpec::new("message.materialise_html", MethodKind::ClientIntegration, 1),
    MethodSpec::new("message.release_handle", MethodKind::Query, 1),
];

/// One of the three, selected by its own [`MethodSpec`].
///
/// One type for three methods because they share every dependency and differ
/// only in which of the three bodies below they run; the dispatcher registers
/// three instances, so each still declares itself separately.
pub struct MessageHandleMethod {
    /// Which of [`MESSAGE_HANDLE_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
    /// The daemon's one handle table, shared with the sweep that reads its pins.
    pub handles: Arc<HandleTable>,
}

impl Method for MessageHandleMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            // The lazy reaper: every call collects what expired since the last
            // one, so an expired handle is unreleasable and its scratch is gone
            // without a periodic tick to be late.
            reap(&self.handles, Utc::now());
            let result = match self.spec.name {
                "message.materialise_attachment" => materialise(
                    &params,
                    &self.config.accounts(),
                    &self.handles,
                    HandleKind::Attachment,
                ),
                "message.materialise_html" => materialise(
                    &params,
                    &self.config.accounts(),
                    &self.handles,
                    HandleKind::Html,
                ),
                _ => release_handle(&params, &self.handles),
            };
            result.map(Outcome::query).map_err(DomainError::from)
        })
    }
}

/// Register the three handle methods on `dispatcher`.
pub fn register_handles(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
    handles: Arc<HandleTable>,
) {
    for spec in MESSAGE_HANDLE_METHOD_SPECS {
        dispatcher.register(Arc::new(MessageHandleMethod {
            spec,
            config: Arc::clone(&config),
            handles: Arc::clone(&handles),
        }));
    }
}

/// The `result` of the two materialisers:
/// `{handle, path, name, bytes, expires_at}`.
///
/// The message is addressed the way [`get`] addresses one - `{id}` or
/// `{selector, mailbox?}` - which P4-U8 added beside the `{id}` form P3b-U12
/// shipped, because a client holding a selector cannot build
/// `"<mailbox>/<uid>"` out of a `ShownMessage` and re-listing the mailbox to
/// find the uid would be a second query to answer a question the daemon already
/// answers.
fn materialise(
    params: &Value,
    accounts: &[AccountConfig],
    handles: &HandleTable,
    kind: HandleKind,
) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    super::account::ready_account(accounts, &name)?;

    // The store is opened by path, as `message.list` opens it, and no engine
    // lock is taken: materialising is a read plus a write into the daemon's own
    // runtime directory, neither of which makes the daemon an account's engine.
    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let blobs = BlobStore::new(crate::config::blobs_dir(&name));
    let row = address(params, &store, &name)?.id;

    let (filename, bytes, pinned) = match kind {
        HandleKind::Attachment => attachment_file(params, &store, &blobs, row)?,
        HandleKind::Html => html_file(&store, &blobs, row)?,
    };
    write_handle(handles, kind, &name, &filename, &bytes, &pinned)
}

/// `id`, which is `"<mailbox>/<uid>"`.
///
/// The last two thirds of the `message:<account>/<mailbox>/<uid>` resource, and
/// the store's own `UNIQUE (account, mailbox, uid)` key: a client composes it
/// from the mailbox it listed and the `uid` of the row it is holding. The row id
/// was the other candidate and is a rebuild away from meaning another message;
/// the `Message-ID` header was the third and is shared by the Inbox and Sent
/// copies of one message.
fn message_param(params: &Value) -> Result<(String, i64), RpcError> {
    let id = string_param(params, "id")?;
    let malformed = || invalid_params(format!("id {id:?} is not a \"<mailbox>/<uid>\" message id"));
    let (mailbox, uid) = id.rsplit_once('/').ok_or_else(malformed)?;
    let uid: i64 = uid.parse().map_err(|_| malformed())?;
    if mailbox.is_empty() {
        return Err(malformed());
    }
    Ok((mailbox.to_string(), uid))
}

/// The name, the bytes and the pinned blobs of one attachment.
///
/// `part` is a dense zero-based index into the message's user-facing attachment
/// list, which is [`read::attachments_for`]'s order with the iMIP sidecar
/// excluded: it is the index of the row a client is looking at, and addressing
/// by the store's raw `ordinal` would leak the hidden sidecar's position into a
/// list it is deliberately absent from.
fn attachment_file(
    params: &Value,
    store: &Store,
    blobs: &BlobStore,
    row: i64,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let part = params
        .get("part")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_params("part is a required non-negative integer"))?;
    let attachments =
        read::attachments_for(store, row).map_err(|e| internal(format!("reading {row}: {e:#}")))?;
    let attachment = usize::try_from(part)
        .ok()
        .and_then(|part| attachments.get(part))
        .ok_or_else(|| {
            invalid_params(format!(
                "part {part} is not one of this message's {} attachments",
                attachments.len()
            ))
        })?;
    let bytes = read::read_blob(blobs, row, &attachment.hash).ok_or_else(|| {
        internal(format!(
            "the blob behind attachment {} is missing or unreadable",
            attachment.name
        ))
    })?;
    // The stored name is sanitised again here rather than trusted, the same rule
    // `read::materialise_attachments` applies: this is the seam that turns a
    // name a sender chose into a path.
    Ok((
        safe_filename(&crate::parse::sanitize_attachment_filename(
            &attachment.name,
        )),
        bytes,
        vec![attachment.hash.clone()],
    ))
}

/// The browser rendition of one message, and the blobs it read.
///
/// The rendition, not the raw markup: the charset and the CSP tag the TUI's `b`
/// binding injects before it hands a `file://` URL to a browser (#0037), and the
/// `cid:` inlining that makes the referenced parts visible without a message to
/// resolve them against. Serving unhardened markup through a new door would undo
/// that fix at the moment the GUI starts using it.
fn html_file(
    store: &Store,
    blobs: &BlobStore,
    row: i64,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let markup = read::load_html(store, blobs, row)
        .ok_or_else(|| invalid_params("this message carries no HTML to render"))?;
    let raw_hash = read::blob_hash_of_kind(store, row, "raw");
    // The `html` blob when there is one; otherwise the markup came out of the
    // raw message and it is the raw blob that must stay.
    let mut pinned: Vec<String> = read::blob_hash_of_kind(store, row, "html")
        .or_else(|| raw_hash.clone())
        .into_iter()
        .collect();
    let markup = if markup.to_ascii_lowercase().contains("cid:") {
        match read::load_raw(store, blobs, row) {
            Some(raw) => {
                // The inline-image scan parsed the raw message, so this handle
                // holds two blobs rather than one.
                if let Some(hash) = raw_hash.filter(|hash| !pinned.contains(hash)) {
                    pinned.push(hash);
                }
                let images = crate::parse::inline_images(&raw, &markup);
                crate::parse::embed_inline_images(&markup, &images)
            }
            None => markup,
        }
    } else {
        markup
    };
    let rendition = crate::parse::inject_csp_meta(&crate::parse::ensure_utf8_charset(&markup));
    Ok(("message.html".to_string(), rendition.into_bytes(), pinned))
}

/// A sanitised name that is still a name: `.` and `..` are directory entries
/// every path has, and joining either would leave the handle's own directory.
fn safe_filename(name: &str) -> String {
    match name {
        "." | ".." => "attachment.bin".to_string(),
        other => other.to_string(),
    }
}

/// Write one file into its own handle directory and record the handle.
///
/// The id is minted before the write because it names the directory; a write
/// that fails leaves no entry in the table and no directory on disk.
fn write_handle(
    handles: &HandleTable,
    kind: HandleKind,
    account: &str,
    filename: &str,
    bytes: &[u8],
    pinned: &[String],
) -> Result<Value, RpcError> {
    let id = handles.mint_id();
    let dir = handle_dir(&id);
    let path = dir.join(filename);
    let written = (|| -> anyhow::Result<u64> {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
        fs::write(&path, bytes)?;
        Ok(fs::metadata(&path)?.len())
    })();
    let written = match written {
        Ok(written) => written,
        Err(e) => {
            remove_handle_dir(&id);
            return Err(internal(format!("materialising {}: {e:#}", path.display())));
        }
    };

    let handle =
        handles.materialise_with_id(id, kind, account, path.clone(), written, pinned, Utc::now());
    Ok(json!({
        "handle": handle.id.as_str(),
        "path": path.display().to_string(),
        // The sanitised file name, so a client builds its own destination
        // without parsing the daemon's path (P4-U8). The daemon never renames a
        // part: two parts sent under one name come back as two handles carrying
        // that one name, in two directories, and the `_1` rule that turns them
        // into two files belongs where the names become paths, in the client.
        "name": filename,
        // The length of the file, not the size of the backing blob: a rendition
        // grew by its CSP tag, and a client preallocating from this reads a file.
        "bytes": handle.bytes,
        "expires_at": handle.expires_at.to_rfc3339(),
    }))
}

/// The `result` of `message.release_handle`, which is `{}`.
///
/// An unknown, an already released and an expired handle are one answer,
/// `-32602`: all three mean "you are not holding that", and the daemon has no
/// reason to tell a client which of the three it did.
fn release_handle(params: &Value, handles: &HandleTable) -> Result<Value, RpcError> {
    let id = HandleId(string_param(params, "handle")?);
    if !handles.release(&id) {
        return Err(invalid_params(format!(
            "{} is not a live handle",
            id.as_str()
        )));
    }
    // Only ever a directory this daemon minted the name of, so the removal
    // cannot be steered by a caller's string.
    remove_handle_dir(&id);
    Ok(json!({}))
}

/// The dump's mailbox filter: a name, an array of names (the repeatable
/// `--mailbox`), or `null`/absent for every listable mailbox of the account.
fn mailbox_filter(params: &Value) -> Result<Vec<String>, RpcError> {
    let malformed =
        || invalid_params("mailbox is a name, an array of names, or null for every mailbox");
    match params.get("mailbox") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(name)) => Ok(vec![name.clone()]),
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| name.as_str().map(str::to_string).ok_or_else(malformed))
            .collect(),
        Some(_) => Err(malformed()),
    }
}

/// An optional boolean parameter, absent and `null` both meaning `default`.
fn flag_param(params: &Value, name: &str, default: bool) -> Result<bool, RpcError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid_params(format!("{name} is a boolean"))),
    }
}

/// An optional string parameter, absent and `null` both meaning `None`.
fn opt_string_param(params: &Value, name: &str) -> Result<Option<String>, RpcError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_params(format!("{name} is a string"))),
    }
}

/// `limit`, which is optional and unsigned; anything else is `-32602`.
fn limit_param(params: &Value) -> Result<Option<usize>, RpcError> {
    match params.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|limit| Some(limit.min(usize::MAX as u64) as usize))
            .ok_or_else(|| {
                invalid_params("limit is a non-negative integer, or null for every message")
            }),
    }
}

/// The mailbox id behind what the caller asked for.
///
/// Role, slug or sidebar label, exactly as `mp list-messages --mailbox` accepts
/// them, and the answer echoes the resolved id rather than the spelling. An
/// unknown one is `-32602` naming the mailboxes this account has, because the
/// caller asked for something that does not exist rather than for something the
/// daemon refuses.
fn resolve_mailbox(account: &AccountConfig, wanted: &str) -> Result<String, RpcError> {
    let mailboxes: Vec<_> = build_mailboxes(account)
        .into_iter()
        .filter(|mailbox| mailbox.id != DRAFTS_MAILBOX)
        .collect();
    if let Some(hit) = mailboxes
        .iter()
        .find(|m| wanted.eq_ignore_ascii_case(&m.id) || wanted.eq_ignore_ascii_case(&m.label))
    {
        return Ok(hit.id.clone());
    }
    let known: Vec<&str> = mailboxes.iter().map(|m| m.id.as_str()).collect();
    Err(invalid_params(format!(
        "'{wanted}' is not a mailbox of {} (known: {})",
        account.name,
        known.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> AccountConfig {
        AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        }
    }

    /// A role, a label and an unknown name, which is the error that names the
    /// alternatives.
    #[test]
    fn a_mailbox_resolves_by_role_or_by_label_and_never_to_drafts() {
        assert_eq!(resolve_mailbox(&account(), "inbox").unwrap(), "inbox");
        assert_eq!(resolve_mailbox(&account(), "Inbox").unwrap(), "inbox");
        let refused = resolve_mailbox(&account(), "drafts").expect_err("drafts is not listable");
        assert_eq!(refused.code, -32602);
        assert!(refused.message.contains("inbox"), "{}", refused.message);
    }

    /// `null`, absent, `0` and a number are four different answers.
    #[test]
    fn the_limit_parameter_tells_null_from_zero() {
        assert_eq!(limit_param(&json!({})).unwrap(), None);
        assert_eq!(limit_param(&json!({"limit": null})).unwrap(), None);
        assert_eq!(limit_param(&json!({"limit": 0})).unwrap(), Some(0));
        assert_eq!(limit_param(&json!({"limit": 5})).unwrap(), Some(5));
        assert!(limit_param(&json!({"limit": "5"})).is_err());
        assert!(limit_param(&json!({"limit": -1})).is_err());
    }
}
