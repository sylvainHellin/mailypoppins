//! `message.list_conversations` on the 50 000-row mailbox of
//! `daemon_list_stream_cap.rs` (#0142, PERSO-138): no frame of the stream is
//! over `ROWS_CHUNK_BYTES` plus one row plus the envelope.
//!
//! The rows carry no threading headers, as the bench fixture's do not, so the
//! mailbox is 50 000 conversations of one message, about 700 bytes each: some
//! 35 MB, twice the 16 MiB response cap, which is the case the ticket gives for
//! streaming the listing rather than answering it in one frame. The store is
//! seeded as that file seeds it, one row through ingest and the rest copied
//! from it in one transaction.

mod support;

use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::listing::{
    ConversationRow, ConversationRowsChunk, MessageListStreamStarted, MessageListing,
    METHOD_MESSAGE_LIST_CONVERSATIONS,
};
use mp_protocol::{frame, Notification, MAX_RESPONSE_BYTES, METHOD_MESSAGE_CONVERSATIONS};

use mailypoppins::daemon::methods::message::ROWS_CHUNK_BYTES;
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{BlobStore, Store};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

/// The rows the store is seeded with.
const ROWS: usize = 50_000;

/// The mailbox the rows are listed from, an `extra` one as in the bench
/// fixture.
const MAILBOX: &str = "Bulk";

const ACCOUNT: &str = "alpha";

/// How long one call or one stream may take: a debug build folding 50 000
/// conversations takes seconds, so this only bounds a hang.
const DEADLINE: Duration = Duration::from_secs(120);

const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts.mailboxes.extra]]
server = "Bulk"
"#;

/// A mailbox of [`ROWS`] one-message conversations streams in frames no larger
/// than the budget plus one row plus the envelope, every conversation's
/// `latest` the `message.list` row of its message, in `message.list`'s order.
#[test]
fn no_conversation_frame_passes_the_budget_on_the_large_mailbox() {
    let tmp = TempDir::new().expect("a temporary root");
    std::fs::write(tmp.path().join("config.toml"), CONFIG).expect("write config.toml");
    let seeding = Instant::now();
    seed_bulk(tmp.path(), ROWS);
    eprintln!(
        "seeded {ROWS} rows in {:.1}s",
        seeding.elapsed().as_secs_f64()
    );
    let root = tmp.path();
    let daemon = DaemonFixture::start(root);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");

    let (bounded, started, frames) = runtime.block_on(async {
        let mut conn = connect(root).await;
        let bounded: MessageListing = serde_json::from_value(
            call(
                &mut conn,
                "message.list",
                json!({"account": ACCOUNT, "mailbox": MAILBOX, "limit": 10_000}),
            )
            .await,
        )
        .expect("a bounded listing decodes");
        let (started, frames) = raw_stream(&mut conn).await;
        (bounded, started, frames)
    });

    assert_eq!(started.total, ROWS as u64, "one conversation per message");
    let mut offset = 0u64;
    let mut bytes_total = 0usize;
    for (chunk, bytes, largest) in &frames {
        assert_eq!(chunk.offset, offset, "the chunks are contiguous");
        offset += chunk.rows.len() as u64;
        bytes_total += bytes;
        let envelope = envelope_bytes(chunk.offset, &chunk.operation_id);
        assert!(
            *bytes <= ROWS_CHUNK_BYTES + largest + envelope + 1,
            "a {bytes}-byte frame is over {ROWS_CHUNK_BYTES} + {largest} + {envelope}"
        );
        assert!(*bytes < MAX_RESPONSE_BYTES / 8, "{bytes}");
    }
    assert_eq!(offset, ROWS as u64, "the chunks carry every conversation");
    assert!(
        bytes_total > MAX_RESPONSE_BYTES,
        "the listing is past the cap, which is why it streams: {bytes_total}"
    );
    let biggest = frames.iter().map(|(_, bytes, _)| *bytes).max().unwrap_or(0);
    eprintln!(
        "{} chunks, {bytes_total} bytes, the largest {biggest}",
        frames.len()
    );

    let streamed: Vec<&ConversationRow> = frames
        .iter()
        .flat_map(|(chunk, _, _)| chunk.rows.iter())
        .collect();
    for (row, listed) in streamed.iter().zip(&bounded.messages) {
        assert_eq!(&row.latest, listed, "message.list's row, in its order");
        assert_eq!(row.member_ids, [listed.id]);
        assert_eq!(row.count, 1);
    }
    daemon.stop();
}

/// The stream on a plain connection: the answer, then each chunk with the
/// bytes of its frame and of its largest row.
async fn raw_stream(
    conn: &mut Connection,
) -> (
    MessageListStreamStarted,
    Vec<(ConversationRowsChunk, usize, usize)>,
) {
    let started: MessageListStreamStarted = serde_json::from_value(
        call(
            conn,
            METHOD_MESSAGE_LIST_CONVERSATIONS,
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
        if notification.method == mp_protocol::METHOD_STATE_EVENT
            && notification.params["kind"] == json!("operation.finished")
            && notification.params["payload"]["operation_id"] == json!(id)
        {
            assert_eq!(
                notification.params["payload"]["state"],
                json!("succeeded"),
                "{notification:?}"
            );
            return (started, frames);
        }
        if notification.method != METHOD_MESSAGE_CONVERSATIONS
            || notification.params["operation_id"] != json!(id)
        {
            continue;
        }
        // The daemon writes its keys sorted, as `frame::encode` does, so
        // re-encoding the decoded frame is the frame.
        let bytes = frame::encode(&notification).expect("encodes").len();
        let largest = notification.params["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| serde_json::to_vec(row).expect("encodes").len())
            .max()
            .unwrap_or(0);
        let chunk: ConversationRowsChunk =
            serde_json::from_value(notification.params).expect("a chunk decodes");
        frames.push((chunk, bytes, largest));
    }
}

/// The bytes of a `message.conversations` frame with no rows, at `offset`.
fn envelope_bytes(offset: u64, id: &str) -> usize {
    let empty = Notification {
        jsonrpc: "2.0".to_string(),
        method: METHOD_MESSAGE_CONVERSATIONS.to_string(),
        params: json!({"offset": offset, "operation_id": id, "rows": []}),
    };
    frame::encode(&empty).expect("encodes").len()
}

/// An initialized connection, subscribed so it sees the stream's finish.
async fn connect(root: &Path) -> Connection {
    let (mut conn, _hello) = within(
        "Connection::open_requiring",
        Connection::open_requiring(
            &socket_path(root),
            ClientInfo {
                kind: ClientKind::Tui,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: root.to_path_buf(),
                config_dir: root.to_path_buf(),
            },
            &[METHOD_MESSAGE_LIST_CONVERSATIONS],
            Duration::from_secs(10),
        ),
    )
    .await
    .expect("a handshake requiring message.list_conversations succeeds");
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

/// `count` rows in `alpha/Bulk`, each its own conversation, one minute apart,
/// as `daemon_list_stream_cap.rs` seeds them: the first through ingest, the
/// rest copies of it in one transaction with their own uid, `Message-ID`,
/// sender, subject, `Cc`, date and `thread_id`.
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
