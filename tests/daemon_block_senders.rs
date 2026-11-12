//! The blocked senders over a real socket, and `mp block`, `mp unblock` and
//! `mp blocked` in front of them (PERSO-261).
//!
//! The list, the matching, the judged mark and the moved memory are
//! unit-tested beside their code (`src/daemon/senders.rs`); this file pins
//! what a client sees: the call's answer and event, the rows that move in the
//! store, and the runner that judges what a tick brought in.
//!
//! Two accounts with a server and no credentials, on the discard port with
//! the scheduler off, so nothing ever connects: `alpha`, whose last folder
//! listing has a `\Junk` folder, and `beta`, whose listing has none. A move
//! commits its row change and queues the server move (`settle: false`'s
//! path), and the drain that would run it is refused for the missing
//! password, which leaves the op queued and the row where the move put it.
//! A tick a client asks for fails the same way, after it ran its body, which
//! is what wakes the account's runner; a message the test writes into the
//! store before that tick is a message the tick brought in.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::events::KIND_SENDER_BLOCKLIST_CHANGED;

use mailypoppins::catalogue::ListedFolder;
use mailypoppins::ingest::{ingest_message, record_mailbox_cursor, IngestInput, MailboxCursor};
use mailypoppins::store::{BlobStore, Store};

use support::events::next_event;
use support::parity::{socket_path, DaemonFixture};

const DEADLINE: Duration = Duration::from_secs(20);
const ALPHA: &str = "alpha";
const BETA: &str = "beta";

const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.imap]
host = "127.0.0.1"
port = 9
username = "alpha@example.com"
sync_interval_secs = 0

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts]]
name = "beta"
default_from = "beta@example.com"

[accounts.imap]
host = "127.0.0.1"
port = 9
username = "beta@example.com"
sync_interval_secs = 0

[accounts.mailboxes.inbox]
server = "INBOX"
"#;

/// A seeded root and a daemon serving it. The field order is the drop order.
struct Slice {
    daemon: DaemonFixture,
    tmp: TempDir,
}

fn store_of(root: &Path, account: &str) -> Store {
    Store::open(root.join("accounts").join(account).join("store.sqlite3")).unwrap()
}

/// Ingest one Inbox message from `from` into `account`'s store; its
/// Message-ID is `<id>`.
fn ingest(root: &Path, account: &str, uid: i64, id: &str, from: &str) {
    let dir = root.join("accounts").join(account);
    let store = store_of(root, account);
    let blobs = BlobStore::new(dir.join("blobs"));
    let raw = format!(
        "From: {from}\r\nTo: {account}@example.com\r\nSubject: {id}\r\nMessage-ID: <{id}>\r\nDate: Wed, 30 Sep 2026 10:00:00 +0200\r\n\r\nbody\r\n"
    );
    let email = mailypoppins::parse::parse_rfc822_to_fetched_email(raw.as_bytes()).unwrap();
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account,
            mailbox: "inbox",
            uid,
            email: &email,
            raw: Some(raw.as_bytes()),
        },
    )
    .unwrap();
}

/// The configuration, and a store per account that has synced its Inbox
/// once and whose last listing has a Junk folder for `alpha` only.
fn seed(root: &Path) {
    std::fs::write(root.join("config.toml"), CONFIG).unwrap();
    for account in [ALPHA, BETA] {
        std::fs::create_dir_all(root.join("accounts").join(account)).unwrap();
        let store = store_of(root, account);
        let mut listed = vec![ListedFolder::named("INBOX").with_attributes(&["\\Inbox"])];
        if account == ALPHA {
            listed.push(ListedFolder::named("Junk").with_attributes(&["\\Junk"]));
        }
        mailypoppins::catalogue::listing::replace(&store, account, &listed).unwrap();
        record_mailbox_cursor(
            &store,
            account,
            "inbox",
            &MailboxCursor {
                uidvalidity: Some(7),
                last_uid: Some(10),
                ..MailboxCursor::default()
            },
        )
        .unwrap();
    }
}

impl Slice {
    fn start(extra: impl FnOnce(&Path)) -> Slice {
        let tmp = TempDir::new().unwrap();
        seed(tmp.path());
        extra(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// The store key of the mailbox `id` sits in, in `account`.
    fn mailbox_of(&self, account: &str, id: &str) -> String {
        store_of(self.root(), account)
            .conn()
            .query_row(
                "SELECT mailbox FROM messages WHERE message_id = ?1",
                [format!("<{id}>")],
                |row| row.get(0),
            )
            .unwrap_or_else(|e| panic!("{id} in {account}: {e}"))
    }

    fn row_id(&self, account: &str, id: &str) -> i64 {
        store_of(self.root(), account)
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE message_id = ?1",
                [format!("<{id}>")],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Wait until `account`'s runner has armed on its store.
    fn wait_armed(&self, account: &str) {
        let start = Instant::now();
        while !mailypoppins::daemon::senders::is_armed(&store_of(self.root(), account)).unwrap() {
            assert!(
                start.elapsed() < DEADLINE,
                "{account}'s blocked-senders runner never armed"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait until `id` sits in `mailbox` of `account`.
    fn wait_in(&self, account: &str, id: &str, mailbox: &str) {
        let start = Instant::now();
        loop {
            let now = self.mailbox_of(account, id);
            if now == mailbox {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "{id} stayed in {now}, never reached {mailbox}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    async fn connect(&self) -> Connection {
        let mut conn =
            tokio::time::timeout(DEADLINE, Connection::connect(&socket_path(self.root())))
                .await
                .unwrap()
                .unwrap();
        tokio::time::timeout(
            DEADLINE,
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
        .unwrap()
        .unwrap();
        conn
    }

    /// Ask `account`'s runtime for a tick and wait for it to end; with no
    /// password it fails after running its body, which is what wakes the
    /// runner.
    async fn tick(&self, conn: &mut Connection, account: &str) {
        let started = call(conn, "sync.quick", json!({"account": account}))
            .await
            .unwrap();
        let id = started["operation_id"].as_str().expect("an operation id");
        let start = Instant::now();
        loop {
            let status = call(conn, "operation.status", json!({"operation_id": id}))
                .await
                .unwrap();
            if matches!(
                status["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) {
                return;
            }
            assert!(start.elapsed() < DEADLINE, "{id} did not settle: {status}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Result<Value, ClientError> {
    tokio::time::timeout(DEADLINE, conn.call(method, params))
        .await
        .unwrap_or_else(|_| panic!("{method} went unanswered"))
}

/// The next `sender.blocklist_changed` on a bootstrapped connection.
async fn next_blocklist_event(conn: &mut Connection) -> Value {
    loop {
        let event = next_event(conn, DEADLINE, "sender.blocklist_changed").await;
        if event.kind == KIND_SENDER_BLOCKLIST_CHANGED {
            return event.payload;
        }
    }
}

/// The row events `conn` hears until every one of `expected` arrived, each as
/// `remove <resource>` for a `state.remove` of a message or `invalidate
/// <resource> <scope>` for a `state.invalidate` of a mailbox outside the
/// counts scope; the test fails after [`ROW_EVENTS_WINDOW`] naming what did
/// arrive (PERSO-286).
async fn wait_for_row_events(conn: &mut Connection, expected: &[String]) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + ROW_EVENTS_WINDOW;
    let mut seen: Vec<String> = Vec::new();
    while !expected.iter().all(|line| seen.contains(line)) {
        let notification = match tokio::time::timeout_at(deadline, conn.next_notification()).await {
            Ok(Some(notification)) => notification,
            Ok(None) => panic!("the daemon closed the connection; heard {seen:?}"),
            Err(_) => panic!("expected {expected:?} within {ROW_EVENTS_WINDOW:?}, heard {seen:?}"),
        };
        let event: mp_protocol::EventEnvelope =
            serde_json::from_value(notification.params.clone()).unwrap();
        let resource = event.payload["resource"].as_str().unwrap_or_default();
        match event.kind.as_str() {
            "state.remove" if resource.starts_with("message:") => {
                seen.push(format!("remove {resource}"));
            }
            "state.invalidate"
                if resource.starts_with("mailbox:")
                    && event.payload["scope"]["query"] != "counts" =>
            {
                seen.push(format!("invalidate {resource} {}", event.payload["scope"]));
            }
            _ => {}
        }
    }
    seen
}

/// How long [`wait_for_row_events`] waits for the events it expects.
const ROW_EVENTS_WINDOW: Duration = Duration::from_secs(8);

/// The `message:` removal of `account`'s row `uid` in `mailbox`.
fn removed(account: &str, mailbox: &str, uid: i64) -> String {
    format!("remove message:{account}/{mailbox}/{uid}")
}

/// The listing invalidate of `account`'s `mailbox`.
fn listing_stale(account: &str, mailbox: &str) -> String {
    format!("invalidate mailbox:{account}/{mailbox} {{\"query\":\"list\"}}")
}

/// A block publishes the removal of every Inbox row it moved, by the uid the
/// row had there, and the Junk listing they joined goes stale, so every
/// client drops the rows without a reload (PERSO-286). The uids are not the
/// row ids, so a removal naming the row id would not pass.
#[tokio::test]
async fn blocking_publishes_the_removal_of_each_moved_row() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 41, "spam1@x", "spam@x.com");
        ingest(root, ALPHA, 42, "friend@x", "friend@ok.example");
        ingest(root, ALPHA, 43, "spam2@x", "Spam <SPAM@x.com>");
    });
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({}))
        .await
        .unwrap();
    let mut conn = slice.connect().await;
    let blocked = call(&mut conn, "sender.block", json!({"address": "spam@x.com"}))
        .await
        .unwrap();
    assert_eq!(blocked["moved"][0]["count"], 2, "{blocked}");

    let seen = wait_for_row_events(
        &mut watcher,
        &[
            removed(ALPHA, "inbox", 41),
            removed(ALPHA, "inbox", 43),
            listing_stale(ALPHA, "Junk"),
        ],
    )
    .await;
    assert!(
        !seen.contains(&removed(ALPHA, "inbox", 42)),
        "the friend's row stayed: {seen:?}"
    );
}

/// `message.move` publishes the removal of the row from the mailbox it left
/// and the destination's listing invalidate, so a client that did not make
/// the move drops it too (PERSO-286).
#[tokio::test]
async fn a_move_publishes_the_removal_of_the_row_it_moved() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 41, "first@x", "friend@ok.example");
        ingest(root, ALPHA, 42, "second@x", "friend@ok.example");
    });
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({}))
        .await
        .unwrap();
    let mut conn = slice.connect().await;
    let row = slice.row_id(ALPHA, "second@x");
    assert_ne!(row, 42, "the uid and the row id differ");
    call(
        &mut conn,
        "message.move",
        json!({"account": ALPHA, "row_id": row, "destination": "Junk", "settle": false}),
    )
    .await
    .unwrap();
    assert_eq!(slice.mailbox_of(ALPHA, "second@x"), "Junk");

    wait_for_row_events(
        &mut watcher,
        &[removed(ALPHA, "inbox", 42), listing_stale(ALPHA, "Junk")],
    )
    .await;
}

/// Mail the runner moves after a tick is published the same way: its Inbox
/// row is removed and the Junk listing goes stale (PERSO-286).
#[tokio::test]
async fn the_runner_publishes_the_removal_of_the_rows_it_moved() {
    let slice = Slice::start(|_| {});
    slice.wait_armed(ALPHA);
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({}))
        .await
        .unwrap();
    let mut conn = slice.connect().await;
    call(
        &mut conn,
        "sender.block",
        json!({"address": "@bulk.example"}),
    )
    .await
    .unwrap();
    ingest(slice.root(), ALPHA, 1, "pad@x", "friend@ok.example");
    ingest(
        slice.root(),
        ALPHA,
        51,
        "late@x",
        "News <news@bulk.example>",
    );
    slice.tick(&mut conn, ALPHA).await;
    slice.wait_in(ALPHA, "late@x", "Junk");

    wait_for_row_events(
        &mut watcher,
        &[removed(ALPHA, "inbox", 51), listing_stale(ALPHA, "Junk")],
    )
    .await;
}

/// Blocking an address and then a domain moves the Inbox mail each names to
/// Junk, in any case, through the durable queue; an account without a Junk
/// folder keeps its mail and is named in the answer; every bootstrapped
/// client hears the new list.
#[tokio::test]
async fn blocking_moves_existing_inbox_mail_to_junk_and_names_an_account_without_one() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 1, "spam1@x", "Spammer <SPAM@x.com>");
        ingest(root, ALPHA, 2, "bulk1@x", "a@bulk.example");
        ingest(root, ALPHA, 3, "bulk2@x", "B <b@Bulk.Example>");
        ingest(root, ALPHA, 4, "friend@x", "friend@ok.example");
        ingest(root, BETA, 1, "spam2@x", "spam@x.com");
    });
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({}))
        .await
        .unwrap();
    let mut conn = slice.connect().await;

    let blocked = call(
        &mut conn,
        "sender.block",
        json!({"address": " Spam@X.com "}),
    )
    .await
    .unwrap();
    assert_eq!(blocked["entry"], "spam@x.com", "{blocked}");
    assert_eq!(blocked["added"], true);
    assert_eq!(
        blocked["moved"],
        json!([{"account": ALPHA, "mailbox": "Junk", "count": 1}])
    );
    assert_eq!(blocked["skipped"][0]["account"], BETA, "{blocked}");
    assert!(
        blocked["skipped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("no Junk folder"),
        "{blocked}"
    );
    assert_eq!(
        next_blocklist_event(&mut watcher).await,
        json!({"entries": ["spam@x.com"]})
    );

    let blocked = call(
        &mut conn,
        "sender.block",
        json!({"address": "@bulk.example"}),
    )
    .await
    .unwrap();
    assert_eq!(blocked["moved"][0]["count"], 2, "{blocked}");
    assert_eq!(blocked["entries"], json!(["@bulk.example", "spam@x.com"]));

    for id in ["spam1@x", "bulk1@x", "bulk2@x"] {
        assert_eq!(slice.mailbox_of(ALPHA, id), "Junk", "{id}");
    }
    assert_eq!(slice.mailbox_of(ALPHA, "friend@x"), "inbox");
    assert_eq!(slice.mailbox_of(BETA, "spam2@x"), "inbox");
    let (queued, _) =
        mailypoppins::pending_ops::counts(&store_of(slice.root(), ALPHA), ALPHA).unwrap();
    assert_eq!(queued, 3, "each move owes the server its half");

    let again = call(&mut conn, "sender.block", json!({"address": "SPAM@x.com"}))
        .await
        .unwrap();
    assert_eq!(again["added"], false);
    assert_eq!(again["moved"], json!([]));

    let listed = call(&mut conn, "sender.list", json!({})).await.unwrap();
    assert_eq!(listed, json!({"entries": ["@bulk.example", "spam@x.com"]}));

    for params in [
        json!({"address": "x.com"}),
        json!({"address": "a@b@c.com"}),
        json!({"address": "a@x.com", "extra": 1}),
        json!({"address": "a@x.com", "move_existing": "yes"}),
        json!({}),
    ] {
        match call(&mut conn, "sender.block", params.clone()).await {
            Err(ClientError::Rpc(error)) => assert_eq!(error.code, -32602, "{params}: {error:?}"),
            other => panic!("{params}: expected -32602, got {other:?}"),
        }
    }
}

/// One of the user's own addresses, or the domain of one, is refused with a
/// sentence that says why, and nothing is written or moved; a colleague at
/// that domain is taken.
#[tokio::test]
async fn blocking_an_own_address_or_its_domain_is_refused() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 1, "self@x", "Alpha <alpha@example.com>");
        ingest(root, ALPHA, 2, "colleague@x", "Gamma <gamma@example.com>");
    });
    let mut conn = slice.connect().await;
    for (address, sentence) in [
        (
            "Alpha@Example.com",
            "'alpha@example.com' is one of your own addresses; blocking it would move your own mail to Junk",
        ),
        (
            "beta@example.com",
            "'beta@example.com' is one of your own addresses; blocking it would move your own mail to Junk",
        ),
        (
            "@EXAMPLE.com",
            "'@example.com' is the domain of your own address alpha@example.com; blocking it would move your own mail to Junk",
        ),
    ] {
        match call(&mut conn, "sender.block", json!({"address": address})).await {
            Err(ClientError::Rpc(error)) => {
                assert_eq!(error.code, -32602, "{address}: {error:?}");
                assert_eq!(error.message, sentence, "{address}");
            }
            other => panic!("{address}: expected -32602, got {other:?}"),
        }
    }
    let listed = call(&mut conn, "sender.list", json!({})).await.unwrap();
    assert_eq!(listed, json!({"entries": []}));
    assert_eq!(slice.mailbox_of(ALPHA, "self@x"), "inbox");

    let blocked = call(
        &mut conn,
        "sender.block",
        json!({"address": "gamma@example.com"}),
    )
    .await
    .unwrap();
    assert_eq!(blocked["moved"][0]["count"], 1, "{blocked}");
    assert_eq!(slice.mailbox_of(ALPHA, "colleague@x"), "Junk");
    assert_eq!(slice.mailbox_of(ALPHA, "self@x"), "inbox");
}

/// `move_existing: false` leaves the Inbox alone, and unblocking removes the
/// entry and moves nothing back.
#[tokio::test]
async fn keeping_existing_mail_moves_nothing_and_unblocking_moves_nothing_back() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 1, "old@x", "spam@x.com");
    });
    let mut conn = slice.connect().await;
    let blocked = call(
        &mut conn,
        "sender.block",
        json!({"address": "spam@x.com", "move_existing": false}),
    )
    .await
    .unwrap();
    assert_eq!(blocked["moved"], json!([]));
    assert_eq!(slice.mailbox_of(ALPHA, "old@x"), "inbox");

    call(
        &mut conn,
        "sender.block",
        json!({"address": "@other.example"}),
    )
    .await
    .unwrap();
    let unblocked = call(
        &mut conn,
        "sender.unblock",
        json!({"address": "SPAM@X.COM"}),
    )
    .await
    .unwrap();
    assert_eq!(
        unblocked,
        json!({"entry": "spam@x.com", "removed": true, "entries": ["@other.example"]})
    );
    let again = call(
        &mut conn,
        "sender.unblock",
        json!({"address": "spam@x.com"}),
    )
    .await
    .unwrap();
    assert_eq!(again["removed"], false);
    assert_eq!(slice.mailbox_of(ALPHA, "old@x"), "inbox");
}

/// An owed-moves file that does not parse never fails an unblock: the entry
/// leaves the list, every bootstrapped client hears the new list, a second
/// unblock answers too, and the broken file is left as it is (PERSO-109).
#[tokio::test]
async fn a_broken_owed_moves_file_never_fails_an_unblock() {
    let slice = Slice::start(|_| {});
    let mut watcher = slice.connect().await;
    call(&mut watcher, "state.bootstrap", json!({}))
        .await
        .unwrap();
    let mut conn = slice.connect().await;
    for address in ["spam@x.com", "@other.example"] {
        call(
            &mut conn,
            "sender.block",
            json!({"address": address, "move_existing": false}),
        )
        .await
        .unwrap();
        next_blocklist_event(&mut watcher).await;
    }
    let owed = slice.root().join(mailypoppins::daemon::senders::OWED_FILE);
    std::fs::write(&owed, "not an owed-moves file").unwrap();

    let unblocked = call(
        &mut conn,
        "sender.unblock",
        json!({"address": "spam@x.com"}),
    )
    .await
    .unwrap();
    assert_eq!(
        unblocked,
        json!({"entry": "spam@x.com", "removed": true, "entries": ["@other.example"]})
    );
    assert_eq!(
        next_blocklist_event(&mut watcher).await,
        json!({"entries": ["@other.example"]})
    );
    let unblocked = call(
        &mut conn,
        "sender.unblock",
        json!({"address": "@other.example"}),
    )
    .await
    .unwrap();
    assert_eq!(unblocked["entries"], json!([]), "{unblocked}");
    let listed = call(&mut conn, "sender.list", json!({})).await.unwrap();
    assert_eq!(listed, json!({"entries": []}));
    assert_eq!(
        std::fs::read_to_string(&owed).unwrap(),
        "not an owed-moves file"
    );
}

/// Mail a tick brings in from a blocked sender moves to Junk once the tick
/// ends, and a message the user moved back to the Inbox stays there.
#[tokio::test]
async fn new_mail_from_a_blocked_sender_moves_after_the_tick_and_a_moved_back_one_stays() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 1, "early@x", "a@bulk.example");
    });
    slice.wait_armed(ALPHA);
    let mut conn = slice.connect().await;
    let blocked = call(
        &mut conn,
        "sender.block",
        json!({"address": "@bulk.example"}),
    )
    .await
    .unwrap();
    assert_eq!(blocked["moved"][0]["count"], 1, "{blocked}");
    assert_eq!(slice.mailbox_of(ALPHA, "early@x"), "Junk");

    // The user moves it back, as any client does.
    let row = slice.row_id(ALPHA, "early@x");
    call(
        &mut conn,
        "message.move",
        json!({"account": ALPHA, "row_id": row, "destination": "inbox", "settle": false}),
    )
    .await
    .unwrap();
    assert_eq!(slice.mailbox_of(ALPHA, "early@x"), "inbox");

    ingest(
        slice.root(),
        ALPHA,
        11,
        "late@x",
        "News <news@BULK.example>",
    );
    ingest(slice.root(), ALPHA, 12, "ham@x", "friend@ok.example");
    slice.tick(&mut conn, ALPHA).await;
    slice.wait_in(ALPHA, "late@x", "Junk");
    assert_eq!(slice.mailbox_of(ALPHA, "ham@x"), "inbox");
    assert_eq!(
        slice.mailbox_of(ALPHA, "early@x"),
        "inbox",
        "a message moved back after its move is not moved again"
    );
}

/// An account with no Junk folder keeps what a tick brings in from a blocked
/// sender, while the account that has one moves its copy.
#[tokio::test]
async fn an_account_without_junk_keeps_new_mail_from_a_blocked_sender() {
    let slice = Slice::start(|_| {});
    slice.wait_armed(ALPHA);
    slice.wait_armed(BETA);
    let mut conn = slice.connect().await;
    let blocked = call(&mut conn, "sender.block", json!({"address": "spam@x.com"}))
        .await
        .unwrap();
    assert_eq!(blocked["skipped"][0]["account"], BETA, "{blocked}");

    ingest(slice.root(), BETA, 11, "b@x", "spam@x.com");
    ingest(slice.root(), ALPHA, 11, "a@x", "spam@x.com");
    slice.tick(&mut conn, BETA).await;
    slice.tick(&mut conn, ALPHA).await;
    slice.wait_in(ALPHA, "a@x", "Junk");
    assert_eq!(slice.mailbox_of(BETA, "b@x"), "inbox");
}

#[test]
fn mp_block_unblock_and_blocked_round_trip() {
    let slice = Slice::start(|root| {
        ingest(root, ALPHA, 1, "spam1@x", "spam@x.com");
    });
    let out = slice.daemon.mp_routed(&["block", "Spam@X.com"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("Blocked spam@x.com"), "{stdout}");
    assert!(
        stdout.contains("alpha: moved 1 message(s) to Junk"),
        "{stdout}"
    );
    assert!(stdout.contains("beta has no Junk folder"), "{stdout}");

    let out = slice
        .daemon
        .mp_routed(&["block", "@bulk.example", "--keep-existing"]);
    assert!(out.status.success());
    let out = slice.daemon.mp_routed(&["blocked"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "@bulk.example\nspam@x.com\n"
    );
    let out = slice.daemon.mp_routed(&["blocked", "--json"]);
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed, json!({"entries": ["@bulk.example", "spam@x.com"]}));

    let out = slice.daemon.mp_routed(&["unblock", "spam@x.com"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("Unblocked spam@x.com"), "{stdout}");
    let out = slice.daemon.mp_routed(&["unblock", "spam@x.com"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("spam@x.com was not blocked"));
    let out = slice.daemon.mp_routed(&["unblock", "@bulk.example"]);
    assert!(out.status.success());
    let out = slice.daemon.mp_routed(&["blocked"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "No blocked senders\n");

    let out = slice.daemon.mp_routed(&["block", "example.com"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("give a full address"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = slice.daemon.mp_routed(&["block", "@example.com"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "'@example.com' is the domain of your own address alpha@example.com; blocking it would move your own mail to Junk"
        ),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = slice.daemon.mp_routed(&["blocked"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "No blocked senders\n");
    assert_eq!(slice.mailbox_of(ALPHA, "spam1@x"), "Junk");
}
