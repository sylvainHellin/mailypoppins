//! `message.list_stream` over a real socket (#0138, rollout step 2).
//!
//! The method answers at once with the listing's head and then streams the rows
//! to the calling connection as `message.rows` notifications, so a mailbox past
//! the 16 MiB response cap still lists. These rows pin what the ticket's
//! "Ordering" and "Cancel and failure" sections promise, against a daemon
//! process started the way every slice starts one:
//!
//! - a mailbox of a few thousand rows, with the chunk budget lowered through
//!   `ROWS_CHUNK_BYTES_ENV`, streams several chunks with contiguous offsets,
//!   every row equal to the `message.list` row for the same message, and the
//!   finish after the last chunk;
//! - a cancel mid-stream, with chunks still in the connection's channel,
//!   settles `cancelled` with `-32008`, and no chunk follows the finish;
//! - a disconnect mid-stream cancels the stream, because it is
//!   `client_scoped`;
//! - an unknown account or mailbox is refused at the call, and no operation is
//!   issued for it;
//! - a row too large for any frame fails the operation with `-32004` and the
//!   connection stays open.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::listing::{
    MessageListRow, MessageListStreamStarted, MessageListing, MessageRowsChunk,
    METHOD_MESSAGE_LIST_STREAM,
};
use mp_protocol::{frame, Notification, METHOD_MESSAGE_ROWS, METHOD_STATE_EVENT};

use mailypoppins::daemon::methods::message::ROWS_CHUNK_BYTES_ENV;
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{BlobStore, Store};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(30);

/// The rows the bulk fixture adds to `alpha`'s inbox, on top of the read
/// fixture's four.
const BULK_ROWS: usize = 2000;

/// The chunk budget of the multi-chunk row: about thirty rows a chunk.
const SMALL_CHUNK: &str = "16384";

/// The chunk budget of the cancel and disconnect rows: about two rows a chunk,
/// so a client that does not read leaves the producer parked with chunks still
/// in the connection's channel long before the stream ends.
const TINY_CHUNK: &str = "1024";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory goes.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    /// The read fixture plus [`BULK_ROWS`] rows in `alpha`'s inbox, served
    /// with the chunk budget `chunk`.
    fn bulk(chunk: &str) -> Slice {
        let tmp = TempDir::new().expect("a temporary stream root");
        fixture::seed(tmp.path());
        seed_bulk(tmp.path(), BULK_ROWS);
        let daemon = DaemonFixture::start_with(tmp.path(), None, &[(ROWS_CHUNK_BYTES_ENV, chunk)]);
        Slice { daemon, tmp }
    }

    /// The read fixture plus one row in `alpha`'s Sent whose subject alone is
    /// larger than any frame may be, at the default chunk budget.
    fn oversized() -> Slice {
        let tmp = TempDir::new().expect("a temporary stream root");
        fixture::seed(tmp.path());
        let mut huge = fixture::email(
            "huge@example.com",
            "sylvain@example.com",
            &"x".repeat(mp_protocol::MAX_RESPONSE_BYTES),
            "Mon, 1 Jun 2026 08:00:00 +0000",
            "",
        );
        huge.message_id = Some("<huge@example.com>".to_string());
        fixture::ingest(tmp.path(), fixture::ACCOUNT, "sent", 2, &huge);
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// An initialized connection, subscribed through `state.bootstrap` so it
    /// sees the `operation.finished` of the streams it starts.
    async fn connect(&self) -> Connection {
        let mut conn = within(
            "Connection::connect",
            Connection::connect(&socket_path(self.root())),
        )
        .await
        .expect("connecting to a live daemon socket succeeds");
        within(
            "Connection::initialize",
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Tui,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.root().to_path_buf(),
                    config_dir: self.root().to_path_buf(),
                },
                &[METHOD_MESSAGE_LIST_STREAM],
                &[],
            ),
        )
        .await
        .expect("a handshake requiring message.list_stream succeeds");
        call(&mut conn, "state.bootstrap", json!({})).await;
        conn
    }
}

/// `count` rows in `alpha`'s inbox, through one store handle, with uids above
/// the read fixture's and one date per row so the order is fixed.
fn seed_bulk(root: &Path, count: usize) {
    let dir = fixture::account_dir(root, fixture::ACCOUNT);
    let store = Store::open(dir.join("store.sqlite3")).expect("open the fixture store");
    let blobs = BlobStore::new(dir.join("blobs"));
    for n in 0..count {
        let mut email = fixture::email(
            &format!("Sender {n} <sender{n}@example.com>"),
            "Sylvain Hellin <sylvain@example.com>",
            &format!("Wochenbericht Nummer {n} über die Anträge"),
            &format!(
                "Mon, {} Jan 2024 {:02}:{:02}:00 +0000",
                1 + n / 1440 % 28,
                n / 60 % 24,
                n % 60
            ),
            "",
        );
        email.message_id = Some(format!("<bulk-{n}@example.com>"));
        email.cc = (n % 3 == 0).then(|| "team@example.com".to_string());
        ingest_message(
            &store,
            &blobs,
            &IngestInput {
                account: fixture::ACCOUNT,
                mailbox: "inbox",
                uid: 100 + n as i64,
                email: &email,
                raw: None,
            },
        )
        .expect("ingest a bulk row");
    }
}

async fn within<F, T>(what: &str, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not answer within {DEADLINE:?}"),
    }
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

/// The refusal a call answered, or a panic naming what came instead.
async fn refused(conn: &mut Connection, method: &str, params: Value) -> mp_protocol::RpcError {
    match within(method, conn.call(method, params.clone())).await {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("{method} {params} was not refused: {other:?}"),
    }
}

async fn start_stream(conn: &mut Connection, mailbox: &str) -> MessageListStreamStarted {
    let answer = call(
        conn,
        METHOD_MESSAGE_LIST_STREAM,
        json!({"account": fixture::ACCOUNT, "mailbox": mailbox}),
    )
    .await;
    let mut keys: Vec<&str> = answer
        .as_object()
        .expect("the answer is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["account", "mailbox", "operation_id", "total"]);
    serde_json::from_value(answer).expect("the answer decodes")
}

async fn next(conn: &mut Connection) -> Notification {
    within("the next notification", conn.next_notification())
        .await
        .expect("the daemon keeps the connection open")
}

/// The finish payload when `notification` is `operation.finished` of `id`.
fn finish_of(notification: &Notification, id: &str) -> Option<Value> {
    (notification.method == METHOD_STATE_EVENT
        && notification.params["kind"] == json!("operation.finished")
        && notification.params["payload"]["operation_id"] == json!(id))
    .then(|| notification.params["payload"].clone())
}

/// The chunk when `notification` is a `message.rows` of `id`.
fn chunk_of(notification: &Notification, id: &str) -> Option<MessageRowsChunk> {
    if notification.method != METHOD_MESSAGE_ROWS {
        return None;
    }
    let chunk: MessageRowsChunk =
        serde_json::from_value(notification.params.clone()).expect("a chunk decodes");
    (chunk.operation_id == id).then_some(chunk)
}

/// Read until the finish of `id`, collecting its chunks, and hand both back.
async fn collect(conn: &mut Connection, id: &str) -> (Vec<(MessageRowsChunk, usize)>, Value) {
    let mut chunks = Vec::new();
    loop {
        let notification = next(conn).await;
        if let Some(finish) = finish_of(&notification, id) {
            return (chunks, finish);
        }
        if let Some(chunk) = chunk_of(&notification, id) {
            let bytes = frame::encode(&notification).expect("encodes").len();
            chunks.push((chunk, bytes));
        }
    }
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// A few thousand rows at a 16 KiB budget stream as many chunks, each starting
/// where the last ended, together equal to the `message.list` rows for the same
/// mailbox, and the finish arrives only after the last of them.
#[tokio::test]
async fn a_large_mailbox_streams_contiguous_chunks_equal_to_the_listing() {
    let slice = Slice::bulk(SMALL_CHUNK);
    let mut conn = slice.connect().await;

    let started = start_stream(&mut conn, "inbox").await;
    assert_eq!(started.account, fixture::ACCOUNT);
    assert_eq!(started.mailbox, "inbox");
    assert_eq!(started.total as usize, BULK_ROWS + 4);

    let (chunks, finish) = collect(&mut conn, &started.operation_id).await;
    assert_eq!(finish["state"], json!("succeeded"), "{finish}");
    assert_eq!(
        finish["result"],
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "total": started.total}),
    );
    assert!(
        chunks.len() > 10,
        "a 16 KiB budget splits {} rows into many chunks, got {}",
        started.total,
        chunks.len()
    );

    let mut rows: Vec<MessageListRow> = Vec::new();
    let mut largest_row = 0;
    for (chunk, bytes) in &chunks {
        assert_eq!(
            chunk.offset as usize,
            rows.len(),
            "each chunk starts where the last one ended"
        );
        assert!(!chunk.rows.is_empty(), "no chunk is empty");
        for row in &chunk.rows {
            largest_row = largest_row.max(serde_json::to_vec(row).expect("encodes").len());
        }
        rows.extend(chunk.rows.iter().cloned());
        let _ = bytes;
    }
    let budget: usize = SMALL_CHUNK.parse().expect("a number");
    for (chunk, bytes) in &chunks {
        assert!(
            *bytes <= budget + largest_row + 256,
            "the chunk at {} is {bytes} bytes, over the budget plus one row plus the envelope",
            chunk.offset
        );
    }
    assert_eq!(
        rows.len() as u64,
        started.total,
        "every row arrived before the finish"
    );

    let listed = call(
        &mut conn,
        "message.list",
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "limit": null}),
    )
    .await;
    let listing: MessageListing = serde_json::from_value(listed).expect("the listing decodes");
    assert_eq!(listing.total, started.total);
    assert_eq!(
        rows, listing.messages,
        "the streamed rows are the message.list rows, field for field and in order"
    );
}

/// `operation.cancel` while the producer is parked on a full channel settles
/// the stream `cancelled` with `-32008`, and nothing of it follows the finish:
/// the chunks still in the channel carry the shut token and are dropped.
#[tokio::test]
async fn a_cancel_mid_stream_settles_cancelled_and_no_chunk_follows_the_finish() {
    let slice = Slice::bulk(TINY_CHUNK);
    let mut conn = slice.connect().await;

    let started = start_stream(&mut conn, "inbox").await;
    let id = started.operation_id.clone();
    // Read nothing for a moment: the socket buffer fills, then the channel,
    // then the producer parks with chunks still queued behind it.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let cancelled = call(&mut conn, "operation.cancel", json!({"operation_id": id})).await;
    assert_eq!(cancelled["state"], json!("cancelled"));

    let (chunks, finish) = collect(&mut conn, &id).await;
    assert_eq!(finish["state"], json!("cancelled"), "{finish}");
    assert_eq!(finish["error"]["code"], json!(-32008));
    assert_eq!(finish["error"]["data"], json!({"operation_id": id}));
    let delivered: usize = chunks.iter().map(|(chunk, _)| chunk.rows.len()).sum();
    assert!(
        (delivered as u64) < started.total,
        "the cancel cut the stream short: {delivered} of {} rows",
        started.total
    );
    let mut offset = 0;
    for (chunk, _) in &chunks {
        assert_eq!(
            chunk.offset as usize, offset,
            "a cancelled stream is a prefix"
        );
        offset += chunk.rows.len();
    }

    // Nothing of the stream after its finish, however long one waits.
    let mut after = Vec::new();
    while let Ok(Some(notification)) =
        tokio::time::timeout(Duration::from_millis(500), conn.next_notification()).await
    {
        after.push(notification);
    }
    assert!(
        after.iter().all(|n| chunk_of(n, &id).is_none()),
        "a chunk followed the cancelled finish: {:?}",
        after.iter().map(|n| &n.method).collect::<Vec<_>>()
    );
    // And the connection still answers.
    call(&mut conn, "account.list", json!({})).await;
}

/// A disconnect of the calling connection cancels its stream, which another
/// subscribed connection sees as the same `cancelled` finish an explicit
/// cancel publishes.
#[tokio::test]
async fn a_disconnect_mid_stream_cancels_the_stream() {
    let slice = Slice::bulk(TINY_CHUNK);
    let mut watcher = slice.connect().await;
    let mut caller = slice.connect().await;

    let started = start_stream(&mut caller, "inbox").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(caller);

    let finish = loop {
        let notification = next(&mut watcher).await;
        assert!(
            notification.method != METHOD_MESSAGE_ROWS,
            "a chunk reached a connection that did not ask for it"
        );
        if let Some(finish) = finish_of(&notification, &started.operation_id) {
            break finish;
        }
    };
    assert_eq!(finish["state"], json!("cancelled"), "{finish}");
    assert_eq!(finish["error"]["code"], json!(-32008));
}

/// Every refusal is the call's own error, issued before any operation: the
/// first `operation.finished` the connection sees afterwards is the one of the
/// stream that was accepted.
#[tokio::test]
async fn a_refused_stream_is_the_calls_error_and_issues_no_operation() {
    let slice = Slice::bulk(SMALL_CHUNK);
    let mut conn = slice.connect().await;

    let unknown = refused(
        &mut conn,
        METHOD_MESSAGE_LIST_STREAM,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "mailbox": "inbox"}),
    )
    .await;
    assert_eq!(unknown.code, -32005, "{unknown:?}");
    assert_eq!(
        unknown.data,
        Some(json!({"account": fixture::UNKNOWN_ACCOUNT}))
    );

    let no_mailbox = refused(
        &mut conn,
        METHOD_MESSAGE_LIST_STREAM,
        json!({"account": fixture::ACCOUNT, "mailbox": "nowhere"}),
    )
    .await;
    assert_eq!(no_mailbox.code, -32602, "{no_mailbox:?}");
    assert!(
        no_mailbox.message.contains("inbox"),
        "names the mailboxes the account has: {}",
        no_mailbox.message
    );

    let drafts = refused(
        &mut conn,
        METHOD_MESSAGE_LIST_STREAM,
        json!({"account": fixture::ACCOUNT, "mailbox": "drafts"}),
    )
    .await;
    assert_eq!(drafts.code, -32602, "Drafts is draft.list's: {drafts:?}");

    let bounded = refused(
        &mut conn,
        METHOD_MESSAGE_LIST_STREAM,
        json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "limit": 10}),
    )
    .await;
    assert_eq!(bounded.code, -32602, "a bounded listing is message.list's");

    for error in [&unknown, &no_mailbox, &drafts, &bounded] {
        assert!(
            error
                .data
                .as_ref()
                .is_none_or(|data| data.get("operation_id").is_none()),
            "a refusal names no operation: {error:?}"
        );
    }

    let started = start_stream(&mut conn, "sent").await;
    let finish = loop {
        let notification = next(&mut conn).await;
        if notification.method == METHOD_STATE_EVENT
            && notification.params["kind"] == json!("operation.finished")
        {
            break notification.params["payload"].clone();
        }
    };
    assert_eq!(
        finish["operation_id"],
        json!(started.operation_id),
        "the first finish is the accepted stream's: no refused call started one"
    );
    assert_eq!(finish["state"], json!("succeeded"));
}

/// A row whose own frame would be over the response cap fails the stream with
/// `frame_too_large` and `{limit, seen}` in the finish, rather than a frame the
/// client's decoder would cut the connection on; the connection stays open.
#[tokio::test]
async fn an_oversized_row_fails_the_stream_and_the_connection_stays_open() {
    let slice = Slice::oversized();
    let mut conn = slice.connect().await;

    let started = start_stream(&mut conn, "sent").await;
    assert_eq!(
        started.total, 2,
        "the read fixture's reply and the huge row"
    );

    let (_chunks, finish) = collect(&mut conn, &started.operation_id).await;
    assert_eq!(finish["state"], json!("failed"), "{finish}");
    assert_eq!(finish["error"]["code"], json!(-32004));
    let limit = finish["error"]["data"]["limit"].as_u64().expect("a limit");
    let seen = finish["error"]["data"]["seen"].as_u64().expect("a seen");
    assert_eq!(limit, mp_protocol::MAX_RESPONSE_BYTES as u64);
    assert!(seen > limit, "seen {seen} breaches the limit {limit}");

    let accounts = call(&mut conn, "account.list", json!({})).await;
    assert!(
        accounts["accounts"].is_array(),
        "the connection still answers"
    );
}
