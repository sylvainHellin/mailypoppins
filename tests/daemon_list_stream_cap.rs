//! The `frame_too_large` regression of #0138 (rollout step 6): a mailbox past
//! the 16 MiB response cap lists through `message.list_stream`.
//!
//! Before the stream, a mailbox past about 34 000 rows of about 500 bytes
//! could not be opened: `message.list` with `limit: null` answered
//! `frame_too_large` (`-32004`) and the client listed nothing. These rows hold,
//! against a daemon process over a real socket:
//!
//! - `message.list` with `limit: null` still answers `-32004`, since it keeps
//!   the cap for a client that still sends it;
//! - `message.list_stream` collected by `mp-client`'s session thread yields
//!   every row, in the order a bounded `message.list` answers them;
//! - no frame of the stream is over `ROWS_CHUNK_BYTES` plus one row plus the
//!   envelope.
//!
//! The store is seeded with 50 000 rows of about 500 bytes, the ticket's
//! figure: one through ingest and the rest copied from it in one transaction
//! (see [`seed_bulk`]), which keeps the run to seconds where ingesting every
//! row would take a quarter of an hour.
//!
//! One [`Session`] per test binary: a [`Connector`] is two plain function
//! pointers, so the socket it opens can only be reached through a static, the
//! pattern `crates/mp-client/src/session.rs`'s own tests use, and this binary
//! therefore holds one test.

mod support;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::session::{Connector, ReopenSession, Session};
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::listing::{
    MessageListRow, MessageListStreamStarted, MessageListing, MessageRowsChunk,
    METHOD_MESSAGE_LIST_STREAM,
};
use mp_protocol::{frame, Notification, MAX_RESPONSE_BYTES, METHOD_MESSAGE_ROWS};

use mailypoppins::daemon::methods::message::ROWS_CHUNK_BYTES;
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{BlobStore, Store};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

/// The rows the store is seeded with: past the cap at about 500 bytes a row.
const ROWS: usize = 50_000;

/// The mailbox the rows are listed from, an `extra` one as in the bench
/// fixture.
const MAILBOX: &str = "Bulk";

const ACCOUNT: &str = "alpha";

/// How long one call, one stream or one collected listing may take: a debug
/// build listing 50 000 rows takes a few seconds, so this only bounds a hang.
const DEADLINE: Duration = Duration::from_secs(120);

/// The configuration of the default run: one account with the `Bulk`
/// mailbox.
const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts.mailboxes.extra]]
server = "Bulk"
"#;

/// The socket the session's connector opens, set once per test binary.
static SOCKET: OnceLock<PathBuf> = OnceLock::new();

fn open() -> Pin<Box<dyn Future<Output = Connection> + Send>> {
    Box::pin(async {
        let socket = SOCKET.get().expect("the socket is set");
        let root = socket
            .parent()
            .and_then(Path::parent)
            .expect("<root>/runtime/daemon.sock");
        let (connection, _hello) = Connection::open_requiring(
            socket,
            client_info(),
            identity(root),
            &[METHOD_MESSAGE_LIST_STREAM],
            Duration::from_secs(10),
        )
        .await
        .expect("the daemon accepts a session requiring the stream");
        connection
    })
}

/// No second connection: the test ends with the first.
const REOPEN: ReopenSession = || Box::pin(async { None });

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Tui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn identity(root: &Path) -> Identity {
    Identity {
        data_dir: root.to_path_buf(),
        config_dir: root.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// A mailbox of [`ROWS`] rows, past the cap, lists through the stream.
#[test]
fn a_mailbox_past_the_response_cap_lists_through_the_stream() {
    let tmp = TempDir::new().expect("a temporary root");
    std::fs::write(tmp.path().join("config.toml"), CONFIG).expect("write config.toml");
    let seeding = Instant::now();
    seed_bulk(tmp.path(), ROWS);
    eprintln!(
        "seeded {ROWS} rows in {:.1}s",
        seeding.elapsed().as_secs_f64()
    );
    regression(tmp.path(), ROWS);
}

/// The three promises, over a daemon serving `root`, whose `alpha/Bulk` holds
/// exactly `rows` rows.
fn regression(root: &Path, rows: usize) {
    let daemon = DaemonFixture::start(root);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");

    // `message.list` with `limit: null` keeps the cap.
    let (refused, bounded, chunks) = runtime.block_on(async {
        let mut conn = connect(root).await;
        let refused = refused(
            &mut conn,
            "message.list",
            json!({"account": ACCOUNT, "mailbox": MAILBOX, "limit": null}),
        )
        .await;
        // The order a bounded `message.list` answers, under the cap.
        let bounded: MessageListing = serde_json::from_value(
            call(
                &mut conn,
                "message.list",
                json!({"account": ACCOUNT, "mailbox": MAILBOX, "limit": 10_000}),
            )
            .await,
        )
        .expect("a bounded listing decodes");
        let chunks = raw_stream(&mut conn).await;
        (refused, bounded, chunks)
    });
    assert_eq!(
        refused.code,
        mp_protocol::ErrorCode::FrameTooLarge.code(),
        "the whole mailbox in one answer is over the cap: {refused:?}"
    );
    let data = refused.data.expect("frame_too_large carries data");
    assert_eq!(data["limit"], json!(MAX_RESPONSE_BYTES));
    let seen = data["seen"].as_u64().expect("seen") as usize;
    assert!(seen > MAX_RESPONSE_BYTES, "{seen}");
    eprintln!(
        "message.list limit: null: {seen} bytes, {:.0} a row",
        seen as f64 / rows as f64
    );
    assert_eq!(bounded.total, rows as u64);

    // No frame of the stream is over the chunk budget plus one row plus the
    // envelope, and every frame is far under the cap.
    let (started, frames) = chunks;
    assert_eq!(started.total, rows as u64);
    let mut offset = 0u64;
    for (chunk, bytes, largest) in &frames {
        assert_eq!(chunk.offset, offset, "the chunks are contiguous");
        offset += chunk.rows.len() as u64;
        let envelope = envelope_bytes(chunk.offset, &chunk.operation_id);
        assert!(
            *bytes <= ROWS_CHUNK_BYTES + largest + envelope + 1,
            "a {bytes}-byte frame is over {ROWS_CHUNK_BYTES} + {largest} + {envelope}"
        );
        assert!(*bytes < MAX_RESPONSE_BYTES / 8, "{bytes}");
    }
    assert_eq!(offset, rows as u64, "the chunks carry every row");
    let biggest = frames.iter().map(|(_, bytes, _)| *bytes).max().unwrap_or(0);
    eprintln!("{} chunks, the largest {biggest} bytes", frames.len());

    // Through `mp-client`'s collector, as both clients list a mailbox.
    SOCKET
        .set(socket_path(root))
        .expect("set once per test binary");
    let session = Session::connect(Connector {
        open,
        reopen: REOPEN,
    })
    .expect("a session");
    session
        .call_within("state.bootstrap", json!({}), DEADLINE)
        .expect("the session subscribes, so it sees the stream's finish");
    let collecting = Instant::now();
    let listing = session
        .list_stream_within(ACCOUNT, MAILBOX, DEADLINE)
        .expect("the stream lists the whole mailbox");
    eprintln!(
        "collected {} rows in {:.0} ms",
        listing.messages.len(),
        collecting.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(listing.account, ACCOUNT);
    assert_eq!(listing.mailbox, MAILBOX);
    assert_eq!(listing.total, rows as u64);
    assert_eq!(listing.messages.len(), rows);

    // The streamed rows equal the listed ones, field for field and in order.
    let streamed: Vec<&MessageListRow> = frames
        .iter()
        .flat_map(|(chunk, _, _)| chunk.rows.iter())
        .collect();
    assert!(
        listing.messages.iter().eq(streamed.iter().copied()),
        "the collected rows are the rows on the wire"
    );
    assert_eq!(
        &listing.messages[..bounded.messages.len()],
        &bounded.messages[..],
        "the stream's rows are message.list's, in message.list's order"
    );
    let mut session = session;
    session.close();
    daemon.stop();
}

// ---------------------------------------------------------------------------
// The raw stream
// ---------------------------------------------------------------------------

/// The stream on a plain connection: the answer, then each chunk with the
/// bytes of its frame and of its largest row.
async fn raw_stream(
    conn: &mut Connection,
) -> (
    MessageListStreamStarted,
    Vec<(MessageRowsChunk, usize, usize)>,
) {
    let started: MessageListStreamStarted = serde_json::from_value(
        call(
            conn,
            METHOD_MESSAGE_LIST_STREAM,
            json!({"account": ACCOUNT, "mailbox": MAILBOX}),
        )
        .await,
    )
    .expect("the answer decodes");
    let id = started.operation_id.clone();
    let mut frames = Vec::new();
    loop {
        let notification = within("the next notification", conn.next_notification())
            .await
            .expect("the daemon keeps the connection open");
        if is_finish_of(&notification, &id) {
            assert_eq!(
                notification.params["payload"]["state"],
                json!("succeeded"),
                "{notification:?}"
            );
            return (started, frames);
        }
        if notification.method != METHOD_MESSAGE_ROWS
            || notification.params["operation_id"] != json!(id)
        {
            continue;
        }
        // The frame's bytes: the daemon writes its keys sorted, as
        // `frame::encode` does, so re-encoding the decoded frame is the frame.
        let bytes = frame::encode(&notification).expect("encodes").len();
        let largest = notification.params["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| serde_json::to_vec(row).expect("encodes").len())
            .max()
            .unwrap_or(0);
        let chunk: MessageRowsChunk =
            serde_json::from_value(notification.params).expect("a chunk decodes");
        frames.push((chunk, bytes, largest));
    }
}

/// Whether `notification` is the `operation.finished` of `id`.
fn is_finish_of(notification: &Notification, id: &str) -> bool {
    notification.method == mp_protocol::METHOD_STATE_EVENT
        && notification.params["kind"] == json!("operation.finished")
        && notification.params["payload"]["operation_id"] == json!(id)
}

/// The bytes of a `message.rows` frame with no rows, at `offset`.
fn envelope_bytes(offset: u64, id: &str) -> usize {
    let empty = Notification {
        jsonrpc: "2.0".to_string(),
        method: METHOD_MESSAGE_ROWS.to_string(),
        params: json!({"offset": offset, "operation_id": id, "rows": []}),
    };
    frame::encode(&empty).expect("encodes").len()
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// An initialized connection, subscribed so it sees the stream's finish.
async fn connect(root: &Path) -> Connection {
    let (mut conn, _hello) = within(
        "Connection::open_requiring",
        Connection::open_requiring(
            &socket_path(root),
            client_info(),
            identity(root),
            &[METHOD_MESSAGE_LIST_STREAM],
            Duration::from_secs(10),
        ),
    )
    .await
    .expect("a handshake requiring message.list_stream succeeds");
    call(&mut conn, "state.bootstrap", json!({})).await;
    conn
}

async fn within<F, T>(what: &str, future: F) -> T
where
    F: Future<Output = T>,
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

async fn refused(conn: &mut Connection, method: &str, params: Value) -> mp_protocol::RpcError {
    match within(method, conn.call(method, params.clone())).await {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("{method} {params} was not refused: {other:?}"),
    }
}

/// `count` rows in `alpha/Bulk`, each about 500 bytes on the wire like a row
/// of the bench fixture, one minute apart so the order is fixed.
///
/// The first row goes through [`ingest_message`], so the store, its schema and
/// that row are what a sync writes. The rest are copies of it in one
/// transaction, each with its own uid, `Message-ID`, sender, subject, `Cc`
/// and date: ingest commits one transaction a row and costs about 20 ms a row
/// in a debug build, which at 50 000 rows is a quarter of an hour, where the
/// copies take a second. The listing reads `messages` alone, so a copy lists
/// exactly as an ingested row with the same columns would; it has no blob rows
/// and no search index entry, which nothing here reads.
fn seed_bulk(root: &Path, count: usize) {
    let dir = fixture::account_dir(root, ACCOUNT);
    std::fs::create_dir_all(&dir).expect("the account directory");
    let store = Store::open(dir.join("store.sqlite3")).expect("open the fixture store");
    let blobs = BlobStore::new(dir.join("blobs"));
    let template = bulk_email(0);
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account: ACCOUNT,
            mailbox: MAILBOX,
            uid: 1,
            email: &template,
            raw: None,
        },
    )
    .expect("ingest the template row");

    let tx = store.immediate_transaction().expect("a write transaction");
    {
        let mut copy = tx
            .prepare(
                "INSERT INTO messages (
                    account, mailbox, uid, message_id, from_, to_, cc, reply_to, bcc,
                    subject, date_sort, date_display, flags, in_reply_to, references_,
                    thread_id, snippet, has_attachments, body_blob, raw_blob, size
                 )
                 SELECT account, mailbox, ?1, ?2, ?3, to_, ?4, reply_to, bcc,
                    ?5, ?6, ?7, flags, in_reply_to, references_,
                    ?2, snippet, has_attachments, body_blob, raw_blob, size
                 FROM messages WHERE account = ?8 AND mailbox = ?9 AND uid = 1",
            )
            .expect("the copy statement");
        for n in 1..count {
            let email = bulk_email(n);
            let date_sort = chrono::DateTime::parse_from_rfc2822(&email.date)
                .expect("the date parses")
                .timestamp();
            copy.execute(rusqlite::params![
                1 + n as i64,
                email.message_id,
                email.from,
                email.cc,
                email.subject,
                date_sort,
                email.date,
                ACCOUNT,
                MAILBOX,
            ])
            .expect("copy a bulk row");
        }
    }
    tx.commit().expect("commit the bulk rows");
}

/// The `n`th bulk message.
fn bulk_email(n: usize) -> mailypoppins::parse::FetchedEmail {
    let mut email = fixture::email(
        &format!("Sender Number {n} <sender{n}@example.com>"),
        "Sylvain Hellin <sylvain@example.com>",
        &format!("Wochenbericht Nummer {n} über die Anträge des Quartals"),
        &chrono::DateTime::from_timestamp(1_700_000_000 + 60 * n as i64, 0)
            .expect("a date in range")
            .to_rfc2822(),
        "",
    );
    email.message_id = Some(format!("<bulk-{n}@example.com>"));
    email.cc = n.is_multiple_of(3).then(|| "team@example.com".to_string());
    email
}
