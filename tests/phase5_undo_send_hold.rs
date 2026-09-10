//! The undo-send-hold criterion of the Phase 5 parity gate (P5-U9, #0124).
//!
//! The fifth oracle plan section 3.7 names, in its own words: *"quitting the
//! last client mid-hold leaves the draft approved and sends nothing"*. It is
//! separate from `tests/phase5_parity_gate.rs` because it is the one oracle
//! that is not a comparison: there is nothing in the `pre-daemon` binary to
//! compare against, since the pre-daemon hold lived in a TUI process that no
//! test can quit, and the property is about what is *not* on the wire.
//!
//! # Where the hold lives, and why the wording is pinned anyway
//!
//! At Phase 5 the hold is client-side: `src/tui/actions.rs` parks the send in
//! `app.held_send` for `email.send_hold_secs` (default 20), the pre-draw loop
//! in `src/tui/mod.rs` fires it when the window elapses, and `u` cancels it.
//! Quitting the TUI mid-hold therefore means the send request is never made,
//! and the criterion becomes a statement about the daemon: an approved draft
//! whose client vanished must stay approved and must not be sent by anyone.
//!
//! P6-U2 moves the hold into the daemon's send scheduler and P6-U3 makes the
//! daemon cancel it when the last client exits, *"leaving the draft
//! approved"*. That is the same sentence, and the rows below are written so
//! they keep meaning the same thing after the move: they approve over a real
//! connection, drop it the way a `q` does, wait past the configured window, and
//! then ask a fresh connection what the daemon did. Today the answer is "it
//! never had a hold to fire"; after P6-U2 it will be "it had one and cancelled
//! it". Neither answer may include a sent message.
//!
//! # What makes the negative assertions non-vacuous
//!
//! Three things, and a fourth row that proves them:
//!
//! - the fake transport ([`send_fixture::FAKE_TRANSPORT_ENV`]) is armed on the
//!   daemon, so a send that happened would leave a line in a ledger file;
//! - the outbox is seeded with nothing, so a durable row that appeared is the
//!   send's;
//! - `email.send_hold_secs` is set to a real, non-zero window and asserted to
//!   be so, because a zero window is the config value that makes "mid-hold"
//!   meaningless;
//! - [`the_same_fixture_records_a_send_when_a_client_really_sends`] runs a send
//!   through the same daemon and the same ledger and finds all three.
//!
//! # Not `#[ignore]`
//!
//! The plan's fallback was to ignore this row if the hold could not be driven
//! headless. It can: everything above happens over the socket, with no
//! terminal and no pty, so the row runs in `cargo test --workspace` like any
//! other. What it cannot do headlessly is press `u` - the cancel key is
//! `KeyAction::Manual` territory, hand-dispatched in the TUI - and that key is
//! covered by the manual checklist of `tests/phase5_parity_gate.rs` instead.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use mp_client::{ClientInfo, ClientKind, Connection, Identity};

use support::parity::{socket_path, DaemonFixture};
use support::send_fixture as fixture;

/// The hold window this fixture configures, in seconds.
///
/// Short enough that a test can wait past it, long enough that "the client quit
/// while the window was open" is true of a connection dropped immediately after
/// the approve.
const HOLD_SECS: u64 = 2;

/// How long a row waits after the window before asking what the daemon did:
/// the whole window again, so a hold that fired late is still caught.
fn past_the_window() -> Duration {
    Duration::from_secs(HOLD_SECS * 2 + 1)
}

/// Seed the send fixture under `root` and give it a real undo-send window.
///
/// The `[email]` table is prepended rather than appended: `config.toml` is a
/// list of `[[accounts]]` array tables, and a top-level table written after one
/// of those belongs to the last account instead of to the document.
fn seed(root: &Path) {
    fixture::seed(root);
    let path = root.join("config.toml");
    let existing = std::fs::read_to_string(&path).expect("the fixture wrote a config.toml");
    std::fs::write(
        &path,
        format!("[email]\nsend_hold_secs = {HOLD_SECS}\n\n{existing}"),
    )
    .expect("write config.toml");
}

/// The configured window, read back the way the TUI reads it.
fn configured_hold(root: &Path) -> u64 {
    let text = std::fs::read_to_string(root.join("config.toml")).expect("read config.toml");
    let config: mailypoppins::config::GlobalConfig =
        toml::from_str(&text).expect("the fixture's config.toml parses");
    config.email.send_hold_secs
}

/// A client of the kind the TUI is: handshaken, bootstrapped, subscribed.
async fn tui_client(root: &Path) -> Connection {
    let mut conn = Connection::connect(&socket_path(root))
        .await
        .expect("connecting to a live daemon socket succeeds");
    conn.initialize(
        ClientInfo {
            kind: ClientKind::Tui,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        Identity {
            data_dir: root.to_path_buf(),
            config_dir: root.to_path_buf(),
        },
        &[],
        &[],
    )
    .await
    .expect("a compatible handshake succeeds");
    conn.call("state.bootstrap", json!({}))
        .await
        .expect("a bootstrap registers this connection for events");
    conn
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

/// The status `draft.list` reports for `id`, or `None` if the draft is gone.
async fn draft_status(conn: &mut Connection, account: &str, id: &str) -> Option<String> {
    let listing = conn
        .call("draft.list", json!({ "account": account }))
        .await
        .expect("draft.list answers");
    listing
        .get("drafts")
        .and_then(Value::as_array)
        .expect("a draft listing carries drafts")
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
        .map(|entry| {
            entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("<no status>")
                .to_string()
        })
}

/// Every unfinished outbox row the daemon holds for `account`.
async fn outbox_rows(conn: &mut Connection, account: &str) -> Vec<Value> {
    let listing = conn
        .call("send.outbox_list", json!({ "account": account }))
        .await
        .expect("send.outbox_list answers");
    listing
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The ids of every unfinished outbox row, which is what a new row moves.
async fn outbox_ids(conn: &mut Connection, account: &str) -> Vec<i64> {
    ids(&outbox_rows(conn, account).await)
}

fn ids(rows: &[Value]) -> Vec<i64> {
    rows.iter()
        .filter_map(|row| row.get("id").and_then(Value::as_i64))
        .collect()
}

/// The canonical path of a draft, as `draft.path` resolves it.
async fn draft_path(conn: &mut Connection, account: &str, id: &str) -> Option<std::path::PathBuf> {
    let located = conn
        .call("draft.path", json!({ "account": account, "id": id }))
        .await
        .ok()?;
    located
        .get("path")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
}

// ---------------------------------------------------------------------------
// The criterion
// ---------------------------------------------------------------------------

/// Quitting the last client mid-hold leaves the draft approved and sends
/// nothing.
#[test]
fn quitting_the_last_client_mid_hold_leaves_the_draft_approved_and_sends_nothing() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root);
    assert_eq!(
        configured_hold(root),
        HOLD_SECS,
        "a zero window would make `mid-hold` meaningless, so the fixture's window is asserted \
         before anything is held"
    );

    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    // The approve the TUI's `cA` / `x` does, over a real connection, followed
    // immediately by the client going away: that is the `q` the criterion is
    // about, taken while the hold window is open.
    //
    // The fixture seeds four outbox rows in the states `mp outbox list`
    // renders, so the assertion afterwards is "no row appeared" rather than
    // "the outbox is empty": an empty-outbox assertion would be false about the
    // fixture rather than about the send.
    let before = block_on(async {
        let mut conn = tui_client(root).await;
        let before = outbox_ids(&mut conn, fixture::ACCOUNT).await;
        let approved = conn
            .call(
                "draft.approve",
                json!({ "account": fixture::ACCOUNT, "id": fixture::VALID }),
            )
            .await
            .expect("draft.approve answers");
        assert_eq!(
            approved.get("status").and_then(Value::as_str),
            Some("approved"),
            "the draft is approved before the client quits: {approved}"
        );
        drop(conn);
        before
    });

    // Past the window, with nobody connected. A daemon that owned the hold and
    // fired it anyway (the failure P6-U3 must not introduce) has had twice the
    // configured window to do so.
    std::thread::sleep(past_the_window());

    let (status, rows, path) = block_on(async {
        let mut conn = tui_client(root).await;
        let status = draft_status(&mut conn, fixture::ACCOUNT, fixture::VALID).await;
        let rows = outbox_rows(&mut conn, fixture::ACCOUNT).await;
        let path = draft_path(&mut conn, fixture::ACCOUNT, fixture::VALID).await;
        (status, rows, path)
    });

    assert_eq!(
        status.as_deref(),
        Some("approved"),
        "the draft the client approved is still approved after the client quit mid-hold"
    );
    let path = path.expect("`draft.path` still resolves the draft the client approved");
    assert!(
        path.exists(),
        "and its file is still on disk at {}: a draft `draft::settle_sent_draft` retired is a sent \
         one",
        path.display()
    );
    assert_eq!(
        ids(&rows),
        before,
        "nothing was queued for delivery: the outbox holds a row the client's approve did not \
         find there\n{rows:?}"
    );
    let events = fixture::transport_events(&log);
    assert!(
        events.is_empty(),
        "nothing reached the transport, and the fake one recorded {} event(s): {events:?}",
        events.len()
    );

    daemon.stop();
}

/// The same fixture, the same daemon and the same ledger, with a client that
/// really sends: the proof that the three negative assertions above can fail.
#[test]
fn the_same_fixture_records_a_send_when_a_client_really_sends() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root);

    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    let selector = fixture::selector(fixture::ACCOUNT, fixture::APPROVED);
    let out = daemon.mp_routed(&["send", &selector, "-y"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the send succeeds\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let events = fixture::transport_events(&log);
    assert!(
        !events.is_empty(),
        "a real send leaves lines in the fake transport's ledger at {}, which is what makes the \
         empty ledger above an assertion rather than a fact about the fixture",
        log.display()
    );
    let retired = fixture::drafts_dir(root, fixture::ACCOUNT).join(fixture::APPROVED_FILE);
    assert!(
        !retired.exists(),
        "and a fully delivered draft is retired, file and all: {} is still there, so the \
         still-on-disk assertion above would hold for a sent draft too",
        retired.display()
    );

    daemon.stop();
}
