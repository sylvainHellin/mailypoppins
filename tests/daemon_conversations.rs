//! `message.list_conversations` and `message.thread`'s two new parameters over
//! a real socket (#0142, PERSO-138 rollout step 2).
//!
//! The rows hold, against a daemon process started the way every slice starts
//! one:
//!
//! - the streamed conversation rows of a mailbox equal the fold of the flat
//!   listing plus `message.thread` for the same mailbox, in Inbox, Sent and
//!   Archive, over contiguous chunks no larger than the budget plus one row
//!   plus the envelope;
//! - the ticket's acceptance scenarios: an inbox message held in Inbox and
//!   Archive, two sent replies and a draft reply are one inbox row with a
//!   `count` of three, one `unread`, `has_draft` and the later reply's date,
//!   whose expansion is four members labelled Inbox, Sent, Sent and Drafts;
//!   the same conversation is one row in Sent with the same four members;
//! - `message.thread` without the new parameters answers byte for byte what it
//!   answered before a draft joined the conversation;
//! - every refusal is the call's own error.

mod support;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::listing::{
    ConversationRow, ConversationRowsChunk, MessageListRow, MessageListStreamStarted,
    MessageListing, ThreadListing, METHOD_MESSAGE_LIST_CONVERSATIONS,
};
use mp_protocol::{frame, Notification, METHOD_MESSAGE_CONVERSATIONS, METHOD_STATE_EVENT};

use mailypoppins::daemon::methods::message::ROWS_CHUNK_BYTES_ENV;
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::parse::FetchedEmail;
use mailypoppins::store::{BlobStore, Store};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(30);

const ACCOUNT: &str = "alpha";

/// The chunk budget: a few conversations a chunk, so every mailbox streams
/// several chunks.
const SMALL_CHUNK: &str = "4096";

/// How many three-message conversations the bulk adds: an inbox root, an
/// inbox reply and a sent reply each.
const BULK_THREADS: usize = 40;

/// One account with the three role mailboxes the scenarios cross.
const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[accounts.mailboxes.sent]
server = "Sent"

[accounts.mailboxes.archive]
server = "Archive"
"#;

/// The question of the acceptance conversation, held in Inbox and Archive.
const QUESTION: &str = "<question@example.com>";

/// The two sent replies, the second the later.
const FIRST_REPLY: &str = "<first-reply@example.com>";
const SECOND_REPLY: &str = "<second-reply@example.com>";

/// The `Date:` of the second sent reply, the conversation's newest message.
const SECOND_REPLY_DATE: &str = "Mon, 1 Jan 2024 12:00:00 +0000";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory goes.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
    /// The row ids the scenarios address, by name.
    ids: HashMap<&'static str, i64>,
}

impl Slice {
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary root");
        std::fs::write(tmp.path().join("config.toml"), CONFIG).expect("write config.toml");
        let ids = seed(tmp.path());
        let daemon =
            DaemonFixture::start_with(tmp.path(), None, &[(ROWS_CHUNK_BYTES_ENV, SMALL_CHUNK)]);
        Slice { daemon, tmp, ids }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn id(&self, name: &str) -> i64 {
        self.ids[name]
    }

    /// An initialized connection, subscribed so it sees the finishes.
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
                &[METHOD_MESSAGE_LIST_CONVERSATIONS],
                &[],
            ),
        )
        .await
        .expect("a handshake requiring message.list_conversations succeeds");
        call(&mut conn, "state.bootstrap", json!({})).await;
        conn
    }
}

/// One message, `Message-ID` and flags set.
fn message(id: &str, from: &str, subject: &str, date: &str, seen: bool) -> FetchedEmail {
    let mut email = fixture::email(from, "alpha@example.com", subject, date, "body\n");
    email.message_id = Some(id.to_string());
    email.flags.seen = seen;
    email
}

/// Ingest `email` into `mailbox`, as a reply to `parent` when there is one,
/// and answer its row id.
fn ingest(
    store: &Store,
    blobs: &BlobStore,
    mailbox: &str,
    uid: i64,
    email: &FetchedEmail,
    parent: Option<&str>,
) -> i64 {
    let raw = parent.map(|parent| format!("In-Reply-To: {parent}\r\nReferences: {parent}\r\n\r\n"));
    ingest_message(
        store,
        blobs,
        &IngestInput {
            account: ACCOUNT,
            mailbox,
            uid,
            email,
            raw: raw.as_deref().map(str::as_bytes),
        },
    )
    .expect("ingest a fixture message")
    .row_id
}

/// The acceptance conversation, a lone inbox message and [`BULK_THREADS`]
/// three-message conversations, answering the ids the scenarios address.
fn seed(root: &Path) -> HashMap<&'static str, i64> {
    let dir = fixture::account_dir(root, ACCOUNT);
    std::fs::create_dir_all(&dir).expect("the account directory");
    let store = Store::open(dir.join("store.sqlite3")).expect("open the fixture store");
    let blobs = BlobStore::new(dir.join("blobs"));
    let mut ids = HashMap::new();

    // The archived copy first, so it is the copy the fold keeps by default
    // and `prefer_mailbox` has something to change.
    let question_date = "Mon, 1 Jan 2024 08:00:00 +0000";
    let ivana = "Ivana <ivana@example.com>";
    ids.insert(
        "question_archive",
        ingest(
            &store,
            &blobs,
            "archive",
            1,
            &message(QUESTION, ivana, "Frage", question_date, true),
            None,
        ),
    );
    ids.insert(
        "question_inbox",
        ingest(
            &store,
            &blobs,
            "inbox",
            1,
            &message(QUESTION, ivana, "Frage", question_date, false),
            None,
        ),
    );
    let me = "Alpha <alpha@example.com>";
    ids.insert(
        "first_reply",
        ingest(
            &store,
            &blobs,
            "sent",
            1,
            &message(
                FIRST_REPLY,
                me,
                "Re: Frage",
                "Mon, 1 Jan 2024 10:00:00 +0000",
                true,
            ),
            Some(QUESTION),
        ),
    );
    ids.insert(
        "second_reply",
        ingest(
            &store,
            &blobs,
            "sent",
            2,
            &message(SECOND_REPLY, me, "Re: Frage", SECOND_REPLY_DATE, true),
            Some(FIRST_REPLY),
        ),
    );
    ids.insert(
        "lone",
        ingest(
            &store,
            &blobs,
            "inbox",
            2,
            &message(
                "<lone@example.com>",
                "Bot <bot@example.com>",
                "Einzeln",
                "Mon, 1 Jan 2024 11:00:00 +0000",
                true,
            ),
            None,
        ),
    );

    for t in 0..BULK_THREADS {
        let day = 2 + t % 27;
        let root = format!("<bulk-{t}@example.com>");
        let mut first = message(
            &root,
            &format!("Sender {t} <sender{t}@example.com>"),
            &format!("Thema {t}"),
            &format!("{day} Feb 2023 08:{:02}:00 +0000", t % 60),
            t % 3 != 0,
        );
        first.flags.flagged = t % 5 == 0;
        first.has_attachments = t % 4 == 0;
        ingest(&store, &blobs, "inbox", 100 + 2 * t as i64, &first, None);
        if t % 7 == 0 {
            ingest(&store, &blobs, "archive", 100 + t as i64, &first, None);
        }
        let reply_id = format!("<bulk-{t}-reply@example.com>");
        let reply = message(
            &reply_id,
            &format!("Other {t} <OTHER{t}@example.com>"),
            &format!("Re: Thema {t}"),
            &format!("{day} Feb 2023 09:{:02}:00 +0000", t % 60),
            t % 2 == 0,
        );
        ingest(
            &store,
            &blobs,
            "inbox",
            101 + 2 * t as i64,
            &reply,
            Some(&root),
        );
        let sent = message(
            &format!("<bulk-{t}-sent@example.com>"),
            me,
            &format!("Re: Thema {t}"),
            &format!("{day} Feb 2023 10:{:02}:00 +0000", t % 60),
            true,
        );
        ingest(
            &store,
            &blobs,
            "sent",
            100 + t as i64,
            &sent,
            Some(&reply_id),
        );
    }
    ids
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

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

async fn refused(conn: &mut Connection, method: &str, params: Value) -> mp_protocol::RpcError {
    match within(method, conn.call(method, params.clone())).await {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("{method} {params} was not refused: {other:?}"),
    }
}

/// The finish payload when `notification` is `operation.finished` of `id`.
fn finish_of(notification: &Notification, id: &str) -> Option<Value> {
    (notification.method == METHOD_STATE_EVENT
        && notification.params["kind"] == json!("operation.finished")
        && notification.params["payload"]["operation_id"] == json!(id))
    .then(|| notification.params["payload"].clone())
}

/// The bytes of a `message.conversations` frame with no rows.
fn envelope_bytes(offset: u64, id: &str) -> usize {
    let empty = Notification {
        jsonrpc: "2.0".to_string(),
        method: METHOD_MESSAGE_CONVERSATIONS.to_string(),
        params: json!({"offset": offset, "operation_id": id, "rows": []}),
    };
    frame::encode(&empty).expect("encodes").len()
}

/// Stream one mailbox's conversations and answer them, checking on the way
/// that the chunks are contiguous, each at most the budget plus one row plus
/// the envelope, all of them ahead of a `succeeded` finish, and as many rows
/// as the answer announced.
async fn conversations(conn: &mut Connection, mailbox: &str) -> Vec<ConversationRow> {
    let answer = call(
        conn,
        METHOD_MESSAGE_LIST_CONVERSATIONS,
        json!({"account": ACCOUNT, "mailbox": mailbox}),
    )
    .await;
    let started: MessageListStreamStarted =
        serde_json::from_value(answer).expect("the answer decodes");
    assert_eq!(started.mailbox, mailbox);
    let id = started.operation_id.clone();
    let budget: usize = SMALL_CHUNK.parse().expect("a number");
    let mut rows: Vec<ConversationRow> = Vec::new();
    loop {
        let notification = within("the next notification", conn.next_notification())
            .await
            .expect("the daemon keeps the connection open");
        if let Some(finish) = finish_of(&notification, &id) {
            assert_eq!(finish["state"], json!("succeeded"), "{finish}");
            assert_eq!(
                finish["result"],
                json!({"account": ACCOUNT, "mailbox": mailbox, "total": started.total})
            );
            break;
        }
        if notification.method != METHOD_MESSAGE_CONVERSATIONS {
            continue;
        }
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
        assert_eq!(chunk.operation_id, id);
        assert_eq!(
            chunk.offset as usize,
            rows.len(),
            "the chunks are contiguous"
        );
        assert!(!chunk.rows.is_empty(), "no chunk is empty");
        let envelope = envelope_bytes(chunk.offset, &id);
        assert!(
            bytes <= budget + largest + envelope + 1,
            "a {bytes}-byte frame is over {budget} + {largest} + {envelope}"
        );
        rows.extend(chunk.rows);
    }
    assert_eq!(
        rows.len() as u64,
        started.total,
        "every row before the finish"
    );
    rows
}

/// `message.thread` with the conversation list's two parameters.
async fn expanded(conn: &mut Connection, row_id: i64, mailbox: &str) -> ThreadListing {
    serde_json::from_value(
        call(
            conn,
            "message.thread",
            json!({
                "account": ACCOUNT,
                "row_id": row_id,
                "prefer_mailbox": mailbox,
                "include_drafts": true,
            }),
        )
        .await,
    )
    .expect("the thread decodes")
}

/// The address a participant is compared by, as the daemon compares it.
fn address(from: &str) -> String {
    match (from.rfind('<'), from.rfind('>')) {
        (Some(open), Some(close)) if open < close => &from[open + 1..close],
        _ => from,
    }
    .trim()
    .to_lowercase()
}

/// The conversations of `mailbox` as a client could fold them without the
/// method: the flat listing, grouped by the `thread_id` `message.thread`
/// answers for each row, each group's figures from its flat rows and its
/// expansion.
async fn folded(conn: &mut Connection, mailbox: &str) -> Vec<ConversationRow> {
    let listing: MessageListing = serde_json::from_value(
        call(
            conn,
            "message.list",
            json!({"account": ACCOUNT, "mailbox": mailbox, "limit": null}),
        )
        .await,
    )
    .expect("the listing decodes");
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (Vec<MessageListRow>, ThreadListing)> = HashMap::new();
    for row in listing.messages {
        let thread = expanded(conn, row.id, mailbox).await;
        let key = thread.thread_id.clone();
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(key.clone(), (Vec::new(), thread));
        }
        groups.get_mut(&key).expect("held").0.push(row);
    }
    order
        .into_iter()
        .map(|key| {
            let (rows, thread) = groups.remove(&key).expect("held");
            let messages: Vec<_> = thread
                .messages
                .iter()
                .filter(|member| member.draft_id.is_none())
                .collect();
            let mut seen = HashSet::new();
            let participants: Vec<String> = messages
                .iter()
                .rev()
                .filter(|member| !member.from.is_empty() && seen.insert(address(&member.from)))
                .take(5)
                .map(|member| member.from.clone())
                .collect();
            let unread: HashSet<&str> = rows
                .iter()
                .filter(|row| !row.flags.seen)
                .map(|row| row.message_id.as_str())
                .collect();
            let mut mailboxes: Vec<String> = messages
                .iter()
                .map(|member| member.mailbox.clone())
                .collect();
            mailboxes.sort();
            mailboxes.dedup();
            ConversationRow {
                count: messages.len() as u64,
                date_display: messages.last().expect("a member").date_display.clone(),
                // Not derivable from a thread row; checked as an order below.
                date_sort: String::new(),
                flagged: rows.iter().any(|row| row.flags.flagged),
                has_attachments: false,
                has_draft: thread.messages.iter().any(|m| m.draft_id.is_some()),
                latest: rows[0].clone(),
                mailboxes,
                member_ids: rows.iter().map(|row| row.id).collect(),
                participants,
                thread_id: key,
                unread: unread.len() as u64,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// The streamed conversations of Inbox, Sent and Archive are the fold of the
/// flat listing plus `message.thread` for the same mailbox, newest first.
#[tokio::test]
async fn streamed_conversations_equal_the_fold_of_the_listing_and_the_thread() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    call(
        &mut conn,
        "draft.reply",
        json!({"account": ACCOUNT, "source": {"row_id": slice.id("question_inbox")}}),
    )
    .await;

    for mailbox in ["inbox", "sent", "archive"] {
        let streamed = conversations(&mut conn, mailbox).await;
        let expected = folded(&mut conn, mailbox).await;
        assert_eq!(
            streamed.len(),
            expected.len(),
            "{mailbox}: one row per conversation with a member here"
        );
        let mut by_thread: HashMap<&str, &ConversationRow> = HashMap::new();
        for row in &streamed {
            assert!(
                by_thread.insert(row.thread_id.as_str(), row).is_none(),
                "{mailbox}: {} is listed twice",
                row.thread_id
            );
        }
        for want in &expected {
            let got = by_thread
                .get(want.thread_id.as_str())
                .unwrap_or_else(|| panic!("{mailbox}: {} is not listed", want.thread_id));
            let label = format!("{mailbox} {}", want.thread_id);
            assert_eq!(got.count, want.count, "{label}: count");
            assert_eq!(got.unread, want.unread, "{label}: unread");
            assert_eq!(got.flagged, want.flagged, "{label}: flagged");
            assert_eq!(got.has_draft, want.has_draft, "{label}: has_draft");
            assert_eq!(got.latest, want.latest, "{label}: latest");
            assert_eq!(got.member_ids, want.member_ids, "{label}: member_ids");
            assert_eq!(got.date_display, want.date_display, "{label}: date");
            assert_eq!(got.participants, want.participants, "{label}: participants");
            assert!(
                want.mailboxes.iter().all(|m| got.mailboxes.contains(m)),
                "{label}: {:?} names every mailbox of {:?}",
                got.mailboxes,
                want.mailboxes
            );
            assert!(got.mailboxes.windows(2).all(|w| w[0] < w[1]), "{label}");
        }
        assert!(
            streamed
                .windows(2)
                .all(|w| (&w[0].date_sort, w[0].latest.id) > (&w[1].date_sort, w[1].latest.id)),
            "{mailbox}: newest first, then by the latest row's id"
        );
    }
}

/// The ticket's scenarios: an inbox message held in Inbox and Archive, two
/// sent replies and a draft reply.
#[tokio::test]
async fn an_inbox_message_its_two_sent_replies_and_a_draft_are_one_conversation() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let question = slice.id("question_inbox");
    let plain = json!({"account": ACCOUNT, "row_id": question});
    let before = call(&mut conn, "message.thread", plain.clone()).await;

    // Before the draft: no `has_draft`.
    let inbox = conversations(&mut conn, "inbox").await;
    let row = inbox
        .iter()
        .find(|row| row.thread_id == QUESTION)
        .expect("the conversation is listed in the inbox");
    assert!(!row.has_draft);

    let draft = call(
        &mut conn,
        "draft.reply",
        json!({"account": ACCOUNT, "source": {"row_id": question}}),
    )
    .await;
    let draft_id = draft["id"].as_str().expect("a draft id").to_string();

    // One inbox row, `(3)`, the draft marker and the later reply's date.
    let inbox = conversations(&mut conn, "inbox").await;
    let rows: Vec<&ConversationRow> = inbox.iter().filter(|r| r.thread_id == QUESTION).collect();
    assert_eq!(rows.len(), 1, "one row for the conversation");
    let row = rows[0];
    assert_eq!(row.count, 3, "the question counts once though held twice");
    assert_eq!(row.unread, 1, "the inbox copy of the question is unread");
    assert!(row.has_draft);
    assert_eq!(row.date_display, SECOND_REPLY_DATE);
    assert_eq!(row.member_ids, [question]);
    assert_eq!(row.latest.id, question);
    assert_eq!(row.mailboxes, ["archive", "inbox", "sent"]);
    assert_eq!(
        inbox[0].thread_id, QUESTION,
        "the later sent reply lifts the conversation to the top"
    );

    // Four members, oldest first, the question as its inbox copy.
    let thread = expanded(&mut conn, question, "inbox").await;
    let labels: Vec<&str> = thread.messages.iter().map(|m| m.mailbox.as_str()).collect();
    assert_eq!(labels, ["inbox", "sent", "sent", "drafts"]);
    assert_eq!(
        thread.messages[0].id, question,
        "the copy in the listed mailbox"
    );
    assert!(thread.messages[0].current);
    let member = thread.messages.last().expect("the draft member");
    assert_eq!(member.draft_id.as_deref(), Some(draft_id.as_str()));
    assert_eq!((member.id, member.message_id.as_str()), (0, ""));
    assert_eq!(member.from, "alpha@example.com");
    assert!(!member.current);
    let message_ids: Vec<String> = thread
        .messages
        .iter()
        .map(|m| m.message_id.clone())
        .collect();
    assert_eq!(message_ids[..3], [QUESTION, FIRST_REPLY, SECOND_REPLY]);

    // Without the two parameters the answer is the one before the draft, to
    // the byte: the archived copy the fold keeps first, and no draft.
    let after = call(&mut conn, "message.thread", plain).await;
    assert_eq!(
        serde_json::to_string(&after).unwrap(),
        serde_json::to_string(&before).unwrap()
    );
    assert_eq!(
        after["messages"][0]["id"],
        json!(slice.id("question_archive"))
    );
    for member in after["messages"].as_array().expect("messages") {
        assert!(member.get("draft_id").is_none(), "{member}");
    }

    // The same conversation is one row in Sent, with the same four members.
    let sent = conversations(&mut conn, "sent").await;
    let rows: Vec<&ConversationRow> = sent.iter().filter(|r| r.thread_id == QUESTION).collect();
    assert_eq!(rows.len(), 1);
    let row = rows[0];
    assert_eq!(row.count, 3);
    assert_eq!(row.unread, 0, "unread counts the listed mailbox only");
    assert!(row.has_draft);
    assert_eq!(
        row.member_ids,
        [slice.id("second_reply"), slice.id("first_reply")],
        "the sent members, newest first"
    );
    assert_eq!(row.date_display, SECOND_REPLY_DATE);
    let from_sent = expanded(&mut conn, slice.id("first_reply"), "sent").await;
    let same: Vec<String> = from_sent
        .messages
        .iter()
        .map(|m| m.message_id.clone())
        .collect();
    assert_eq!(same, message_ids, "the same four members");
    assert_eq!(
        from_sent.messages.last().and_then(|m| m.draft_id.clone()),
        Some(draft_id)
    );

    // In Archive the conversation is its archived copy, read.
    let archive = conversations(&mut conn, "archive").await;
    let row = archive
        .iter()
        .find(|r| r.thread_id == QUESTION)
        .expect("listed in Archive");
    assert_eq!(row.member_ids, [slice.id("question_archive")]);
    assert_eq!((row.count, row.unread), (3, 0));

    // The lone message is a conversation of one.
    let lone = inbox
        .iter()
        .find(|r| r.member_ids == [slice.id("lone")])
        .expect("the lone message is listed");
    assert_eq!((lone.count, lone.unread), (1, 0));
    assert_eq!(lone.mailboxes, ["inbox"]);
    assert!(!lone.has_draft);
}

/// Every refusal is the call's own error, issued before any operation.
#[tokio::test]
async fn a_refused_conversation_listing_is_the_calls_error() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let method = METHOD_MESSAGE_LIST_CONVERSATIONS;

    let drafts = refused(
        &mut conn,
        method,
        json!({"account": ACCOUNT, "mailbox": "drafts"}),
    )
    .await;
    assert_eq!(drafts.code, -32602, "Drafts is draft.list's: {drafts:?}");
    let unknown = refused(
        &mut conn,
        method,
        json!({"account": ACCOUNT, "mailbox": "nowhere"}),
    )
    .await;
    assert_eq!(unknown.code, -32602);
    let account = refused(
        &mut conn,
        method,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "mailbox": "inbox"}),
    )
    .await;
    assert_eq!(account.code, -32005);
    let extra = refused(
        &mut conn,
        method,
        json!({"account": ACCOUNT, "mailbox": "inbox", "limit": 3}),
    )
    .await;
    assert_eq!(extra.code, -32602);

    let question = slice.id("question_inbox");
    let prefer = refused(
        &mut conn,
        "message.thread",
        json!({"account": ACCOUNT, "row_id": question, "prefer_mailbox": "nowhere"}),
    )
    .await;
    assert_eq!(prefer.code, -32602, "{prefer:?}");
    let include = refused(
        &mut conn,
        "message.thread",
        json!({"account": ACCOUNT, "row_id": question, "include_drafts": "yes"}),
    )
    .await;
    assert_eq!(include.code, -32602, "{include:?}");
}
