//! `LST-10`: the conversation a message belongs to, read over the socket
//! (#0126, plan unit P5-U10d).
//!
//! This is a **contract test**, written before `message.thread` exists and
//! against the shape fixed here, in `docs/daemon-protocol.md` and in
//! `crates/mp-protocol/fixtures/message.thread.{request,response}.json`. It
//! compiles at HEAD and fails at HEAD with `-32601`, which is the proof there
//! is no stub behind it. The implementer does not edit this file.
//!
//! # The surface under test
//!
//! ```text
//! message.thread {account, row_id|id|selector, mailbox?}
//!                -> {account, thread_id, subject, messages: [ThreadMessage]}
//! ```
//!
//! # Why a method and not a fold the client performs
//!
//! `src/tui/app/keys.rs`'s `open_thread_overlay` opens the account's store,
//! reads the cursor row with `read::find_by_id`, takes its `thread_id` and
//! folds `read::thread_messages` over it. Nothing a client holds can produce
//! that answer: a conversation is a query over every mailbox of the account,
//! keyed on a column ingest wrote, and the client holds one mailbox's listing
//! at a time. It is the last read in `src/tui/app/` that no method answers,
//! and it is what keeps `crates/mp-tui` linked to the store.
//!
//! # Why a row type of its own
//!
//! A `MessageListRow` names no mailbox, because a listing names its mailbox
//! once and every row is in it. A conversation is the opposite: the Inbox copy
//! and the archived original are two rows of one answer, and the overlay's
//! `Enter` switches mailbox when it opens the highlighted one. So a thread row
//! carries `mailbox`, and carries nothing the overlay does not render: no
//! `uid`, no `selector`, no recipients, and no `date_sort`, because the daemon
//! orders the conversation and a client that re-sorted it would be inventing
//! an order the overlay does not have.
//!
//! # The two facts that are easy to get wrong
//!
//! **One row per `Message-ID`.** `thread_messages` collapses the copies of one
//! message to the first the order yields; a client that received both would
//! draw the same message twice.
//!
//! **`current` is decided on the `Message-ID`, not on the row id.** The
//! surviving copy of the addressed message may be the other one, so the marked
//! row can carry an `id` the call did not name. That is what the overlay's
//! cursor starts on, and it is why the flag is served rather than computed
//! client-side from the row id it asked about.
//!
//! # What this file does not assert
//!
//! The *grouping*. Which messages share a `thread_id` is ingest's decision and
//! `src/ingest.rs` owns its tests; the fixture below seeds a chain through
//! real RFC822 headers so the daemon reads a thread ingest built, rather than
//! one this file asserted into existence.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::listing::ThreadListing;
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{read, BlobStore, Store};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The method under test, written once so a rename is one diff.
const METHOD: &str = "message.thread";

/// Every field of a thread row, which is what the fixture and
/// `docs/daemon-protocol.md` document and what a client may read.
const ROW_FIELDS: [&str; 7] = [
    "current",
    "date_display",
    "flags",
    "from",
    "id",
    "mailbox",
    "message_id",
];

/// Every field of the answer.
const RESULT_FIELDS: [&str; 4] = ["account", "messages", "subject", "thread_id"];

/// The `Message-ID` of the conversation's root, which the fixture puts in the
/// inbox.
const ROOT: &str = "faden-1@example.com";

/// The reply the fixture files in `sent`.
const REPLY: &str = "faden-2@example.com";

/// The reply to the reply, back in the inbox, which is where the overlay is
/// opened from.
const LAST: &str = "faden-3@example.com";

/// A message of its own, in the same mailbox and with no relatives.
const LONELY: &str = "allein@example.com";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root and a daemon serving it. The field order is the drop order.
struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory it was
    /// reading goes away.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    /// The shared read fixture plus a three-message conversation and one lone
    /// message, seeded here rather than in `support::read_fixture` so no other
    /// suite's row counts move.
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary thread-slice root");
        fixture::seed(tmp.path());
        seed_thread(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

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
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.root().to_path_buf(),
                    config_dir: self.root().to_path_buf(),
                },
                &[],
                &[],
            ),
        )
        .await
        .expect("a compatible handshake succeeds");
        conn
    }

    /// The row id of the message whose `Message-ID` key is `message_id`, read
    /// out of the fixture store the way the daemon reads it.
    fn row_id(&self, account: &str, mailbox: &str, message_id: &str) -> i64 {
        let store = fixture::store(self.root(), account);
        read::list_mailbox(&store, account, mailbox)
            .unwrap_or_else(|e| panic!("listing {account}/{mailbox}: {e:#}"))
            .into_iter()
            .find(|row| row.message_id.contains(message_id))
            .unwrap_or_else(|| panic!("the fixture holds {message_id} in {mailbox}"))
            .id
    }

    /// The conversation as the store itself folds it: the oracle the answer is
    /// measured against, rather than a second copy of the expected order.
    fn thread_of(&self, account: &str, row_id: i64) -> Vec<read::MessageRow> {
        let store = fixture::store(self.root(), account);
        let row = read::find_by_id(&store, row_id)
            .expect("reading the row")
            .expect("the fixture holds the row");
        let thread_id = row.thread_id.clone().unwrap_or_else(|| row.message_id.clone());
        read::thread_messages(&store, account, &thread_id).expect("folding the conversation")
    }
}

/// One RFC822 message, headers the threading pass reads included.
fn raw(message_id: &str, in_reply_to: Option<&str>, references: &[&str], body: &str) -> Vec<u8> {
    let mut head = format!(
        "From: Ivana <ivana@example.com>\r\n\
         To: alpha@example.com\r\n\
         Subject: Faden\r\n\
         Date: Thu, 2 Jul 2026 13:57:30 +0200\r\n\
         Message-ID: <{message_id}>\r\n"
    );
    if let Some(parent) = in_reply_to {
        head.push_str(&format!("In-Reply-To: <{parent}>\r\n"));
    }
    if !references.is_empty() {
        let list: Vec<String> = references.iter().map(|id| format!("<{id}>")).collect();
        head.push_str(&format!("References: {}\r\n", list.join(" ")));
    }
    head.push_str("\r\n");
    head.push_str(body);
    head.into_bytes()
}

/// Ingest one message with its raw bytes, which is what gives the threading
/// pass the `In-Reply-To` and `References` headers it groups on.
fn ingest_raw(root: &Path, mailbox: &str, uid: i64, message_id: &str, bytes: &[u8], date: &str) {
    let dir = fixture::account_dir(root, fixture::ACCOUNT);
    let store = Store::open(dir.join("store.sqlite3")).expect("open the fixture store");
    let blobs = BlobStore::new(dir.join("blobs"));
    let mut email = fixture::email(
        "Ivana <ivana@example.com>",
        "alpha@example.com",
        "Faden",
        date,
        "im faden\n",
    );
    email.message_id = Some(format!("<{message_id}>"));
    email.flags = mailypoppins::types::MessageFlags::seen(true);
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account: fixture::ACCOUNT,
            mailbox,
            uid,
            email: &email,
            raw: Some(bytes),
        },
    )
    .expect("ingest a conversation message");
}

/// A three-message chain across two mailboxes, plus one message with no
/// relatives at all.
fn seed_thread(root: &Path) {
    ingest_raw(
        root,
        "inbox",
        101,
        ROOT,
        &raw(ROOT, None, &[], "der faden beginnt\n"),
        "Thu, 2 Jul 2026 13:57:30 +0200",
    );
    ingest_raw(
        root,
        "sent",
        102,
        REPLY,
        &raw(REPLY, Some(ROOT), &[ROOT], "die antwort\n"),
        "Thu, 2 Jul 2026 15:02:11 +0200",
    );
    ingest_raw(
        root,
        "inbox",
        103,
        LAST,
        &raw(LAST, Some(REPLY), &[ROOT, REPLY], "und noch eine\n"),
        "Fri, 3 Jul 2026 09:14:00 +0200",
    );
    ingest_raw(
        root,
        "inbox",
        104,
        LONELY,
        &raw(LONELY, None, &[], "niemand antwortet\n"),
        "Sat, 4 Jul 2026 10:00:00 +0200",
    );
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

async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("this call must be refused");
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{method}: expected a typed RPC error, got {other:?}"),
    }
}

/// The keys of a JSON object, sorted, or a failure naming what came instead.
fn assert_keys(value: &Value, expected: &[&str], label: &str) {
    let map = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} is a JSON object, got {value}"));
    let mut found: Vec<&str> = map.keys().map(String::as_str).collect();
    found.sort_unstable();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    assert_eq!(
        found, want,
        "{label} carries exactly these fields, got {value}"
    );
}

// ---------------------------------------------------------------------------
// 0. The fixture is a conversation ingest really threaded
// ---------------------------------------------------------------------------

/// The premise of every row below: the seeded chain is one thread because
/// ingest said so, not because this file asserted it.
///
/// It needs no daemon and **passes at HEAD**, deliberately. A fixture whose
/// `In-Reply-To` headers did not group would make the rows below pass or fail
/// for a reason that has nothing to do with the method.
#[test]
fn the_fixture_is_a_conversation_ingest_threaded_itself() {
    let tmp = TempDir::new().expect("a temporary thread-fixture root");
    fixture::seed(tmp.path());
    seed_thread(tmp.path());

    let store = fixture::store(tmp.path(), fixture::ACCOUNT);
    let rows = read::list_mailbox(&store, fixture::ACCOUNT, "inbox").expect("listing the inbox");
    let last = rows
        .iter()
        .find(|row| row.message_id.contains(LAST))
        .expect("the fixture holds the last message of the chain");
    let thread_id = last.thread_id.clone().unwrap_or_else(|| last.message_id.clone());
    let thread =
        read::thread_messages(&store, fixture::ACCOUNT, &thread_id).expect("folding the thread");
    let ids: Vec<&str> = thread.iter().map(|row| row.message_id.as_str()).collect();
    assert_eq!(
        ids.len(),
        3,
        "the chain is three messages and ingest grouped them, got {ids:?}"
    );
    assert!(ids[0].contains(ROOT), "oldest first, got {ids:?}");
    assert!(ids[2].contains(LAST));

    let lonely = rows
        .iter()
        .find(|row| row.message_id.contains(LONELY))
        .expect("the fixture holds the lone message");
    let alone = read::thread_messages(
        &store,
        fixture::ACCOUNT,
        &lonely
            .thread_id
            .clone()
            .unwrap_or_else(|| lonely.message_id.clone()),
    )
    .expect("folding the lone message's thread");
    assert_eq!(alone.len(), 1, "a lone message is its own conversation");

    // The premise of `a_message_the_store_holds_twice_is_one_row`: a second
    // copy in another mailbox is a second row, and the fold still yields three
    // messages.
    drop(store);
    ingest_raw(
        tmp.path(),
        "Team/Reports",
        201,
        ROOT,
        &raw(ROOT, None, &[], "der faden beginnt\n"),
        "Thu, 2 Jul 2026 13:57:30 +0200",
    );
    let store = fixture::store(tmp.path(), fixture::ACCOUNT);
    let copies = read::find_by_message_id(&store, fixture::ACCOUNT, &format!("<{ROOT}>"))
        .expect("reading both copies");
    assert_eq!(
        copies.len(),
        2,
        "the same message in two mailboxes is two rows in the store"
    );
    let folded =
        read::thread_messages(&store, fixture::ACCOUNT, &thread_id).expect("folding again");
    assert_eq!(
        folded.len(),
        3,
        "and one conversation of three messages, because the fold dedupes by Message-ID"
    );
}

// ---------------------------------------------------------------------------
// 1. The method is registered
// ---------------------------------------------------------------------------

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the name appearing there is what
/// says it is registered rather than merely documented.
///
/// **Fails at HEAD**: nothing registers the method, so it is in no capability
/// list.
#[tokio::test]
async fn the_daemon_advertises_the_conversation_read() {
    let slice = Slice::start();
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&socket_path(slice.root())),
    )
    .await
    .expect("connect");
    let result = within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: slice.root().to_path_buf(),
                config_dir: slice.root().to_path_buf(),
            },
            &[],
            &[],
        ),
    )
    .await
    .expect("handshake");

    let capabilities: Vec<&str> = result.capabilities.iter().map(String::as_str).collect();
    assert!(
        capabilities.contains(&METHOD),
        "{METHOD} must be advertised beside the reads it joins, got {capabilities:?}"
    );
    for sibling in ["message.get", "message.list"] {
        assert!(
            capabilities.contains(&sibling),
            "{sibling} is the family {METHOD} joins and must still be there"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The conversation
// ---------------------------------------------------------------------------

/// The answer is the store's own fold, oldest first, across every mailbox the
/// conversation touches.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_conversation_is_the_store_s_own_fold_oldest_first() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let opened = slice.row_id(fixture::ACCOUNT, "inbox", LAST);

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": opened}),
    )
    .await;
    assert_keys(&result, &RESULT_FIELDS, "the conversation");

    let thread: ThreadListing =
        serde_json::from_value(result.clone()).expect("the answer decodes into the typed listing");
    let rows = slice.thread_of(fixture::ACCOUNT, opened);
    assert_eq!(
        rows.len(),
        3,
        "the fixture's chain is three messages; ingest grouped {} of them",
        rows.len()
    );
    assert_eq!(
        thread.messages.len(),
        rows.len(),
        "the answer and the store agree about how many messages the thread holds"
    );
    for (wire, row) in thread.messages.iter().zip(rows.iter()) {
        assert_eq!(wire.id, row.id, "the rows are in the store's own order");
        assert_eq!(wire.message_id, row.message_id);
    }

    assert_eq!(
        thread.account, fixture::ACCOUNT,
        "the answer names the account it read"
    );
    assert_eq!(
        thread.thread_id,
        rows[0].thread_id.clone().unwrap_or_default(),
        "the answer names the thread ingest assigned"
    );
    let mailboxes: Vec<&str> = thread
        .messages
        .iter()
        .map(|message| message.mailbox.as_str())
        .collect();
    assert_eq!(
        mailboxes,
        vec!["inbox", "sent", "inbox"],
        "a conversation crosses mailboxes and every row says which one it is in"
    );
}

/// Every value the overlay renders is on the thread row, carrying the store's
/// own value, and nothing else is.
///
/// `open_thread_overlay` builds a `ThreadEntry` out of `r.id`, `r.mailbox`,
/// `r.from`, `r.date_display`, `r.flags()` and `r.message_id`; the subject of
/// the *opened* row is the overlay's title. That is the whole read, and this is
/// the assertion that the data to draw it is on the wire.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn the_row_carries_every_field_the_overlay_renders() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let opened = slice.row_id(fixture::ACCOUNT, "inbox", LAST);

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": opened}),
    )
    .await;
    for (index, row) in result["messages"]
        .as_array()
        .expect("the conversation carries rows")
        .iter()
        .enumerate()
    {
        assert_keys(row, &ROW_FIELDS, &format!("messages[{index}]"));
    }

    let thread: ThreadListing = serde_json::from_value(result).expect("the answer decodes");
    let rows = slice.thread_of(fixture::ACCOUNT, opened);
    for (wire, row) in thread.messages.iter().zip(rows.iter()) {
        let flags = row.flags();
        assert_eq!(wire.mailbox, row.mailbox);
        assert_eq!(wire.from, row.from.clone().unwrap_or_default());
        assert_eq!(
            wire.date_display,
            row.date_display.clone().unwrap_or_default(),
            "the `Date:` header travels as the store holds it, and the client renders it"
        );
        assert_eq!(wire.flags.seen, flags.seen);
        assert_eq!(wire.flags.answered, flags.answered);
        assert_eq!(wire.flags.forwarded, flags.forwarded);
        assert_eq!(wire.flags.flagged, flags.flagged);
    }

    let subject = rows
        .iter()
        .find(|row| row.message_id.contains(LAST))
        .and_then(|row| row.subject.clone())
        .unwrap_or_default();
    assert_eq!(
        thread.subject, subject,
        "the title is the opened message's own subject, with no placeholder invented"
    );
}

/// Exactly one row is `current`, and it is the addressed message, matched on
/// its `Message-ID`.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn exactly_one_row_is_the_message_the_thread_was_opened_from() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for message_id in [ROOT, LAST] {
        let mailbox = "inbox";
        let opened = slice.row_id(fixture::ACCOUNT, mailbox, message_id);
        let result = call(
            &mut conn,
            METHOD,
            json!({"account": fixture::ACCOUNT, "row_id": opened}),
        )
        .await;
        let thread: ThreadListing = serde_json::from_value(result).expect("the answer decodes");

        let current: Vec<&str> = thread
            .messages
            .iter()
            .filter(|message| message.current)
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(
            current.len(),
            1,
            "one row is the one the overlay's cursor starts on, got {current:?}"
        );
        assert!(
            current[0].contains(message_id),
            "the marked row is the message that was addressed, got {current:?}"
        );
    }
}

/// A message with no relatives answers with itself alone.
///
/// The client's "No related emails for this message in the store" line is a
/// branch on the length, which is why this is a one-row answer and not an
/// empty one or a refusal.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_message_with_no_relatives_is_a_one_row_conversation() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let opened = slice.row_id(fixture::ACCOUNT, "inbox", LONELY);

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": opened}),
    )
    .await;
    let thread: ThreadListing = serde_json::from_value(result).expect("the answer decodes");

    assert_eq!(
        thread.messages.len(),
        1,
        "a lone message is its own conversation"
    );
    assert_eq!(thread.messages[0].id, opened);
    assert!(thread.messages[0].current);
    assert!(
        thread.thread_id.contains(LONELY),
        "a message ingest threaded with nothing is the root of its own thread, got {:?}",
        thread.thread_id
    );
}

/// The same message in two mailboxes is one row of the conversation.
///
/// The store holds an Inbox copy and an archived original after a move, and the
/// overlay lists messages rather than copies.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_message_the_store_holds_twice_is_one_row() {
    let slice = Slice::start();
    // The same `Message-ID` as the chain's root, ingested into a second
    // mailbox: the copy an archive leaves behind.
    ingest_raw(
        slice.root(),
        "Team/Reports",
        201,
        ROOT,
        &raw(ROOT, None, &[], "der faden beginnt\n"),
        "Thu, 2 Jul 2026 13:57:30 +0200",
    );
    let mut conn = slice.connect().await;
    let opened = slice.row_id(fixture::ACCOUNT, "inbox", LAST);

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": opened}),
    )
    .await;
    let thread: ThreadListing = serde_json::from_value(result).expect("the answer decodes");

    let roots = thread
        .messages
        .iter()
        .filter(|message| message.message_id.contains(ROOT))
        .count();
    assert_eq!(
        roots, 1,
        "two copies of one message are one row: the conversation lists messages, not copies"
    );
    assert_eq!(
        thread.messages.len(),
        3,
        "and the conversation is still three messages long"
    );
}

// ---------------------------------------------------------------------------
// 3. Addressing and refusals
// ---------------------------------------------------------------------------

/// The three addresses reach the same conversation, and the answer does not
/// depend on which one was used.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn the_three_addresses_reach_the_same_conversation() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let opened = slice.row_id(fixture::ACCOUNT, "inbox", LAST);
    let store = fixture::store(slice.root(), fixture::ACCOUNT);
    let row = read::find_by_id(&store, opened)
        .expect("reading the row")
        .expect("the fixture holds the row");
    drop(store);

    let by_row_id = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": opened}),
    )
    .await;
    let by_id = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "id": format!("{}/{}", row.mailbox, row.uid)}),
    )
    .await;
    let by_selector = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "selector": LAST, "mailbox": "inbox"}),
    )
    .await;

    assert_eq!(by_row_id, by_id, "`row_id` and `id` name one message");
    assert_eq!(
        by_row_id, by_selector,
        "and so does the selector the user types"
    );
}

/// A bad address is `-32602`, in the read family's own words: none, two at
/// once, and a `row_id` no message has.
///
/// **Fails at HEAD**: `-32601`, where `-32602` is owed.
#[tokio::test]
async fn a_bad_address_is_invalid_params() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let opened = slice.row_id(fixture::ACCOUNT, "inbox", LAST);

    for params in [
        json!({"account": fixture::ACCOUNT}),
        json!({"account": fixture::ACCOUNT, "row_id": opened, "id": "inbox/1"}),
        json!({"account": fixture::ACCOUNT, "row_id": 9_999_999}),
        json!({"account": fixture::ACCOUNT, "selector": "nothing-resolves-to-this"}),
    ] {
        let error = call_err(&mut conn, METHOD, params.clone()).await;
        assert_eq!(
            error.code, -32602,
            "{params} is a parameter error, got {error:?}"
        );
    }
}

/// The account refusals are the read family's: `-32005` for an account no
/// configuration names, `-32006` for one with no store.
///
/// **Fails at HEAD**: `-32601`, where `-32005` and `-32006` are owed.
#[tokio::test]
async fn the_account_refusals_are_the_read_family_s() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let unknown = call_err(
        &mut conn,
        METHOD,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "row_id": 1}),
    )
    .await;
    assert_eq!(
        unknown.code,
        ErrorCode::AccountUnknown.code(),
        "got {unknown:?}"
    );

    let storeless = call_err(
        &mut conn,
        METHOD,
        json!({"account": fixture::STORELESS_ACCOUNT, "row_id": 1}),
    )
    .await;
    assert_eq!(
        storeless.code,
        ErrorCode::AccountNotReady.code(),
        "got {storeless:?}"
    );
}
