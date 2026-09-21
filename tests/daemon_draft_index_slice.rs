//! `DFT-12`: the drafts index, once the client stops keeping one (#0126, plan
//! unit P5-U10d).
//!
//! This file is the **(a) half** of a T unit: the surface already exists, and
//! what is written down here is the pin that says so, so the implementer
//! deletes the client-side poll rather than contracting a method for it.
//!
//! # What the client does today, and what answers it
//!
//! `src/tui/mod.rs` runs a one-second `store::drafts::fingerprint` scan of the
//! *active* account's drafts directory and, on a change, calls
//! `store::drafts::refresh_account` before reloading the list;
//! `src/tui/actions.rs` and `src/tui/commands.rs` call the same refresh after
//! an editor session and after a recipient edit. Those four calls are the
//! `mod.rs store` row of `tests/fixtures/tui-engine-imports.txt` and two rows
//! of the path guard beside it.
//!
//! Three served surfaces answer all of it, and no new method is needed:
//!
//! 1. **The daemon's own watcher** (P3b-U10) polls every configured account's
//!    drafts directory, debounced, and publishes `draft.changed`,
//!    `draft.invalid` and `state.remove` of `draft:<account>/<id>`. That is
//!    the poll, moved, and `tests/daemon_draft_watch.rs` pins it over the
//!    socket: `a_direct_write_reaches_a_subscribed_client_as_draft_changed`,
//!    `an_atomic_save_reaches_a_subscribed_client_as_one_draft_changed`,
//!    `rapid_saves_reach_a_client_as_one_event_carrying_the_last_write`,
//!    `deleting_a_draft_reaches_a_client_as_a_remove_of_its_resource` and
//!    `an_unparseable_draft_reaches_a_client_as_draft_invalid_with_a_line`.
//! 2. **`draft.list`** answers from a fresh directory scan rather than from a
//!    store index, so a client that reloads its Drafts mailbox owes no refresh
//!    first. `tests/daemon_draft_slice.rs`'s
//!    `the_mutators_see_a_draft_that_was_created_a_moment_ago` pins the same
//!    rule for `draft.path` and `draft.approve`; the row below is the listing's.
//! 3. **The bootstrap snapshot's `drafts` section**, which carries the same
//!    rows for every account.
//!
//! So this file adds exactly the two facts the three above do not state, and
//! both **pass at HEAD**, deliberately: an (a) decision's product is the pin
//! that the surface is there, and the failing half of the unit is the guard
//! that still names the call sites.
//!
//! # What does not survive the move, and is not a loss
//!
//! The *store's* drafts index. `store::drafts::refresh_account` writes a
//! `drafts` table the daemon reads for one thing only, the sidebar's Drafts
//! count, and it refreshes that itself on every `mailbox.list`. A client that
//! stopped refreshing it therefore changes nothing anybody reads, which is why
//! the four call sites go without a method to replace them.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::draft::DraftListing;
use mp_protocol::events::KIND_DRAFT_CHANGED;
use mp_protocol::state::Bootstrap;
use mp_protocol::{EventEnvelope, METHOD_STATE_EVENT};

use support::draft_fixture as fixture;
use support::parity::{socket_path, DaemonFixture};

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// Upper bound on waiting for a watcher to notice a write: the default poll is
/// one second and the default debounce 300 ms, so this is roughly six windows.
const ANNOUNCE_DEADLINE: Duration = Duration::from_secs(8);

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
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary draft-index root");
        fixture::seed(tmp.path());
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

/// Bootstrap, which is also what subscribes this connection to the event
/// stream.
async fn subscribe(conn: &mut Connection) -> Bootstrap {
    let answer = call(conn, "state.bootstrap", json!({})).await;
    serde_json::from_value(answer).expect("the bootstrap decodes")
}

/// The next `state.event` of `kind`, or a failure after
/// [`ANNOUNCE_DEADLINE`].
async fn next_event_of_kind(conn: &mut Connection, kind: &str) -> EventEnvelope {
    let deadline = tokio::time::Instant::now() + ANNOUNCE_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "no {kind} event arrived within {ANNOUNCE_DEADLINE:?}"
        );
        let notification = tokio::time::timeout(remaining, conn.next_notification())
            .await
            .unwrap_or_else(|_| panic!("no {kind} event arrived within {ANNOUNCE_DEADLINE:?}"))
            .expect("the daemon kept the connection open");
        if notification.method != METHOD_STATE_EVENT {
            continue;
        }
        let envelope: EventEnvelope =
            serde_json::from_value(notification.params).expect("a state.event decodes");
        if envelope.kind == kind {
            return envelope;
        }
    }
}

// ---------------------------------------------------------------------------
// 1. The listing needs no refresh first
// ---------------------------------------------------------------------------

/// A draft another process wrote a millisecond ago is in the listing, with no
/// index refresh asked for and no watcher poll waited on.
///
/// This is what lets the three `store::drafts::refresh_account` calls in
/// `src/tui/` go: they exist because the client's own listing read the store's
/// index, and `draft.list` reads the directory.
///
/// **Passes at HEAD**, deliberately: it is the pin that the surface is already
/// there.
#[tokio::test]
async fn a_draft_written_a_moment_ago_is_listed_without_a_refresh() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let before: DraftListing = serde_json::from_value(
        call(
            &mut conn,
            "draft.list",
            json!({"account": fixture::ACCOUNT, "status": null}),
        )
        .await,
    )
    .expect("the listing decodes");

    let id = "a0000000000000ff";
    let path = fixture::drafts_dir(slice.root(), fixture::ACCOUNT).join("von-aussen.md");
    fs::write(
        &path,
        fixture::document(id, "robin@example.com", "Von aussen", "draft", "Body.\n"),
    )
    .expect("write the draft another process made");

    let after: DraftListing = serde_json::from_value(
        call(
            &mut conn,
            "draft.list",
            json!({"account": fixture::ACCOUNT, "status": null}),
        )
        .await,
    )
    .expect("the listing decodes");

    assert_eq!(
        after.drafts.len(),
        before.drafts.len() + 1,
        "the listing is a directory scan, so the new file is in the very next answer"
    );
    let row = after
        .drafts
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("the listing carries {id}, got {:?}", after.drafts));
    assert_eq!(row.path, path.display().to_string());
    assert_eq!(row.subject.as_deref(), Some("Von aussen"));

    // And the sidebar count the same listing labels moved with it, which is
    // the other half of what the client's refresh used to keep in step.
    let mailboxes = call(
        &mut conn,
        "mailbox.list",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    let drafts = mailboxes["mailboxes"]
        .as_array()
        .expect("mailbox rows")
        .iter()
        .find(|row| row["role"] == json!("drafts"))
        .expect("the account lists a Drafts mailbox")
        .clone();
    assert_eq!(
        drafts["total"].as_u64(),
        Some((after.drafts.len() + after.skipped.len()) as u64),
        "the count and the list it labels come from the same scan, error rows included (#0080)"
    );
}

// ---------------------------------------------------------------------------
// 2. Every account is watched, not just the one on screen
// ---------------------------------------------------------------------------

/// A draft written into an account the client is not looking at is announced
/// too.
///
/// The client's poll scanned one directory, the active account's, and adopted
/// another account's state silently on a switch; the daemon's watcher has a
/// root per configured account. So a client that drops the poll gains a fact
/// rather than losing one, and that is worth pinning before the poll goes.
///
/// **Passes at HEAD**, deliberately.
#[tokio::test]
async fn a_draft_in_another_account_is_announced_as_well() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let bootstrap = subscribe(&mut conn).await;
    assert!(
        bootstrap
            .snapshot
            .drafts_of(fixture::OTHER_ACCOUNT)
            .iter()
            .all(|row| row.id != "b0000000000000ff"),
        "the draft this row writes is not in the snapshot yet"
    );

    let id = "b0000000000000ff";
    let path = fixture::drafts_dir(slice.root(), fixture::OTHER_ACCOUNT).join("nebenan.md");
    fs::write(
        &path,
        fixture::document(id, "robin@example.com", "Nebenan", "draft", "Body.\n"),
    )
    .expect("write a draft for the account nobody is looking at");

    loop {
        let event = next_event_of_kind(&mut conn, KIND_DRAFT_CHANGED).await;
        if event.payload["id"] == json!(id) {
            assert_eq!(
                event.payload["account"],
                json!(fixture::OTHER_ACCOUNT),
                "the event names the account whose directory moved"
            );
            assert_eq!(event.payload["path"], json!(path.display().to_string()));
            assert_eq!(event.payload["subject"], json!("Nebenan"));
            break;
        }
    }
}
