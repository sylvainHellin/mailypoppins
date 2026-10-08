//! The read slice (`message.get`, `message.list`, `message.search`, P4-U4), and
//! the three methods that turn a stored message into a file a client can open:
//! `message.materialise_attachment`, `message.materialise_html` and
//! `message.release_handle` (P3b-U12).
//!
//! `message.html`: the rendition `message.materialise_html` writes, answered
//! inline instead of as a file, for a webview that has no use for a path. Both
//! are built by one function, [`html_rendition`], so their bytes are the same.
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

use std::collections::VecDeque;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::Utc;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::listing::{ThreadListing, ThreadMessage};
use mp_protocol::{RpcError, MAX_RESPONSE_BYTES};

use crate::config::AccountConfig;
use crate::ops::Backend;
use crate::pending_ops;
use crate::selector::{Namespace, Selector, DRAFTS_MAILBOX};
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};
use crate::tui::app::{build_mailboxes, resolve_date};

use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId, RowsSink,
};
use super::super::handles::{
    handle_dir, reap, remove_handle_dir, HandleId, HandleKind, HandleTable,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::state::ConnectionId;
use super::{internal, invalid_params, only_params, string_param};

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

/// The conversation read, declared on its own (`LST-10`, P5-U10d, #0126).
///
/// A query and durable like the three above, served by the same type, and in a
/// separate array for the reason [`MESSAGE_MARKDOWN_METHOD_SPECS`] is one:
/// `tests/daemon_read_slice.rs` pins [`MESSAGE_READ_METHOD_SPECS`] at exactly
/// the three names P4-U4 shipped, so growing it would edit a pinned test to
/// say something it was not written to say.
pub const MESSAGE_THREAD_METHOD_SPECS: [MethodSpec; 1] =
    [MethodSpec::new("message.thread", MethodKind::Query, 1)];

/// The inline HTML rendition, declared on its own for the reason
/// [`MESSAGE_THREAD_METHOD_SPECS`] is.
///
/// A query rather than `ClientIntegration`, unlike `message.materialise_html`
/// whose bytes it answers: it writes no file, mints no handle and leaves the
/// client nothing to release, so the answer is a read like `message.get`'s.
/// Durable, as every read is: it finishes in the time the store read takes.
pub const MESSAGE_HTML_METHOD_SPECS: [MethodSpec; 1] = [MethodSpec::new(
    mp_protocol::rendition::METHOD_MESSAGE_HTML,
    MethodKind::Query,
    1,
)];

/// One of the five, selected by its own [`MethodSpec`].
///
/// One type for five methods because they share their one dependency and
/// differ only in which of the five bodies below they run; the dispatcher
/// registers five instances, so each still declares itself separately.
pub struct MessageReadMethod {
    /// Which of [`MESSAGE_READ_METHOD_SPECS`],
    /// [`MESSAGE_THREAD_METHOD_SPECS`] or [`MESSAGE_HTML_METHOD_SPECS`] this
    /// instance serves.
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
        // The store reads are synchronous and, but for a whole-mailbox
        // `message.list`, finish in milliseconds on the task that called them.
        // That one listing is the exception: at tens of thousands of rows it is
        // over a hundred milliseconds of SQLite and row building, which would
        // hold a runtime worker every other connection shares, so
        // [`list_off_thread`] moves its `list` projection to the blocking pool.
        Box::pin(async move {
            let accounts = self.config.accounts();
            let result = match self.spec.name {
                "message.get" => get(&params, &accounts),
                "message.list" => list_off_thread(&params, &accounts).await,
                "message.thread" => thread(&params, &accounts),
                mp_protocol::rendition::METHOD_MESSAGE_HTML => html(&params, &accounts),
                _ => search(&params, &accounts),
            };
            result.map(Outcome::query).map_err(DomainError::from)
        })
    }
}

/// Register the five read methods on `dispatcher`.
pub fn register_reads(dispatcher: &mut Dispatcher, config: Arc<super::super::config::ConfigStore>) {
    for spec in MESSAGE_READ_METHOD_SPECS
        .into_iter()
        .chain(MESSAGE_THREAD_METHOD_SPECS)
        .chain(MESSAGE_HTML_METHOD_SPECS)
    {
        dispatcher.register(Arc::new(MessageReadMethod {
            spec,
            config: Arc::clone(&config),
        }));
    }
}

/// The `result` of `message.list`, in whichever projection was asked for.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    if is_envelope_projection(params)? {
        envelopes(params, accounts)
    } else {
        list_read(params, accounts)?.run()
    }
}

/// [`list`] as the dispatcher runs it: the `list` projection's store read on
/// the blocking pool, everything else where it was called.
///
/// Only the read moves. The parameters, the account gate and the store path
/// are resolved here, on the calling thread, so the worker is handed a path
/// rather than resolving one: a test fixture points the data root at a
/// tempdir for its own thread only (`docs/lessons-learned.md`, "The
/// data-root override is thread-local"), and a worker that resolved
/// `store_path` itself would read the developer's own tree. The envelope
/// projection stays on the calling thread for the same reason, since
/// [`crate::dump::collect_records`] resolves its own paths.
async fn list_off_thread(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    if is_envelope_projection(params)? {
        return envelopes(params, accounts);
    }
    let read = list_read(params, accounts)?;
    tokio::task::spawn_blocking(move || read.run())
        .await
        .map_err(|e| internal(format!("the listing worker did not finish: {e}")))?
}

/// Whether `projection` asks for envelopes rather than rows; an unknown one is
/// the caller's mistake.
fn is_envelope_projection(params: &Value) -> Result<bool, RpcError> {
    match params.get("projection") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::String(name)) if name == "list" => Ok(false),
        Some(Value::String(name)) if name == "envelope" => Ok(true),
        Some(other) => Err(invalid_params(format!(
            "projection {other} is neither \"list\" nor \"envelope\""
        ))),
    }
}

/// The `list` projection, resolved and not yet read: one mailbox of one
/// account, newest first.
struct ListRead {
    name: String,
    mailbox: String,
    limit: Option<usize>,
    path: std::path::PathBuf,
}

/// Validate the `list` projection's parameters and gate the account.
fn list_read(params: &Value, accounts: &[AccountConfig]) -> Result<ListRead, RpcError> {
    let name = string_param(params, "account")?;
    let wanted = string_param(params, "mailbox")?;
    let limit = limit_param(params)?;

    let account = super::account::ready_account(accounts, &name)?;
    let mailbox = resolve_mailbox(account, &wanted)?;
    let path = crate::config::store_path(&name);
    Ok(ListRead {
        name,
        mailbox,
        limit,
        path,
    })
}

impl ListRead {
    /// Read the rows and build the answer.
    ///
    /// A `limit` pages in SQL and counts separately; `null`, which is what the
    /// TUI sends, is the whole mailbox, whose length is the total. Each row is
    /// serialised from a [`WireRow`] borrowing the stored one, with its
    /// `date_sort` formatted from the column ingest stamped rather than parsed
    /// out of `date_display` again.
    fn run(self) -> Result<Value, RpcError> {
        let ListRead {
            name,
            mailbox,
            limit,
            path,
        } = self;
        let store = Store::open(&path)
            .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
        let (rows, total) = read::list_mailbox_dated(&store, &name, &mailbox, limit)
            .map_err(|e| internal(format!("listing {name}/{mailbox}: {e:#}")))?;
        let messages = rows
            .iter()
            .map(|(row, stamped)| {
                serde_json::to_value(WireRow::new(&name, row, wire_date_sort(row, *stamped)))
            })
            .collect::<Result<Vec<Value>, _>>()
            .map_err(|e| internal(format!("serialising {name}/{mailbox}: {e}")))?;
        Ok(json!({
            "account": name,
            "mailbox": mailbox,
            "total": total,
            "messages": messages,
        }))
    }
}

/// The wire `date_sort` of a listed row, from the `date_sort` column.
///
/// The column is the unix time ingest derived from the `Date:` header with
/// the RFC 2822 parser [`resolve_date`] uses, so formatting it in UTC is the
/// string `resolve_date` would have returned, without the parse. `0` and
/// `NULL` are the column's "no parsable date" (and the one real date that
/// stamps as `0`, the epoch itself), so those rows, rare by construction, take
/// `resolve_date` and keep its answer exactly. So does a header with a leap
/// second (`23:59:60`): chrono folds it into the unix time but prints the
/// `60` from the parsed value, which only the parse can give back.
fn wire_date_sort(row: &MessageRow, stamped: Option<i64>) -> String {
    use chrono::{Datelike, Timelike};

    let at = stamped
        .filter(|secs| *secs != 0 && !row.date_display.as_deref().unwrap_or("").contains(":60"))
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0));
    match at {
        // `%Y` pads to four digits inside this range and signs outside it, so
        // the plain integer format is chrono's own spelling here and is several
        // times cheaper than interpreting a strftime string per row.
        Some(at) if (0..=9999).contains(&at.year()) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            at.year(),
            at.month(),
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        ),
        Some(at) => at.format("%Y-%m-%dT%H:%M:%S").to_string(),
        None => resolve_date(&row.date_display, &None, Path::new("")).1,
    }
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
///
/// `selector` is the canonical `mp://<account>/<mailbox>/<key>` of the row,
/// rendered here by [`Selector::for_message`] rather than composed by a client
/// out of `message_id` and the answer's mailbox (`RD-07`, #0126). A client
/// could compose it, since `mp_core::selector` is a shared module; it may not,
/// because that would be a second implementation of the percent-encoding and
/// of `message_key`'s normalisation, and `tests/cli_selector_contract.rs` pins
/// only the CLI's spelling. The cost is the sixteenth key of a row, about
/// forty bytes of ASCII, paid once per listing rather than once per copy.
///
/// A row read without its `date_sort` column, which is what a search hit is,
/// takes it from [`resolve_date`]; a listing formats the column instead
/// ([`ListRead::run`]). Both serialise the one [`WireRow`].
pub fn to_json(account: &str, row: &MessageRow) -> Value {
    let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
    serde_json::to_value(WireRow::new(account, row, date_sort))
        .expect("a row of strings, integers and booleans serialises")
}

/// [`to_json`]'s row, borrowing the stored one rather than cloning it.
///
/// The fields are declared in key order on purpose: `serde_json` without
/// `preserve_order` sorts an object's keys, which is the order the `json!`
/// literal this replaced put on the wire, so a row serialises to the same
/// bytes whichever way it is built (`a_wire_row_is_the_json_literal_it_replaced`).
#[derive(serde::Serialize)]
struct WireRow<'a> {
    bcc: Option<&'a str>,
    cc: Option<&'a str>,
    date_display: &'a str,
    date_sort: String,
    flags: WireFlags,
    from: &'a str,
    has_attachments: bool,
    id: i64,
    is_invite: bool,
    message_id: &'a str,
    reply_to: Option<&'a str>,
    #[serde(serialize_with = "serialize_display")]
    selector: Selector,
    subject: &'a str,
    to: &'a str,
    uid: i64,
}

/// The four flag axes of a [`WireRow`], in key order for the same reason.
#[derive(serde::Serialize)]
struct WireFlags {
    answered: bool,
    flagged: bool,
    forwarded: bool,
    seen: bool,
}

impl<'a> WireRow<'a> {
    fn new(account: &str, row: &'a MessageRow, date_sort: String) -> Self {
        let flags = row.flags();
        WireRow {
            bcc: row.bcc.as_deref(),
            cc: row.cc.as_deref(),
            date_display: row.date_display.as_deref().unwrap_or_default(),
            date_sort,
            flags: WireFlags {
                answered: flags.answered,
                flagged: flags.flagged,
                forwarded: flags.forwarded,
                seen: flags.seen,
            },
            from: row.from.as_deref().unwrap_or_default(),
            has_attachments: row.has_attachments,
            id: row.id,
            is_invite: row.is_invite,
            message_id: &row.message_id,
            reply_to: row.reply_to.as_deref(),
            selector: Selector::for_message(account, row),
            subject: row.subject.as_deref().unwrap_or_default(),
            to: row.to.as_deref().unwrap_or_default(),
            uid: row.uid,
        }
    }
}

/// A field serialised as its `Display` string, without a `String` in between.
fn serialize_display<S: serde::Serializer>(
    value: &impl std::fmt::Display,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

// ---------------------------------------------------------------------------
// The streamed listing (#0138)
// ---------------------------------------------------------------------------

/// The streamed whole-mailbox listing, declared on its own for the reason
/// [`MESSAGE_HTML_METHOD_SPECS`] is.
///
/// An operation, because the rows travel after the answer as `message.rows`
/// chunks and the stream settles with `operation.finished`; `client_scoped`,
/// because the rows are addressed to one connection and once it is gone
/// nobody can read them.
pub const MESSAGE_STREAM_METHOD_SPECS: [MethodSpec; 1] = [MethodSpec::new(
    mp_protocol::listing::METHOD_MESSAGE_LIST_STREAM,
    MethodKind::Operation,
    1,
)
.client_scoped()];

/// The byte budget of one chunk's rows: a chunk closes once its encoded rows
/// reach it, so a chunk frame is at most this plus one row plus the envelope,
/// sixteen times under [`MAX_RESPONSE_BYTES`].
///
/// A byte budget rather than a row count, because a row's size varies with its
/// subject and recipients and the cap is in bytes. At the 488 bytes a row of
/// `examples/mkfixture.rs` costs, it is about 2150 rows a chunk.
pub const ROWS_CHUNK_BYTES: usize = 1 << 20;

/// Test hook: overrides [`ROWS_CHUNK_BYTES`], in bytes, so a test streams
/// several chunks out of a few thousand rows. Unset, unparseable or zero means
/// the default: a daemon may not change behaviour over a stray variable.
///
/// Read once, when the method is registered, on the
/// [`HANDLE_TTL_ENV`](super::super::handles::HANDLE_TTL_ENV) precedent.
pub const ROWS_CHUNK_BYTES_ENV: &str = "MAILYPOPPINS_DAEMON_ROWS_CHUNK_BYTES";

/// [`ROWS_CHUNK_BYTES`], with [`ROWS_CHUNK_BYTES_ENV`] honoured.
fn rows_chunk_bytes() -> usize {
    std::env::var(ROWS_CHUNK_BYTES_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|bytes| *bytes > 0)
        .unwrap_or(ROWS_CHUNK_BYTES)
}

/// `message.list_stream`: one whole mailbox, read before the answer and
/// streamed to the calling connection after it.
pub struct MessageListStream {
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
    /// The registry the stream's operation is started in.
    pub operations: Arc<OperationRegistry>,
    /// The chunk budget, [`ROWS_CHUNK_BYTES`] unless a test lowered it.
    pub chunk_bytes: usize,
}

impl Method for MessageListStream {
    fn spec(&self) -> MethodSpec {
        MESSAGE_STREAM_METHOD_SPECS[0]
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let spec = self.spec();
            only_params(spec.name, &params, &["account", "mailbox"])?;
            let accounts = self.config.accounts();
            // The parameters, the account gate and the store path resolve on
            // the calling thread, for the reason `list_off_thread` gives: the
            // data-root override of a test fixture is thread-local.
            let ListRead {
                name,
                mailbox,
                path,
                ..
            } = list_read(&params, &accounts)?;
            let Some(sink) = ctx.rows.clone() else {
                return Err(DomainError::internal(format!(
                    "{} needs a connection to stream to, and this caller has none",
                    spec.name
                )));
            };

            // The whole read before the answer, on the blocking pool: every
            // refusal is the call's own error, no operation id is issued for a
            // call that cannot succeed, and `total` is exactly what the stream
            // will carry.
            let (name, mailbox, rows) = tokio::task::spawn_blocking(move || {
                read_dated(&name, &mailbox, &path).map(|rows| (name, mailbox, rows))
            })
            .await
            .map_err(|e| internal(format!("the listing worker did not finish: {e}")))??;
            let total = rows.len();

            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                spec.cancel_scope,
                spec.name,
            );
            handle.set_running();
            let answer = json!({
                "operation_id": id.as_str(),
                "account": name,
                "mailbox": mailbox,
                "total": total,
            });
            let job = StreamJob {
                account: name,
                mailbox,
                rows,
                handle,
                sink,
                chunk_bytes: self.chunk_bytes,
            };
            tokio::task::spawn_blocking(move || job.run());
            Ok(Outcome::query(answer))
        })
    }
}

/// Register `message.list_stream` on `dispatcher`.
pub fn register_stream(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
    operations: Arc<OperationRegistry>,
) {
    dispatcher.register(Arc::new(MessageListStream {
        config,
        operations,
        chunk_bytes: rows_chunk_bytes(),
    }));
}

/// Every row of one mailbox, newest first, with its stamped `date_sort`.
fn read_dated(name: &str, mailbox: &str, path: &Path) -> Result<Vec<read::DatedRow>, RpcError> {
    let store =
        Store::open(path).map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let (rows, _total) = read::list_mailbox_dated(&store, name, mailbox, None)
        .map_err(|e| internal(format!("listing {name}/{mailbox}: {e:#}")))?;
    Ok(rows)
}

/// One stream's producer: the owned rows of the read, encoded one chunk at a
/// time and handed to the connection's writer.
///
/// Runs on the blocking pool, so the encoding stays off the runtime workers
/// the way the read does, and parks in [`RowsSink::blocking_send`] when the
/// connection's channel is full, so a client that stops reading costs four
/// chunks and no more.
struct StreamJob {
    account: String,
    mailbox: String,
    rows: Vec<read::DatedRow>,
    handle: OperationHandle,
    sink: RowsSink,
    chunk_bytes: usize,
}

impl StreamJob {
    /// Stream every row and settle the operation.
    ///
    /// The token is observed between chunks. A cancel has already settled the
    /// operation by the time the token reads shut, and a connection that is
    /// gone is cancelled by its disconnect, so neither path settles anything
    /// here; `operation.finished` is published only after the last chunk is in
    /// the channel, which is half of what puts every chunk ahead of a
    /// `succeeded` finish on the wire.
    fn run(self) {
        let StreamJob {
            account,
            mailbox,
            rows,
            handle,
            sink,
            chunk_bytes,
        } = self;
        let token = handle.token.clone();
        let id = handle.id();
        let mut chunks = ChunkEncoder::new(id.as_str(), chunk_bytes, MAX_RESPONSE_BYTES);
        let send = |chunks: &mut ChunkEncoder| -> bool {
            while let Some(frame) = chunks.next_frame() {
                if token.is_cancelled() || !sink.blocking_send(frame, token.clone()) {
                    return false;
                }
            }
            true
        };

        for (index, (row, stamped)) in rows.iter().enumerate() {
            let wire = WireRow::new(&account, row, wire_date_sort(row, *stamped));
            if let Err(error) = chunks.push(&wire) {
                handle.fail(error.into_domain(&account, &mailbox, index));
                return;
            }
            if !send(&mut chunks) {
                return;
            }
        }
        chunks.finish();
        if !send(&mut chunks) {
            return;
        }
        handle.succeed(json!({
            "account": account,
            "mailbox": mailbox,
            "total": rows.len(),
        }));
    }
}

/// What closes a `message.rows` frame, after its last row.
const ROWS_FRAME_SUFFIX: &[u8] = b"]}}\n";

/// Why a row could not be put into a chunk.
#[derive(Debug, PartialEq)]
enum ChunkError {
    /// The row's own frame, alone in a chunk, is over the response cap.
    TooLarge { limit: usize, seen: usize },
    /// `serde_json` refused the row, which a row of strings, integers and
    /// booleans never makes it do.
    Encode(String),
}

impl ChunkError {
    /// The error the stream's operation fails with.
    fn into_domain(self, account: &str, mailbox: &str, index: usize) -> DomainError {
        match self {
            ChunkError::TooLarge { limit, seen } => DomainError::new(
                mp_protocol::ErrorCode::FrameTooLarge,
                format!(
                    "row {index} of {account}/{mailbox} needs a {seen}-byte frame, \
                     over the {limit}-byte response cap"
                ),
                Some(json!({"limit": limit, "seen": seen})),
            ),
            ChunkError::Encode(error) => DomainError::internal(format!(
                "serialising row {index} of {account}/{mailbox}: {error}"
            )),
        }
    }
}

/// Builds `message.rows` frames as bytes, one chunk at a time.
///
/// The envelope is written by hand and each row with `serde_json::to_writer`,
/// so no `Value` tree is built for a row. The keys are in sorted order at
/// every level, which is what `serde_json` without `preserve_order` writes for
/// a `Value`, so a frame is byte for byte what `frame::encode` of the same
/// notification as a `Value` produces
/// (`a_chunk_frame_is_frame_encode_of_the_same_notification`).
struct ChunkEncoder {
    /// The operation id, already a JSON string literal.
    operation_id: String,
    /// A chunk closes once its rows reach this many bytes.
    budget: usize,
    /// No frame may exceed this, terminator included.
    cap: usize,
    /// The open chunk's frame so far, envelope included.
    buf: Vec<u8>,
    /// How much of `buf` is the envelope prefix.
    prefix_len: usize,
    /// The stream position of the open chunk's first row.
    offset: usize,
    /// How many rows the open chunk holds.
    count: usize,
    /// Closed frames not yet handed out.
    ready: VecDeque<Vec<u8>>,
}

impl ChunkEncoder {
    fn new(operation_id: &str, budget: usize, cap: usize) -> Self {
        ChunkEncoder {
            operation_id: Value::String(operation_id.to_string()).to_string(),
            budget,
            cap,
            buf: Vec::new(),
            prefix_len: 0,
            offset: 0,
            count: 0,
            ready: VecDeque::new(),
        }
    }

    /// Start a chunk at the current offset.
    fn open(&mut self) {
        use std::io::Write as _;

        self.buf = Vec::with_capacity(self.budget.saturating_add(self.budget / 8).min(self.cap));
        // Writing into a `Vec` cannot fail.
        let _ = write!(
            self.buf,
            r#"{{"jsonrpc":"2.0","method":"{}","params":{{"offset":{},"operation_id":{},"rows":["#,
            mp_protocol::METHOD_MESSAGE_ROWS,
            self.offset,
            self.operation_id
        );
        self.prefix_len = self.buf.len();
    }

    /// Append one row, closing the chunk once its rows reach the budget.
    ///
    /// A row that would push a non-empty chunk over the cap closes that chunk
    /// without it and opens the next one with it, so every frame stays under
    /// the cap however the budget and the row sizes fall; a row whose frame
    /// would be over the cap even alone is [`ChunkError::TooLarge`].
    fn push(&mut self, row: &impl serde::Serialize) -> Result<(), ChunkError> {
        if self.count == 0 {
            self.open();
        }
        let mark = self.buf.len();
        if self.count > 0 {
            self.buf.push(b',');
        }
        let start = self.buf.len();
        serde_json::to_writer(&mut self.buf, row).map_err(|e| ChunkError::Encode(e.to_string()))?;
        if self.buf.len() + ROWS_FRAME_SUFFIX.len() > self.cap {
            if self.count > 0 {
                let bytes = self.buf[start..].to_vec();
                self.buf.truncate(mark);
                self.close();
                self.open();
                self.buf.extend_from_slice(&bytes);
            }
            let seen = self.buf.len() + ROWS_FRAME_SUFFIX.len();
            if seen > self.cap {
                // Nothing of this chunk is handed out: the operation fails.
                self.buf.clear();
                return Err(ChunkError::TooLarge {
                    limit: self.cap,
                    seen,
                });
            }
        }
        self.count += 1;
        if self.buf.len() - self.prefix_len >= self.budget {
            self.close();
        }
        Ok(())
    }

    /// Close the open chunk, which must hold at least one row.
    fn close(&mut self) {
        self.buf.extend_from_slice(ROWS_FRAME_SUFFIX);
        self.ready.push_back(std::mem::take(&mut self.buf));
        self.offset += self.count;
        self.count = 0;
    }

    /// Close the last chunk, if it holds anything: a stream of zero rows
    /// sends no chunk at all.
    fn finish(&mut self) {
        if self.count > 0 {
            self.close();
        }
    }

    /// The oldest closed frame not yet handed out.
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }
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
pub(super) fn address(
    params: &Value,
    store: &Store,
    account: &str,
) -> Result<MessageRow, RpcError> {
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

/// The `result` of `message.thread`: the conversation the addressed message
/// belongs to, oldest first (`LST-10`, #0126).
///
/// The grouping is [`read::thread_messages`]'s, which is the set of rows ingest
/// gave the same `thread_id`: decided once at ingest and read back off the
/// `messages_thread` index rather than recomputed from headers here. A message ingest
/// assigned no thread to is the root of its own, so the id falls back to its
/// own `Message-ID`, and the answer is that message alone.
///
/// A row type of its own rather than [`to_json`]'s: a listing names its mailbox
/// once and every row of it is in that mailbox, while a conversation holds the
/// Inbox copy and the archived original side by side, so each row says which
/// mailbox it is in. It carries nothing the overlay does not render - no `uid`,
/// no `selector`, no recipients, and no `date_sort`, because the daemon orders
/// the conversation and a client that re-sorted it would be inventing an order
/// the overlay does not have.
///
/// `current` is decided on the `Message-ID` rather than on the row id, because
/// the fold keeps the first copy its order yields: the surviving row for the
/// addressed message can carry an `id` the call did not name, and a client
/// computing the flag from what it asked about would mark nothing.
pub fn thread(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    super::account::ready_account(accounts, &name)?;

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let row = address(params, &store, &name)?;
    let thread_id = row
        .thread_id
        .clone()
        .unwrap_or_else(|| row.message_id.clone());
    let mut rows = read::thread_messages(&store, &name, &thread_id).map_err(|e| {
        internal(format!(
            "folding the conversation {thread_id} of {name}: {e:#}"
        ))
    })?;
    // The addressed message is always in its own conversation. The fold reads
    // the `thread_id` column, so a row ingest left `NULL` there matches
    // nothing and would answer an empty array, which is the different claim
    // that the store holds not even the message that was addressed.
    if rows.is_empty() {
        rows.push(row.clone());
    }

    let messages: Vec<ThreadMessage> = rows
        .iter()
        .map(|member| {
            let flags = member.flags();
            ThreadMessage {
                id: member.id,
                mailbox: member.mailbox.clone(),
                message_id: member.message_id.clone(),
                from: member.from.clone().unwrap_or_default(),
                date_display: member.date_display.clone().unwrap_or_default(),
                flags: mp_protocol::listing::MessageFlags {
                    seen: flags.seen,
                    answered: flags.answered,
                    forwarded: flags.forwarded,
                    flagged: flags.flagged,
                },
                current: member.message_id == row.message_id,
            }
        })
        .collect();

    let listing = ThreadListing {
        account: name.clone(),
        thread_id,
        // The opened message's own subject, `""` when it carried none: the
        // overlay's title renders its own placeholder and the wire invents
        // none.
        subject: row.subject.clone().unwrap_or_default(),
        messages,
    };
    serde_json::to_value(&listing)
        .map_err(|e| internal(format!("serialising the conversation of {name}: {e}")))
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
            let mut wire = to_json(&name, &hit.row);
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
// The invitation reads (P5-U10)
// ---------------------------------------------------------------------------

/// The two invitation reads, in method-name order and in an array of their own
/// for the reason [`MESSAGE_QUEUE_METHOD_SPECS`] is one: the arrays above are
/// pinned by tests written about the slices that created them.
///
/// ```text
/// message.ics    {account, id|row_id|selector, mailbox?}
///                    -> {account, row_id, ics: <base64>|null}
/// message.invite {account, id|row_id|selector, mailbox?}
///                    -> {account, row_id, event: EventFrontmatter|null}
/// ```
///
/// Two methods rather than one with a projection, because they answer two
/// questions about one row and only one of them is a fold: `message.ics` hands
/// out the row's raw `invite.ics` blob, which is what an RSVP reply is built
/// from, and `message.invite` hands out the *card*, the payload with the
/// account's REPLY rows and its cancellation chain folded onto it (#0031).
///
/// Both are queries and both answer `null` rather than refusing when the row
/// carries no invitation: a non-invite is the ordinary case, not an error, and
/// the preview shows no card for it exactly as it always did.
///
/// The blob travels base64-encoded, in the standard alphabet with padding: an
/// ics payload is text in practice but is a byte blob on the wire (#0038 item
/// 6 stores it as one), and a JSON string cannot carry a byte that is not
/// valid UTF-8.
pub const MESSAGE_INVITE_METHOD_SPECS: [MethodSpec; 2] = [
    MethodSpec::new("message.ics", MethodKind::Query, 1),
    MethodSpec::new("message.invite", MethodKind::Query, 1),
];

/// One of the two, selected by its own [`MethodSpec`].
pub struct MessageInviteMethod {
    /// Which of [`MESSAGE_INVITE_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
}

impl Method for MessageInviteMethod {
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
            let accounts = self.config.accounts();
            let folded = self.spec.name == "message.invite";
            invite(&params, &accounts, folded)
                .map(Outcome::query)
                .map_err(DomainError::from)
        })
    }
}

/// Register the two invitation reads on `dispatcher`.
pub fn register_invites(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
) {
    for spec in MESSAGE_INVITE_METHOD_SPECS {
        dispatcher.register(Arc::new(MessageInviteMethod {
            spec,
            config: Arc::clone(&config),
        }));
    }
}

/// The `result` of `message.invite` (`folded`) and of `message.ics`.
pub fn invite(params: &Value, accounts: &[AccountConfig], folded: bool) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(accounts, &name)?;
    let self_address = crate::parse::extract_email_address(&account.default_from);

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let row = address(params, &store, &name)?;
    let blobs = BlobStore::for_account(&name);

    if folded {
        let event =
            crate::reconcile::event_for_message(&store, &blobs, &name, row.id, &self_address);
        let event = match event {
            Some(event) => serde_json::to_value(event)
                .map_err(|e| internal(format!("serialising the invitation of {name}: {e}")))?,
            None => Value::Null,
        };
        return Ok(json!({"account": name, "row_id": row.id, "event": event}));
    }

    let ics = read::load_invite_ics(&store, &blobs, row.id)
        .map(|bytes| Value::String(BASE64.encode(bytes)))
        .unwrap_or(Value::Null);
    Ok(json!({"account": name, "row_id": row.id, "ics": ics}))
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

/// The three mutations a client drives from a list row, in method-name order
/// (P5-U6, `docs/parity-matrix.md` MSG-03/04/05).
///
/// An array of their own rather than three more entries in
/// [`MESSAGE_MUTATION_METHOD_SPECS`], for the reason
/// [`MESSAGE_SERVER_METHOD_SPECS`] is one: that array is pinned at two by
/// `tests/daemon_mutation_slice.rs`, whose `MUTATION_METHODS` is the P4-U8
/// slice's own list and is checked at compile time. Growing it would edit a
/// pinned test to say something it was not written to say; these three are a
/// later slice and declare themselves separately.
///
/// All three are `Command`: each moves the revision and invalidates
/// `message:<account>/<mailbox>/<uid>`. All three are `Durable`, the same
/// reason the two above are: a caller that asked to settle its op may not have
/// the drain torn down because its socket went away.
pub const MESSAGE_QUEUE_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("message.move", MethodKind::Command, 1),
    MethodSpec::new("message.set_flag", MethodKind::Command, 1),
    MethodSpec::new("message.set_read", MethodKind::Command, 1),
];

/// One of the five, selected by its own [`MethodSpec`].
pub struct MessageMutationMethod {
    /// Which of [`MESSAGE_MUTATION_METHOD_SPECS`] or
    /// [`MESSAGE_QUEUE_METHOD_SPECS`] this instance serves.
    pub spec: MethodSpec,
    /// The live configuration, so a reload is visible to the next call.
    pub config: Arc<super::super::config::ConfigStore>,
    /// The state whose revision a command reports.
    pub canonical: Arc<super::super::state::CanonicalState>,
    /// The account runtimes, so a queued mutation can ask the one serving its
    /// account for a drain (#0133).
    pub runtimes: Arc<super::super::server::RuntimeTable>,
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
            let kind = Kind::of(self.spec.name);
            // Read before `params` moves into the worker. A malformed `settle`
            // is refused inside `mutate`, so this never drains for one.
            let queued = params.get("settle").and_then(Value::as_bool) == Some(false);
            let account = params
                .get("account")
                .and_then(Value::as_str)
                .map(str::to_string);
            // On a blocking thread, because a [`Store`] is not `Sync` and this
            // method holds one across the drain's `await`: the local commit and
            // the server op are one unit, and splitting them to satisfy the
            // scheduler would be a change to what the command promises. The
            // drain's own I/O still runs on the daemon's runtime, through the
            // handle this thread blocks on.
            let done = tokio::task::spawn_blocking(move || {
                tokio::runtime::Handle::current().block_on(mutate(&params, &snapshot, kind))
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
            // The op is committed and owed; ask the account's runtime to drain
            // it once the burst this call may be part of goes quiet (#0133).
            // Only a request: the answer does not wait for the server.
            if queued {
                if let Some(runtime) = account.and_then(|name| self.runtimes.get(&name)) {
                    runtime.request_drain();
                }
            }
            Ok(Outcome::command(
                result,
                self.canonical.revision().get(),
                vec![ResourceId::new(resource)],
            ))
        })
    }
}

/// Register the five mutations on `dispatcher`.
pub fn register_mutations(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
    canonical: Arc<super::super::state::CanonicalState>,
    runtimes: Arc<super::super::server::RuntimeTable>,
) {
    for spec in MESSAGE_MUTATION_METHOD_SPECS
        .into_iter()
        .chain(MESSAGE_QUEUE_METHOD_SPECS)
    {
        dispatcher.register(Arc::new(MessageMutationMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
            runtimes: Arc::clone(&runtimes),
        }));
    }
}

/// Which of the five mutations a call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Archive,
    Delete,
    Move,
    SetFlag,
    SetRead,
}

impl Kind {
    /// The kind a registered method name is. The fallthrough is `set_read`
    /// rather than a panic because only [`register_mutations`] mints these and
    /// it mints exactly the five names below.
    fn of(method: &str) -> Kind {
        match method {
            "message.archive" => Kind::Archive,
            "message.delete" => Kind::Delete,
            "message.move" => Kind::Move,
            "message.set_flag" => Kind::SetFlag,
            _ => Kind::SetRead,
        }
    }
}

/// The `result` of one of the five mutations, plus the resource the call
/// invalidated.
///
/// The order of the four steps is the pre-daemon command's, and it is the whole
/// safety property of this slice: resolve the account, resolve the message,
/// **resolve the backend**, and only then commit the row change and drain the
/// op it owes. An account with no credentials therefore refuses with the
/// secret store's own sentence and leaves the row exactly where it was, rather
/// than archiving it locally and queueing a move behind a password the user has
/// not entered yet.
///
/// # `settle`
///
/// That third step is what `settle: false` skips (P5-U6). It defaults to
/// `true`, which is `mp archive`'s blocking UX unchanged, and a client that
/// sends `false` gets the interactive contract instead: the row change and the
/// owed server op commit in one transaction (#0039), and the method asks the
/// account's runtime for a drain, which runs once the account's mutations have
/// been quiet for [`crate::daemon::runtime::drainer::DRAIN_DEBOUNCE`] (#0133);
/// a sync tick that comes first drains it at its head or tail instead. The TUI has never waited for a server on a keystroke - it is why
/// `u` over a thousand-message selection costs no network - and a mutation that
/// resolved credentials would also refuse outright on an account whose password
/// is not in the keyring yet, where the pre-daemon TUI wrote the local half and
/// carried on.
///
/// No credential is resolved on that path, which is deliberate: the drain
/// resolves one when it runs, and a client that queues has asked for exactly
/// that.
async fn mutate(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
    kind: Kind,
) -> Result<(Value, String), RpcError> {
    let name = string_param(params, "account")?;
    let account = super::account::ready_account(&snapshot.accounts, &name)?;

    // The new state, read before anything is opened: a caller who named none is
    // `-32602` over an untouched store.
    let destination = match kind {
        Kind::Archive => Some(ARCHIVE_MAILBOX.to_string()),
        Kind::Move => Some(destination_param(params, account)?),
        _ => None,
    };
    let read = match kind {
        Kind::SetRead => Some(bool_param(params, "read")?),
        _ => None,
    };
    let flagged = match kind {
        Kind::SetFlag => Some(bool_param(params, "flagged")?),
        _ => None,
    };
    let settle_owed = flag_param(params, "settle", true)?;

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let row = address(params, &store, &name)?;
    let selector = Selector::for_message(&name, &row).to_string();
    let id = format!("{}/{}", row.mailbox, row.uid);
    let resource = format!("message:{name}/{}/{}", row.mailbox, row.uid);
    let source_server = crate::config::find_server_name_for_role(account, &row.mailbox);

    let backend = if settle_owed {
        // The secrets backend is opened on first use rather than at startup,
        // the same rule `config.set_password` follows: a first run has no
        // configuration to select one from, and the opener is idempotent.
        if let Err(e) = crate::secrets::init(snapshot.config.secrets_backend) {
            return Err(credentials(&name, &anyhow::anyhow!("{e}")));
        }
        Some(Backend::resolve(account).map_err(|e| credentials(&name, &e))?)
    } else {
        None
    };
    let blobs = BlobStore::for_account(&name);
    let gone = || invalid_params(format!("{selector} is no longer in the store"));
    let rows = [row.id];

    // Through the durable queue in every case, which is the one place a row
    // change and the [`crate::ops::ServerOp`] it owes are committed together
    // (#0039). On a server refusal `run_and_settle` rolls a move home and
    // propagates the error verbatim; a delete has nothing to roll back (the row
    // is gone and the server still holds the message), so the next sync
    // refetches the UID.
    let (queued, result) = match kind {
        Kind::Archive | Kind::Move => {
            let destination = destination.expect("an archive and a move both name a mailbox");
            let dest_server = crate::config::find_server_name_for_role(account, &destination);
            let queued = crate::mutations::queue_move(
                &store,
                &name,
                &rows,
                &destination,
                &source_server,
                &dest_server,
            );
            let result = json!({
                "account": name,
                "id": id,
                "selector": selector,
                "mailbox": row.mailbox,
                "moved_to": {
                    "mailbox": destination,
                    // The Message-ID as the store holds it, which is what the
                    // pre-daemon "now" line printed.
                    "selector": Selector::new(&name, &destination, &row.message_id).to_string(),
                },
            });
            (queued, result)
        }
        Kind::Delete => {
            let queued =
                crate::mutations::queue_delete(&store, &blobs, &name, &rows, &source_server);
            let result = json!({
                "account": name,
                "id": id,
                "selector": selector,
                "mailbox": row.mailbox,
            });
            (queued, result)
        }
        Kind::SetRead => {
            let read = read.expect("message.set_read reads its new state");
            let queued =
                crate::mutations::queue_read_flag(&store, &name, &rows, read, &source_server);
            let result = json!({
                "account": name,
                "id": id,
                "selector": selector,
                "mailbox": row.mailbox,
                "read": read,
            });
            (queued, result)
        }
        Kind::SetFlag => {
            let flagged = flagged.expect("message.set_flag reads its new state");
            let queued =
                crate::mutations::queue_flag(&store, &name, &rows, flagged, &source_server);
            let result = json!({
                "account": name,
                "id": id,
                "selector": selector,
                "mailbox": row.mailbox,
                "flagged": flagged,
            });
            (queued, result)
        }
    };

    let Some(queued) = queued.first() else {
        return Err(gone());
    };
    if let Some(backend) = backend {
        settle(&store, &blobs, queued.op_id, &backend).await?;
    }
    Ok((result, resource))
}

/// `destination`, the mailbox a `message.move` moves into, resolved the way
/// every other mailbox parameter is.
///
/// Not `mailbox`: that key is already the *narrowing* of a selector address on
/// this method, and one key may not mean two things. Drafts is not a
/// destination, which [`resolve_mailbox`] already refuses for every caller.
fn destination_param(params: &Value, account: &AccountConfig) -> Result<String, RpcError> {
    let wanted = string_param(params, "destination")?;
    resolve_mailbox(account, &wanted)
}

/// A required boolean parameter, which is what the new state of a flag is.
fn bool_param(params: &Value, name: &str) -> Result<bool, RpcError> {
    match params.get(name) {
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(invalid_params(format!(
            "{name} is a required boolean: the state to set, not the state to toggle"
        ))),
    }
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

/// The Markdown rendition, in an array of its own (`RD-06`, #0126).
///
/// A fourth member of the family above in every way that matters - same result
/// shape, same handle directory, same ten-minute lifetime, same
/// `message.release_handle`, same `ANO-6` pin - and a separate array for the
/// reason [`MESSAGE_QUEUE_METHOD_SPECS`] is one: `tests/daemon_handles.rs`
/// pins [`MESSAGE_HANDLE_METHOD_SPECS`] at exactly the three names P3b-U12
/// shipped, and growing it would edit a pinned test to say something it was
/// not written to say.
///
/// `ClientIntegration` and `Durable`, as the two materialisers are: the daemon
/// prepares the file and only the client's own process can open it in
/// `$EDITOR`, and a viewer holding it open may not lose it because the socket
/// that asked for it went away.
pub const MESSAGE_MARKDOWN_METHOD_SPECS: [MethodSpec; 1] = [MethodSpec::new(
    "message.materialise_markdown",
    MethodKind::ClientIntegration,
    1,
)];

/// One of the four, selected by its own [`MethodSpec`].
///
/// One type for four methods because they share every dependency and differ
/// only in which of the bodies below they run; the dispatcher registers four
/// instances, so each still declares itself separately.
pub struct MessageHandleMethod {
    /// Which of [`MESSAGE_HANDLE_METHOD_SPECS`] or
    /// [`MESSAGE_MARKDOWN_METHOD_SPECS`] this instance serves.
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
                "message.materialise_markdown" => materialise(
                    &params,
                    &self.config.accounts(),
                    &self.handles,
                    HandleKind::Markdown,
                ),
                _ => release_handle(&params, &self.handles),
            };
            result.map(Outcome::query).map_err(DomainError::from)
        })
    }
}

/// Register the four handle methods on `dispatcher`.
pub fn register_handles(
    dispatcher: &mut Dispatcher,
    config: Arc<super::super::config::ConfigStore>,
    handles: Arc<HandleTable>,
) {
    for spec in MESSAGE_HANDLE_METHOD_SPECS
        .into_iter()
        .chain(MESSAGE_MARKDOWN_METHOD_SPECS)
    {
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
    let row = address(params, &store, &name)?;

    let (filename, bytes, pinned) = match kind {
        HandleKind::Attachment => attachment_file(params, &store, &blobs, row.id)?,
        HandleKind::Html => html_file(&store, &blobs, row.id)?,
        HandleKind::Markdown => markdown_file(&store, &blobs, &row),
    };
    let mode = match kind {
        // #0075's rule, moved to the daemon with the bytes: `$EDITOR` opens
        // the buffer read-only and says so, rather than letting someone
        // believe an edit reaches the message.
        HandleKind::Markdown => 0o444,
        _ => 0o644,
    };
    write_handle(handles, kind, &name, &filename, &bytes, &pinned, mode)
}

/// The store's own Markdown view of one message, and the blob it read.
///
/// [`read::render_markdown`] verbatim, which is the rendition `mp show` and
/// the pre-daemon `$EDITOR` view were both built from: YAML frontmatter folded
/// out of the `messages` row, then the stored plain text. It cannot fail - a
/// message with no readable body renders with an empty one, exactly as
/// `mp show` prints its "no stored body" sentence for the same row - so a
/// bodyless message is a rendition and not a refusal.
///
/// The pinned blob is the row's own `body_blob`, so a retention sweep running
/// beside an open editor cannot evict the bytes the file was built from
/// (`ANO-6`), exactly as the html rendition pins its markup.
///
/// The name is the subject slugified, or `message-<row_id>.md` for a message
/// with no subject, so a user reading three open buffers can tell them apart.
fn markdown_file(
    store: &Store,
    blobs: &BlobStore,
    row: &MessageRow,
) -> (String, Vec<u8>, Vec<String>) {
    let rendition = read::render_markdown(store, blobs, row);
    let pinned = row.body_blob.iter().cloned().collect();
    (
        markdown_name(row.subject.as_deref().unwrap_or_default(), row.id),
        rendition.into_bytes(),
        pinned,
    )
}

/// The file name of a Markdown rendition: the subject slugified, or
/// `message-<row_id>.md` when the slug is empty.
///
/// The row id is in the fallback rather than in every name because a subject
/// is what a user recognises; two messages sharing one subject collide on a
/// name and not on a path, since every handle owns its own directory.
fn markdown_name(subject: &str, row_id: i64) -> String {
    let slug: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug: String = crate::types::collapse_hyphens(&slug)
        .chars()
        .take(40)
        .collect();
    if slug.is_empty() {
        format!("message-{row_id}.md")
    } else {
        format!("{slug}.md")
    }
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

/// The browser rendition of one message as the file `message.materialise_html`
/// writes, and the blobs it read.
///
/// The bytes are [`html_rendition`]'s, unchanged: this adds the file name and
/// nothing else, so the file and `message.html`'s inline string cannot drift.
fn html_file(
    store: &Store,
    blobs: &BlobStore,
    row: i64,
) -> Result<(String, Vec<u8>, Vec<String>), RpcError> {
    let (rendition, pinned) = html_rendition(store, blobs, row)?;
    Ok(("message.html".to_string(), rendition.into_bytes(), pinned))
}

/// The browser rendition of one message, and the blobs it read.
///
/// The rendition, not the raw markup: the charset and the CSP tag the TUI's `b`
/// binding injects before it hands a `file://` URL to a browser (#0037), and the
/// `cid:` inlining that makes the referenced parts visible without a message to
/// resolve them against. Serving unhardened markup through a new door would undo
/// that fix at the moment the GUI starts using it, which is why both doors,
/// the file of `message.materialise_html` and the string of `message.html`, are
/// built here and nowhere else.
///
/// The pinned hashes matter only to a caller that holds the rendition past the
/// call, which is the file path's handle; the inline path drops them.
fn html_rendition(
    store: &Store,
    blobs: &BlobStore,
    row: i64,
) -> Result<(String, Vec<String>), RpcError> {
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
    Ok((rendition, pinned))
}

/// The `result` of `message.html`: [`html_rendition`]'s string inline, with
/// no file and no handle (`mp_protocol::rendition`).
///
/// The message is addressed as [`get`] addresses one. A message with no
/// markup is `-32602`, the refusal `message.materialise_html` answers for the
/// same row. The rendition's blobs are not pinned: nothing outlives the call,
/// so there is nothing for a retention sweep to pull out from under.
pub fn html(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    let name = string_param(params, "account")?;
    super::account::ready_account(accounts, &name)?;

    let store = Store::open(crate::config::store_path(&name))
        .map_err(|e| internal(format!("opening the store of {name}: {e:#}")))?;
    let blobs = BlobStore::new(crate::config::blobs_dir(&name));
    let row = address(params, &store, &name)?;
    let (rendition, _pinned) = html_rendition(&store, &blobs, row.id)?;
    inline_html(&name, row.id, rendition)
}

/// Wrap a rendition as `message.html`'s answer, or refuse one over
/// [`mp_protocol::rendition::MAX_INLINE_HTML_BYTES`].
///
/// The refusal is `frame_too_large` (`-32004`) rather than a code of its own:
/// the condition is the one that code names, an answer too large for the frame
/// it would travel in, measured against this method's own lower limit. Its
/// `data` is the `{limit, seen}` every `frame_too_large` carries plus
/// `fallback`, the method that serves the same bytes as a file, which is what
/// tells a client this refusal apart from one the transport made.
fn inline_html(account: &str, row_id: i64, rendition: String) -> Result<Value, RpcError> {
    use mp_protocol::rendition::{
        InlineHtmlRefusal, MessageHtml, HTML_FALLBACK_METHOD, MAX_INLINE_HTML_BYTES,
    };

    let bytes = rendition.len();
    if bytes > MAX_INLINE_HTML_BYTES {
        let data = InlineHtmlRefusal {
            limit: MAX_INLINE_HTML_BYTES as u64,
            seen: bytes as u64,
            fallback: HTML_FALLBACK_METHOD.to_string(),
        };
        return Err(RpcError {
            code: mp_protocol::ErrorCode::FrameTooLarge.code(),
            message: format!(
                "the HTML rendition is {bytes} bytes, over the {MAX_INLINE_HTML_BYTES}-byte \
                 inline limit; {HTML_FALLBACK_METHOD} serves it as a file"
            ),
            data: Some(
                serde_json::to_value(data)
                    .map_err(|e| internal(format!("serialising the inline-limit refusal: {e}")))?,
            ),
        });
    }
    let answer = MessageHtml {
        account: account.to_string(),
        row_id,
        bytes: bytes as u64,
        html: rendition,
    };
    serde_json::to_value(answer)
        .map_err(|e| internal(format!("serialising the rendition of {account}: {e}")))
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
    mode: u32,
) -> Result<Value, RpcError> {
    let id = handles.mint_id();
    let dir = handle_dir(&id);
    let path = dir.join(filename);
    let written = (|| -> anyhow::Result<u64> {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
        fs::write(&path, bytes)?;
        let len = fs::metadata(&path)?.len();
        // Last, because a 0444 file cannot be written to: the mode is the
        // rendition's property and the write is what produces it.
        fs::set_permissions(&path, Permissions::from_mode(mode))?;
        Ok(len)
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
pub(super) fn resolve_mailbox(account: &AccountConfig, wanted: &str) -> Result<String, RpcError> {
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

    /// A rendition at the inline limit is answered whole; one byte over it is
    /// `frame_too_large` naming the file path to fall back to.
    #[test]
    fn an_inline_rendition_over_the_limit_names_the_file_path() {
        use mp_protocol::rendition::{InlineHtmlRefusal, MessageHtml, MAX_INLINE_HTML_BYTES};

        let at_limit = "a".repeat(MAX_INLINE_HTML_BYTES);
        let answer = inline_html("alpha", 7, at_limit.clone()).expect("at the limit is inline");
        let answer: MessageHtml = serde_json::from_value(answer).expect("decodes");
        assert_eq!(answer.bytes, MAX_INLINE_HTML_BYTES as u64);
        assert_eq!(answer.row_id, 7);
        assert!(answer.html == at_limit);

        let over = "a".repeat(MAX_INLINE_HTML_BYTES + 1);
        let refused = inline_html("alpha", 7, over).expect_err("one byte over is refused");
        assert_eq!(refused.code, mp_protocol::ErrorCode::FrameTooLarge.code());
        let data: InlineHtmlRefusal =
            serde_json::from_value(refused.data.expect("carries data")).expect("decodes");
        assert_eq!(data.limit, MAX_INLINE_HTML_BYTES as u64);
        assert_eq!(data.seen, MAX_INLINE_HTML_BYTES as u64 + 1);
        assert_eq!(data.fallback, "message.materialise_html");
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

    /// The `json!` literal [`WireRow`] replaced, kept as the oracle it is
    /// checked against: the wire row of every listing before (perf) 2026-10-01.
    fn legacy_row(account: &str, row: &MessageRow) -> Value {
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
            "selector": Selector::for_message(account, row).to_string(),
        })
    }

    /// A listing built from [`WireRow`] and the `date_sort` column is the
    /// listing the `json!` literal and a fresh `resolve_date` parse built, to
    /// the byte, over the dates that could tell them apart: offsets that cross
    /// midnight in UTC, the epoch itself (which stamps as the column's "no
    /// date" `0`), a pre-epoch date, an unparsable and an empty header, and a
    /// far-future year; plus absent and present Cc/Reply-To/Bcc, every flag,
    /// an empty subject and a `Message-ID` the selector has to percent-encode.
    #[test]
    fn a_wire_row_is_the_json_literal_it_replaced() {
        use crate::ingest::{ingest_message, IngestInput};
        use crate::parse::FetchedEmail;
        use crate::types::MessageFlags;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.sqlite3");
        let store = Store::open(&path).expect("store");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let dates = [
            "Mon, 01 Jan 2024 09:00:00 +0000",
            "Tue, 02 Jan 2024 01:30:00 +0530",
            "Tue, 02 Jan 2024 22:15:07 -0800",
            "Thu, 01 Jan 1970 00:00:00 +0000",
            "Wed, 31 Dec 1969 23:00:00 +0000",
            "not a date at all",
            "",
            "Fri, 31 Dec 2100 23:59:59 +1400",
            "2 Jan 2024 08:00:00 GMT",
            "Sat, 31 Dec 2016 23:59:60 +0000",
            "Sat, 31 Dec 2016 23:59:60 +0100",
        ];
        for (index, date) in dates.iter().enumerate() {
            let odd = index % 2 == 1;
            let email = FetchedEmail {
                from: if index == 6 {
                    String::new()
                } else {
                    format!("Sender {index} <s{index}@example.com>")
                },
                to: "me@example.com".into(),
                cc: odd.then(|| "cc@example.com".to_string()),
                reply_to: (index % 3 == 0).then(|| "reply@example.com".to_string()),
                bcc: (index == 4).then(|| "hidden@example.com".to_string()),
                subject: if index == 5 {
                    String::new()
                } else {
                    format!("subject {index}")
                },
                date: (*date).into(),
                body_text: format!("body {index}"),
                html_body: None,
                has_attachments: index == 2,
                message_id: Some(format!("<odd {index}/%?#@example.com>")),
                attachments: Vec::new(),
                flags: MessageFlags {
                    seen: odd,
                    answered: index % 3 == 1,
                    forwarded: index % 4 == 2,
                    flagged: index == 7,
                },
                calendar_ics: None,
                event: None,
            };
            let input = IngestInput {
                account: "alpha",
                mailbox: "inbox",
                uid: index as i64 + 1,
                email: &email,
                raw: None,
            };
            ingest_message(&store, &blobs, &input).expect("ingests");
        }

        for limit in [None, Some(4), Some(0)] {
            let rows = read::list_mailbox(&store, "alpha", "inbox").expect("rows");
            let shown: Vec<Value> = rows
                .iter()
                .take(limit.unwrap_or(usize::MAX))
                .map(|row| legacy_row("alpha", row))
                .collect();
            let legacy = json!({
                "account": "alpha",
                "mailbox": "inbox",
                "total": rows.len(),
                "messages": shown,
            });
            let built = ListRead {
                name: "alpha".into(),
                mailbox: "inbox".into(),
                limit,
                path: path.clone(),
            }
            .run()
            .expect("lists");
            assert_eq!(
                serde_json::to_string(&built).unwrap(),
                serde_json::to_string(&legacy).unwrap(),
                "limit {limit:?}"
            );
            // `to_json`, which search answers through, is the same row too.
            for row in &rows {
                assert_eq!(
                    serde_json::to_string(&to_json("alpha", row)).unwrap(),
                    serde_json::to_string(&legacy_row("alpha", row)).unwrap()
                );
            }
        }
    }

    /// A stored row with every optional header set or not by `index`, and a
    /// subject that needs escaping, so the encoder meets the shapes a real
    /// listing carries.
    fn sample_row(index: i64) -> MessageRow {
        MessageRow {
            id: 1000 + index,
            mailbox: "inbox".to_string(),
            uid: index + 1,
            message_id: format!("<row {index}/%?#@example.com>"),
            from: (index % 5 != 0).then(|| format!("Sender {index} <s{index}@example.com>")),
            to: Some("me@example.com".to_string()),
            cc: (index % 2 == 1).then(|| "cc@example.com".to_string()),
            reply_to: (index % 3 == 0).then(|| "reply@example.com".to_string()),
            bcc: None,
            subject: Some(format!("Bericht \"{index}\" \u{fc}ber Antr\u{e4}ge\n")),
            date_display: Some("Thu, 2 Jul 2026 13:57:30 +0200".to_string()),
            flags: Some(if index % 2 == 0 { "\\Seen" } else { "" }.to_string()),
            has_attachments: index % 4 == 0,
            body_blob: None,
            thread_id: None,
            is_invite: index % 7 == 0,
        }
    }

    /// `frame::encode` of one `message.rows` notification built as a `Value`,
    /// with the keys inserted in sorted order so the comparison holds whether
    /// or not `serde_json` keeps insertion order.
    fn value_frame(offset: usize, operation_id: &str, rows: &[MessageRow]) -> Vec<u8> {
        let rows: Vec<Value> = rows
            .iter()
            .map(|row| {
                serde_json::to_value(WireRow::new("alpha", row, wire_date_sort(row, Some(0))))
                    .expect("a row serialises")
            })
            .collect();
        mp_protocol::frame::encode(&json!({
            "jsonrpc": "2.0",
            "method": "message.rows",
            "params": {"offset": offset, "operation_id": operation_id, "rows": rows},
        }))
        .expect("a notification encodes")
    }

    /// Every hand-built chunk frame is `frame::encode` of the same notification
    /// as a `Value`, byte for byte, and the chunks are contiguous: the first
    /// starts at 0, each next one where the last ended, and together they carry
    /// every row once, in order.
    #[test]
    fn a_chunk_frame_is_frame_encode_of_the_same_notification() {
        let rows: Vec<MessageRow> = (0..40).map(sample_row).collect();
        let id = "8f2c41d6b0e94a7fa3c5d81e6b0947fc";
        // About three rows a chunk, so the stream has many chunks and a short
        // last one.
        let mut chunks = ChunkEncoder::new(id, 1000, MAX_RESPONSE_BYTES);
        let mut frames = Vec::new();
        for row in &rows {
            chunks
                .push(&WireRow::new("alpha", row, wire_date_sort(row, Some(0))))
                .expect("a small row fits");
            while let Some(frame) = chunks.next_frame() {
                frames.push(frame);
            }
        }
        chunks.finish();
        while let Some(frame) = chunks.next_frame() {
            frames.push(frame);
        }
        assert!(
            frames.len() > 5,
            "the budget splits the rows: {}",
            frames.len()
        );

        let mut offset = 0;
        for frame in &frames {
            let decoded: Value = serde_json::from_slice(frame).expect("a frame is JSON");
            assert_eq!(decoded["params"]["offset"], json!(offset));
            let count = decoded["params"]["rows"].as_array().expect("rows").len();
            assert!(count > 0, "no chunk is empty");
            assert_eq!(
                frame,
                &value_frame(offset, id, &rows[offset..offset + count]),
                "the chunk at offset {offset} is frame::encode's bytes"
            );
            assert_eq!(frame.iter().filter(|b| **b == b'\n').count(), 1);
            offset += count;
        }
        assert_eq!(offset, rows.len(), "the chunks carry every row once");

        let mut empty = ChunkEncoder::new(id, 1000, MAX_RESPONSE_BYTES);
        empty.finish();
        assert_eq!(empty.next_frame(), None, "zero rows stream no chunk");
    }

    /// A chunk closes once its rows reach the budget, so no frame is larger
    /// than the budget plus one row plus the envelope; a row that would push a
    /// chunk over the cap opens the next chunk instead; and a row whose frame
    /// is over the cap even alone is `TooLarge` with the cap and the size.
    #[test]
    fn no_chunk_frame_passes_the_cap_and_an_oversized_row_is_refused() {
        let rows: Vec<MessageRow> = (0..20).map(sample_row).collect();
        let encoded: Vec<Vec<u8>> = rows
            .iter()
            .map(|row| {
                serde_json::to_vec(&WireRow::new("alpha", row, wire_date_sort(row, Some(0))))
                    .expect("encodes")
            })
            .collect();
        let largest = encoded.iter().map(Vec::len).max().expect("rows");
        let envelope = value_frame(0, "op", &[]).len();

        // A budget far above the cap: only the cap closes a chunk.
        let cap = envelope + 3 * largest;
        let mut chunks = ChunkEncoder::new("op", usize::MAX / 2, cap);
        let mut offset = 0;
        for row in &rows {
            chunks
                .push(&WireRow::new("alpha", row, wire_date_sort(row, Some(0))))
                .expect("every row fits alone");
        }
        chunks.finish();
        while let Some(frame) = chunks.next_frame() {
            assert!(frame.len() <= cap, "{} > {cap}", frame.len());
            let decoded: Value = serde_json::from_slice(&frame).expect("JSON");
            assert_eq!(decoded["params"]["offset"], json!(offset));
            offset += decoded["params"]["rows"].as_array().expect("rows").len();
        }
        assert_eq!(offset, rows.len());

        // A budget of one byte: every row is its own chunk.
        let mut chunks = ChunkEncoder::new("op", 1, MAX_RESPONSE_BYTES);
        for row in &rows {
            chunks
                .push(&WireRow::new("alpha", row, wire_date_sort(row, Some(0))))
                .expect("fits");
        }
        let mut frames = 0;
        while let Some(frame) = chunks.next_frame() {
            assert!(frame.len() <= envelope + largest + 2);
            frames += 1;
        }
        assert_eq!(frames, rows.len());

        // A cap below one row's frame.
        let mut chunks = ChunkEncoder::new("op", ROWS_CHUNK_BYTES, envelope + 10);
        let refused = chunks
            .push(&WireRow::new(
                "alpha",
                &rows[0],
                wire_date_sort(&rows[0], Some(0)),
            ))
            .expect_err("the row does not fit in any frame");
        let ChunkError::TooLarge { limit, seen } = refused else {
            panic!("not a TooLarge: {refused:?}");
        };
        assert_eq!(limit, envelope + 10);
        assert_eq!(seen, value_frame(0, "op", &rows[..1]).len());
        assert_eq!(
            chunks.next_frame(),
            None,
            "nothing of the failed row goes out"
        );

        let error = ChunkError::TooLarge { limit, seen }.into_domain("alpha", "inbox", 0);
        assert_eq!(
            error.code(),
            i64::from(mp_protocol::ErrorCode::FrameTooLarge.code())
        );
        assert_eq!(error.data(), Some(json!({"limit": limit, "seen": seen})));
    }

    /// The method is a client-scoped operation, and a chunk's budget is 1 MiB.
    #[test]
    fn the_stream_is_a_client_scoped_operation_with_a_one_mib_budget() {
        assert_eq!(ROWS_CHUNK_BYTES, 1 << 20);
        assert_eq!(
            MESSAGE_STREAM_METHOD_SPECS[0].cancel_scope,
            super::super::super::dispatch::CancelScope::ClientScoped
        );
        assert_eq!(MESSAGE_STREAM_METHOD_SPECS[0].kind, MethodKind::Operation);
    }

    /// `message.list` with `limit: null` over a large mailbox, timed in process:
    /// the store read alone, the whole method, and the frame the server would
    /// encode from its answer (`docs/baselines/message-list-unbounded.md`).
    ///
    /// Points at a fixture `examples/mkfixture.rs` built, named by
    /// `MP_BENCH_FIXTURE`, and prints nothing but a skip line without one:
    ///
    /// ```sh
    /// target/release/examples/mkfixture --out /var/tmp/mp-bench-50k --rows 50000
    /// MP_BENCH_FIXTURE=/var/tmp/mp-bench-50k cargo test --release --lib \
    ///   message_list_unbounded_bench -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a benchmark over a generated fixture; see the doc comment"]
    fn message_list_unbounded_bench() {
        use std::time::Instant;

        let Some(root) = std::env::var_os("MP_BENCH_FIXTURE").map(std::path::PathBuf::from) else {
            eprintln!("MP_BENCH_FIXTURE is unset; nothing to measure");
            return;
        };
        let mailbox = std::env::var("MP_BENCH_MAILBOX").unwrap_or_else(|_| "Bulk".to_string());
        let _data = crate::config::test_env::DataDirOverride::set(root.join("data"));
        let toml = fs::read_to_string(root.join("config/config.toml")).expect("fixture config");
        let config: crate::config::GlobalConfig = toml::from_str(&toml).expect("parses");
        let accounts = config.accounts;
        let params = json!({"account": "alpha", "mailbox": mailbox, "limit": null});

        // Median of eleven after one discarded warm-up, min and max beside it:
        // the protocol of `docs/baselines/pre-daemon/workloads.md`.
        fn sample(mut run: impl FnMut()) -> (f64, f64, f64) {
            run();
            let mut ms: Vec<f64> = (0..11)
                .map(|_| {
                    let started = Instant::now();
                    run();
                    started.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            ms.sort_by(f64::total_cmp);
            (ms[5], ms[0], ms[10])
        }

        let path = crate::config::store_path("alpha");
        let resolved = resolve_mailbox(
            accounts.iter().find(|a| a.name == "alpha").expect("alpha"),
            &mailbox,
        )
        .expect("mailbox");
        let read_only = sample(|| {
            let store = Store::open(&path).expect("store");
            let rows = read::list_mailbox(&store, "alpha", &resolved).expect("rows");
            std::hint::black_box(rows);
        });
        // The pre-(perf) answer, rebuilt from the oracle in the same run, so a
        // before/after pair is taken under the same host load.
        let legacy = sample(|| {
            let store = Store::open(&path).expect("store");
            let rows = read::list_mailbox(&store, "alpha", &resolved).expect("rows");
            let messages: Vec<Value> = rows.iter().map(|row| legacy_row("alpha", row)).collect();
            std::hint::black_box(json!({
                "account": "alpha",
                "mailbox": resolved,
                "total": rows.len(),
                "messages": messages,
            }));
        });
        let method = sample(|| {
            std::hint::black_box(list(&params, &accounts).expect("lists"));
        });
        // Two references for what the remaining time is: the rows alone with
        // no JSON at all, and the rows serialised straight to bytes, which is
        // the floor an answer that skipped the `Value` tree would reach.
        let store = Store::open(&path).expect("store");
        let (dated, _) = read::list_mailbox_dated(&store, "alpha", &resolved, None).expect("rows");
        let wire_only = sample(|| {
            for (row, stamped) in &dated {
                std::hint::black_box(WireRow::new("alpha", row, wire_date_sort(row, *stamped)));
            }
        });
        let to_bytes = sample(|| {
            let rows: Vec<WireRow<'_>> = dated
                .iter()
                .map(|(row, stamped)| WireRow::new("alpha", row, wire_date_sort(row, *stamped)))
                .collect();
            std::hint::black_box(serde_json::to_vec(&rows).expect("encodes"));
        });
        let answer = list(&params, &accounts).expect("lists");
        let rows = answer["messages"].as_array().map_or(0, Vec::len);
        let reply = json!({"jsonrpc": "2.0", "id": 1, "result": answer});
        let encode = sample(|| {
            std::hint::black_box(mp_protocol::frame::encode(&reply).expect("encodes"));
        });
        let bytes = mp_protocol::frame::encode(&reply).expect("encodes").len();
        let decode = sample(|| {
            let bytes = mp_protocol::frame::encode(&reply).expect("encodes");
            let value: Value = serde_json::from_slice(&bytes).expect("decodes");
            std::hint::black_box(value);
        });

        let show = |(median, min, max): (f64, f64, f64)| format!("{median:.1} {min:.1} {max:.1}");
        eprintln!(
            "rows {rows}, frame {bytes} bytes, cap {}",
            mp_protocol::MAX_RESPONSE_BYTES
        );
        eprintln!(
            "store read (list_mailbox)   ms median min max: {}",
            show(read_only)
        );
        eprintln!(
            "legacy json! rows (before)  ms median min max: {}",
            show(legacy)
        );
        eprintln!(
            "message.list (read + rows)  ms median min max: {}",
            show(method)
        );
        eprintln!(
            "  WireRow::new only (ref)    ms median min max: {}",
            show(wire_only)
        );
        eprintln!(
            "  rows straight to bytes(ref) ms median min max: {}",
            show(to_bytes)
        );
        eprintln!(
            "frame::encode of the reply  ms median min max: {}",
            show(encode)
        );
        eprintln!(
            "encode + client-side parse  ms median min max: {}",
            show(decode)
        );
    }
}
