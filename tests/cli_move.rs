//! `mp move <SELECTOR> <MAILBOX>` (PERSO-235): the CLI side of the TUI's `M`.
//!
//! The command is `mp archive` with a destination of the user's choosing, so it
//! is measured against the mutation slice's fixture the same way: no account
//! has credentials and nothing listens on a port, so the settled move refuses
//! with [`fixture::CREDENTIALS_REFUSAL`] before the store is touched, and every
//! refusal that comes before that one (an unknown destination, the mailbox the
//! message is already in, an ambiguous selector, a drafts selector) is reached
//! with no server. The successful row move is asserted on `message.move` with
//! `settle: false`, the TUI's contract, which needs no backend and answers with
//! the selector `mp move` prints.
//!
//! There is no pre-daemon oracle to compare against: the command is new, so
//! these are plain assertions over the routed client rather than parity rows.

mod support;

use std::process::Output;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};

use mailypoppins::pending_ops;
use mailypoppins::store::read;

use support::mutation_fixture as fixture;
use support::parity::{mp_no_daemon, socket_path, DaemonFixture, EXIT_UNAVAILABLE};
use support::read_fixture::KICKOFF;

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// A seeded root and a daemon serving it, dropped in that order.
struct Slice {
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    fn start() -> Slice {
        Slice::start_with(|_| {})
    }

    /// [`Slice::start`] with `extra` run over the seeded root before the
    /// daemon starts.
    fn start_with(extra: impl FnOnce(&std::path::Path)) -> Slice {
        let tmp = TempDir::new().expect("a temporary move root");
        fixture::seed(tmp.path());
        extra(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn routed(&self, args: &[&str]) -> Output {
        self.daemon.mp_routed(args)
    }

    /// `(mailbox, uid, message_id)` for every row of the first account.
    fn rows(&self) -> Vec<(String, i64, String)> {
        let store = fixture::store(self.tmp.path(), fixture::ACCOUNT);
        let mut rows = Vec::new();
        for mailbox in ["archive", "inbox", "sent", "Team/Reports"] {
            for row in read::list_mailbox(&store, fixture::ACCOUNT, mailbox)
                .unwrap_or_else(|e| panic!("list {mailbox}: {e:#}"))
            {
                rows.push((row.mailbox.clone(), row.uid, row.message_id.clone()));
            }
        }
        rows.sort();
        rows
    }

    async fn connect(&self) -> Connection {
        let mut conn =
            tokio::time::timeout(DEADLINE, Connection::connect(&socket_path(self.tmp.path())))
                .await
                .expect("connect within the deadline")
                .expect("connecting to a live daemon socket succeeds");
        tokio::time::timeout(
            DEADLINE,
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.tmp.path().to_path_buf(),
                    config_dir: self.tmp.path().to_path_buf(),
                },
                &[],
                &[],
            ),
        )
        .await
        .expect("initialize within the deadline")
        .expect("a compatible handshake succeeds");
        conn
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("UTF-8 stderr")
}

/// Assert a failing run exited 1 with `sentence` in its `Error:` line, and
/// printed nothing on stdout.
fn assert_refused(out: &Output, sentence: &str) {
    assert_eq!(out.status.code(), Some(1), "a refusal exits 1: {out:?}");
    assert!(
        stderr(out).contains(&format!("Error: {sentence}")),
        "expected the refusal {sentence:?}, got:\n{}",
        stderr(out)
    );
    assert_eq!(stdout(out), "", "a refusal prints no selector");
}

fn kickoff_in_inbox() -> String {
    fixture::message_selector(fixture::ACCOUNT, "inbox", KICKOFF)
}

/// The destination is resolved the way `--mailbox` resolves a name, and one
/// the account does not have is refused with the mailboxes it does have.
#[test]
fn an_unknown_destination_is_refused_with_the_known_mailboxes() {
    let slice = Slice::start();
    let before = slice.rows();

    let out = slice.routed(&["move", &kickoff_in_inbox(), "Nowhere"]);
    assert_refused(&out, "'Nowhere' is not a mailbox of alpha (known: ");
    assert!(stderr(&out).contains("inbox"), "{}", stderr(&out));

    // Drafts is not a destination for a received message either.
    let out = slice.routed(&["move", &kickoff_in_inbox(), "drafts"]);
    assert_refused(&out, "'drafts' is not a mailbox of alpha");

    assert_eq!(slice.rows(), before, "a refused move moves nothing");
}

/// A move into the mailbox the message already sits in is refused, however
/// the user spells that mailbox, before any credential is asked for.
#[test]
fn a_move_into_the_messages_own_mailbox_is_refused() {
    let slice = Slice::start();
    let before = slice.rows();
    let selector = kickoff_in_inbox();
    for spelling in ["inbox", "Inbox", "INBOX"] {
        let out = slice.routed(&["move", &selector, spelling]);
        assert_refused(&out, &format!("{selector} is already in inbox"));
    }
    assert_eq!(slice.rows(), before);
}

/// The same refusal reaches a caller who addresses the row by `row_id`, as the
/// TUI and the desktop app do: the desktop picker over search results can
/// offer a hit's own mailbox, and that row fails alone with `-32602`.
#[tokio::test]
async fn message_move_by_row_id_into_its_own_mailbox_is_refused() {
    let slice = Slice::start();
    let before = slice.rows();
    let store = fixture::store(slice.tmp.path(), fixture::ACCOUNT);
    let stored = format!("<{KICKOFF}>");
    let row = read::list_mailbox(&store, fixture::ACCOUNT, "inbox")
        .expect("list inbox")
        .into_iter()
        .find(|row| row.message_id == stored)
        .expect("the kickoff row in inbox");
    drop(store);

    let mut conn = slice.connect().await;
    let error = tokio::time::timeout(
        DEADLINE,
        conn.call(
            "message.move",
            json!({
                "account": fixture::ACCOUNT,
                "row_id": row.id,
                "destination": "inbox",
                "settle": false,
            }),
        ),
    )
    .await
    .expect("message.move within the deadline")
    .expect_err("a move into the row's own mailbox is refused");
    let ClientError::Rpc(error) = error else {
        panic!("expected a typed RPC error, got {error:?}");
    };
    assert_eq!(error.code, -32602, "{error:?}");
    assert_eq!(
        error.message,
        format!("{} is already in inbox", kickoff_in_inbox()),
    );
    assert_eq!(slice.rows(), before, "a refused move moves nothing");
}

/// Past every address check, an account with no credentials refuses with the
/// secret store's sentence and the row stays where it was: the same ordering
/// `mp archive` keeps.
#[test]
fn a_move_refuses_before_it_touches_the_store_without_credentials() {
    let slice = Slice::start();
    let before = slice.rows();
    let out = slice.routed(&["move", &kickoff_in_inbox(), "archive"]);
    assert_refused(&out, fixture::CREDENTIALS_REFUSAL);
    assert_eq!(slice.rows(), before);
}

/// A bare message-id held in two mailboxes is ambiguous, and `--mailbox`
/// settles it the way it does for `mp archive`.
#[test]
fn mailbox_narrows_an_ambiguous_selector() {
    let slice = Slice::start();
    let out = slice.routed(&["move", fixture::SHARED, "sent"]);
    assert_refused(&out, "shared@example.com matches 2 messages in alpha");

    let out = slice.routed(&["move", "--mailbox", "inbox", fixture::SHARED, "sent"]);
    assert_refused(&out, fixture::CREDENTIALS_REFUSAL);
}

/// A second server copy of [`fixture::SHARED`] in archive, on UID 9 beside the
/// seeded one on UID 1: two rows under one Message-ID in one mailbox, as a
/// move that ran twice leaves them (PERSO-236).
fn seed_second_archive_copy(root: &std::path::Path) {
    let copy = support::read_fixture::email(
        "list@example.com",
        "sylvain@example.com",
        "Copied everywhere",
        "Sat, 27 Jun 2026 06:00:00 +0000",
        "one message, two copies in archive\n",
    );
    support::read_fixture::ingest(root, fixture::ACCOUNT, "archive", 9, &copy);
    // Ingest would rebind a second UID of a Message-ID onto the first row, so
    // the copy is ingested under its own id and renamed to the shared one.
    let store = fixture::store(root, fixture::ACCOUNT);
    let renamed = store
        .conn()
        .execute(
            "UPDATE messages SET message_id = ?1 WHERE account = ?2 AND mailbox = 'archive' AND uid = 9",
            [format!("<{}>", fixture::SHARED), fixture::ACCOUNT.to_string()],
        )
        .expect("rename the second copy");
    assert_eq!(renamed, 1);
}

/// Two copies of one Message-ID in one mailbox are refused as ambiguous with
/// two *different* selectors, each ending in its copy's UID, and each of them
/// resolves to its own row (PERSO-239). `--mailbox` cannot tell them apart,
/// so the refusal asks for the selector instead.
#[tokio::test]
async fn two_copies_in_one_mailbox_are_listed_by_uid_and_each_resolves() {
    let slice = Slice::start_with(seed_second_archive_copy);
    let in_archive = fixture::message_selector(fixture::ACCOUNT, "archive", fixture::SHARED);

    let out = slice.routed(&["move", &in_archive, "inbox"]);
    assert_refused(
        &out,
        "shared@example.com matches 2 messages in alpha; name one by its selector:",
    );
    let listed: Vec<String> = stderr(&out)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("mp://"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        listed,
        [format!("{in_archive}/1"), format!("{in_archive}/9")],
        "{}",
        stderr(&out)
    );

    // Each listed selector names one row: the CLI gets past the address to the
    // missing credentials, for archive and delete as for move.
    for selector in &listed {
        let out = slice.routed(&["move", selector, "inbox"]);
        assert_refused(&out, fixture::CREDENTIALS_REFUSAL);
        let out = slice.routed(&["archive", selector]);
        assert_refused(&out, fixture::CREDENTIALS_REFUSAL);
        let out = slice.routed(&["delete", selector]);
        assert_refused(&out, fixture::CREDENTIALS_REFUSAL);
    }
    let out = slice.routed(&["move", &format!("{in_archive}/4"), "inbox"]);
    assert_refused(
        &out,
        "no match for shared@example.com at UID 4 in the received mail index of alpha/archive",
    );

    // And the move takes the copy the selector named: the row on UID 9 leaves
    // archive, the row on UID 1 stays, and the queued server op names UID 9.
    let mut conn = slice.connect().await;
    tokio::time::timeout(
        DEADLINE,
        conn.call(
            "message.move",
            json!({
                "account": fixture::ACCOUNT,
                "selector": listed[1],
                "destination": "inbox",
                "settle": false,
            }),
        ),
    )
    .await
    .expect("message.move within the deadline")
    .unwrap_or_else(|e| panic!("message.move failed: {e:?}"));
    let shared = format!("<{}>", fixture::SHARED);
    let archived: Vec<i64> = slice
        .rows()
        .into_iter()
        .filter(|(mailbox, _, id)| mailbox == "archive" && *id == shared)
        .map(|(_, uid, _)| uid)
        .collect();
    assert_eq!(archived, [1], "the copy on UID 9 moved");
    let store = fixture::store(slice.tmp.path(), fixture::ACCOUNT);
    let owed = pending_ops::queued_ops(&store, fixture::ACCOUNT).expect("read the queue");
    assert!(
        matches!(
            &owed[..],
            [op] if matches!(
                &op.op,
                mailypoppins::ops::ServerOp::Move { copy: Some(copy), .. } if copy.uid == 9
            )
        ),
        "{owed:?}"
    );
}

/// A draft is not a received message, so a drafts selector is refused rather
/// than moved.
#[test]
fn a_drafts_selector_is_refused() {
    let slice = Slice::start();
    let draft = fixture::draft_selector(fixture::ACCOUNT, fixture::VALID_DRAFT);
    let out = slice.routed(&["move", &draft, "inbox"]);
    assert_refused(
        &out,
        &format!("{draft} names the reserved mailbox `drafts`, which holds local drafts"),
    );
}

/// With nothing listening and auto-start off, the command has no in-process
/// path to fall back on.
#[test]
fn mp_move_needs_a_daemon() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());
    let out = mp_no_daemon(&["move", &kickoff_in_inbox(), "archive"], tmp.path());
    assert_eq!(out.status.code(), Some(EXIT_UNAVAILABLE), "{out:?}");
}

/// The row move itself, through the method `mp move` calls: the row lands in
/// the destination, resolved from a label, and the answer carries the new
/// selector, which is what `mp move` hands back.
#[tokio::test]
async fn message_move_lands_the_row_and_answers_the_new_selector() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let moved: Value = tokio::time::timeout(
        DEADLINE,
        conn.call(
            "message.move",
            json!({
                "account": fixture::ACCOUNT,
                "selector": kickoff_in_inbox(),
                "destination": "Archive",
                "settle": false,
            }),
        ),
    )
    .await
    .expect("message.move within the deadline")
    .unwrap_or_else(|e| panic!("message.move failed: {e:?}"));

    // `moved_to.selector` carries the stored Message-ID, angle brackets
    // percent-encoded, as `message.archive` does (docs/daemon-protocol.md).
    let now = fixture::message_selector(fixture::ACCOUNT, "archive", &format!("%3C{KICKOFF}%3E"));
    assert_eq!(moved["selector"], json!(kickoff_in_inbox()));
    assert_eq!(moved["moved_to"]["mailbox"], json!("archive"));
    assert_eq!(moved["moved_to"]["selector"], json!(now));

    // The store keeps the Message-ID with its angle brackets.
    let stored = format!("<{KICKOFF}>");
    let mailboxes_of_kickoff: Vec<String> = slice
        .rows()
        .into_iter()
        .filter(|(_, _, id)| *id == stored)
        .map(|(mailbox, _, _)| mailbox)
        .collect();
    assert_eq!(mailboxes_of_kickoff, ["archive"], "the row moved, once");

    // `settle: false` commits the server move for the drain, `queued`, not
    // `settling` for a caller that never runs it (PERSO-236): a drain leaves
    // a `settling` row alone, so the move would never reach the server.
    let store = fixture::store(slice.tmp.path(), fixture::ACCOUNT);
    let owed = pending_ops::queued_ops(&store, fixture::ACCOUNT).expect("read the queue");
    assert_eq!(
        owed.iter().map(|op| op.kind.as_str()).collect::<Vec<_>>(),
        ["move"],
        "the move is the drain's: {owed:?}"
    );
    drop(store);

    // The selector `mp move` hands back resolves, so a caller can chain
    // `mp show` or `mp reply` on it.
    let shown = slice.routed(&["show", &now]);
    assert_eq!(shown.status.code(), Some(0), "{shown:?}");

    // And a second move of the same message out of its new mailbox resolves
    // the moved row, then refuses on the missing credentials, not the address.
    let out = slice.routed(&["move", &now, "inbox"]);
    assert_refused(&out, fixture::CREDENTIALS_REFUSAL);
}
