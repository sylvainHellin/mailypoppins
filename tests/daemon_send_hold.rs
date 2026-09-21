//! The daemon-owned undo-send hold, over a socket (P6-U1, ticket #0125).
//!
//! The half of the contract that only two live connections can express. The
//! in-process half is `src/tui/hold_tests.rs`: the wire vocabulary, the status
//! line, the `u` key and the residue. Here the daemon is a real `mp daemon
//! run` over a seeded root, the transport is the fake one whose ledger records
//! every submission, and the clients are two `mp_client::Connection`s that
//! handshake and bootstrap like the TUI does.
//!
//! # What this file pins
//!
//! - One countdown, seen by everyone: a hold armed by client **A** reaches
//!   client **B** as a `send.hold_started` event, and `send.hold_status`
//!   answers B the same object.
//! - Cancellation from a window that did not send: **B** calls
//!   `send.cancel_hold` on A's operation id, nothing reaches the transport,
//!   and the draft stays `approved` with its file on disk.
//! - A hold nobody cancels fires: the ledger gets one submission per
//!   recipient of the draft and the draft is retired, file and all.
//! - A send outlives the client that asked for it: **A** disconnects mid-hold
//!   while **B** watches, and the hold still fires. A durable operation is not
//!   cancelled by a closing window, which is what `send.*`'s
//!   `CancelScope::Durable` has meant since P4-U12.
//! - `send_hold_secs = 0` is the opt-out: `hold: true` against a zero window
//!   sends at once, publishes no `send.hold_*` event, and leaves
//!   `send.hold_status` empty.
//! - The CLI still bypasses the hold: `mp send-approved -y` against a
//!   configured twenty-second window returns in well under it with the
//!   submission already in the ledger, and prints the line the pre-daemon
//!   binary printed (`send_fixture::approved_summary`, which is
//!   `mp_client::format::send_approved_summary`'s wording and the oracle's).
//!
//! # What this file deliberately does not pin
//!
//! **The last client exiting mid-hold.** That is `tests/phase5_undo_send_hold.rs`,
//! the fifth oracle of the Phase 5 parity gate, which already asserts it from
//! the outside: approve, drop the connection, wait past the window, and find
//! the draft approved, the outbox untouched and the ledger empty. Duplicating
//! it here would give two files one sentence to keep true. The plan's rule -
//! *"When the last client exits mid-hold the daemon cancels the hold and
//! leaves the draft approved"* - is P6-U3/P6-U4's to implement; what P6-U2
//! owes is that the *sender* disconnecting while another client watches does
//! **not** cancel, which is the fourth row above and the opposite failure.
//!
//! # Timing
//!
//! [`HOLD_SECS`] is two seconds and every wait is twice that plus one, the
//! same arithmetic `tests/phase5_undo_send_hold.rs` uses: long enough that
//! "mid-hold" is a real interval on a loaded machine, short enough that the
//! file costs seconds rather than minutes. No row polls a clock to decide what
//! the daemon did; the fired rows sleep past the window and then read the
//! ledger, and the event rows block on `next_notification` with a deadline.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use mp_client::{ClientInfo, ClientKind, Connection, Identity};

use mailypoppins::daemon::dispatch::MethodKind;
use mailypoppins::daemon::methods::send::SEND_METHOD_SPECS;

use support::parity::{socket_path, DaemonFixture};
use support::send_fixture as fixture;

/// The hold window every fixture configures, in seconds.
const HOLD_SECS: u64 = 2;

/// The window the CLI rows configure, which is `email.send_hold_secs`'s own
/// default: a `mp send-approved` that waited it out would take twenty seconds
/// and this file would notice.
const CLI_HOLD_SECS: u64 = 20;

/// The ceiling a CLI send must come in under to have bypassed the hold.
///
/// Generous against a loaded machine and still nowhere near
/// [`CLI_HOLD_SECS`]: the question is "did it wait out a window", not "how
/// fast is SMTP".
const BYPASS_CEILING: Duration = Duration::from_secs(10);

/// How long an event row waits for a notification before failing.
const EVENT_DEADLINE: Duration = Duration::from_secs(10);

/// How long a fired row waits after the window before asking what the daemon
/// did: the whole window again, so a hold that fired late is still caught.
fn past_the_window() -> Duration {
    Duration::from_secs(HOLD_SECS * 2 + 1)
}

/// The recipients one send of `fixture::APPROVED` submits to, sorted.
///
/// The `to:` the draft fixture writes (`ivana@example.com`) and the `cc:`
/// `send_fixture`'s `widen_approved_draft` adds (`fixture::REJECTED`).
const APPROVED_RECIPIENTS: [&str; 2] = [fixture::REJECTED, "ivana@example.com"];

/// The addresses the transport was handed under the draft's own Message-ID,
/// sorted.
///
/// Counted by Message-ID rather than by the ledger's length, the way
/// `tests/daemon_send_slice.rs` counts a send's Sent copy: one send of this
/// draft is four ledger lines and never one. The fake transport writes one
/// `Submit` per recipient and the widened draft has two; the send files its
/// own Sent copy; and it drains the account's outbox on its way out, so the
/// seeded row parked on its APPEND (`fixture::APPENDING_ROW`) files its copy
/// in the same run. "Sent exactly once" is therefore
/// [`APPROVED_RECIPIENTS`] with nothing repeated.
fn draft_submissions(events: &[fixture::TransportEvent]) -> Vec<String> {
    let Some(mid) = events.iter().find_map(|event| match event {
        fixture::TransportEvent::Submit { message_id, .. } => Some(message_id.clone()),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut addresses: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            fixture::TransportEvent::Submit {
                message_id,
                address,
                ..
            } if *message_id == mid => Some(address.clone()),
            _ => None,
        })
        .collect();
    addresses.sort();
    addresses
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// Seed the send fixture under `root` with an undo-send window of `secs`.
///
/// The `[email]` table is prepended rather than appended, for the reason
/// `tests/phase5_undo_send_hold.rs` records: `config.toml` is a list of
/// `[[accounts]]` array tables, and a top-level table written after one of
/// those belongs to the last account instead of to the document.
fn seed(root: &Path, secs: u64) {
    fixture::seed(root);
    let path = root.join("config.toml");
    let existing = std::fs::read_to_string(&path).expect("the fixture wrote a config.toml");
    std::fs::write(
        &path,
        format!("[email]\nsend_hold_secs = {secs}\n\n{existing}"),
    )
    .expect("write config.toml");
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

/// Read notifications until one of `kind` arrives, and answer its payload.
async fn await_kind(conn: &mut Connection, kind: &str) -> Value {
    let deadline = Instant::now() + EVENT_DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no {kind} event arrived within {EVENT_DEADLINE:?}"
        );
        let notification = tokio::time::timeout(EVENT_DEADLINE, conn.next_notification())
            .await
            .unwrap_or_else(|_| panic!("no {kind} event arrived within {EVENT_DEADLINE:?}"))
            .expect("the daemon keeps the connection open");
        if notification.params["kind"] == kind {
            return notification.params["payload"].clone();
        }
    }
}

/// Every `send.hold_*` kind this connection has been told about so far,
/// without blocking for one that has not arrived.
async fn hold_kinds_so_far(conn: &mut Connection) -> Vec<String> {
    let mut kinds = Vec::new();
    while let Ok(Some(notification)) =
        tokio::time::timeout(Duration::from_millis(200), conn.next_notification()).await
    {
        if let Some(kind) = notification.params["kind"].as_str() {
            if kind.starts_with("send.hold_") {
                kinds.push(kind.to_string());
            }
        }
    }
    kinds
}

/// The holds the daemon is carrying for `account`.
async fn holds(conn: &mut Connection, account: &str) -> Vec<Value> {
    let listing = conn
        .call("send.hold_status", json!({ "account": account }))
        .await
        .expect("send.hold_status answers");
    listing
        .get("holds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The status `draft.list` reports for `id`, or `None` when the draft is gone.
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

/// Arm a hold for the approved fixture draft and answer its operation id.
async fn send_held(conn: &mut Connection, account: &str, id: &str) -> String {
    let answer = conn
        .call(
            "send.draft",
            json!({"account": account, "id": id, "hold": true}),
        )
        .await
        .expect("send.draft accepts a hold");
    assert_eq!(
        answer.get("held").and_then(Value::as_bool),
        Some(true),
        "a send that armed a hold says so in its own answer: {answer}"
    );
    answer
        .get("operation_id")
        .and_then(Value::as_str)
        .expect("a send answers with an operation id")
        .to_string()
}

// ---------------------------------------------------------------------------
// 1. The two methods exist
// ---------------------------------------------------------------------------

/// The send family serves the hold's query and its command.
///
/// Read off the declaration rather than probed over the wire, the way
/// `tests/daemon_send_slice.rs` reads the same array: a method missing from it
/// is never registered, so the array is where the fact lives.
#[test]
fn the_send_family_serves_the_hold() {
    let names: Vec<&str> = SEND_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert!(
        names.contains(&"send.hold_status") && names.contains(&"send.cancel_hold"),
        "the hold's query and its cancel are part of the send family: {names:?}"
    );
    let kind = |name: &str| {
        SEND_METHOD_SPECS
            .iter()
            .find(|spec| spec.name == name)
            .unwrap_or_else(|| panic!("{name} is declared"))
            .kind
    };
    assert_eq!(kind("send.hold_status"), MethodKind::Query);
    assert_eq!(kind("send.cancel_hold"), MethodKind::Command);

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is still in method-name order");
}

// ---------------------------------------------------------------------------
// 2. One countdown, two clients, either may cancel
// ---------------------------------------------------------------------------

/// A sends with a hold, B sees the countdown and cancels it: nothing is sent
/// and the draft stays approved.
///
/// The row the plan asks for in its own words - *"cancellation from a client
/// other than the one that sent must work"* - and the one that makes the hold
/// daemon-owned rather than merely daemon-hosted. B never saw the send; all it
/// has is the operation id the event carried, which is why
/// `send.cancel_hold`'s only parameter is that id.
#[test]
fn a_second_client_sees_the_countdown_and_cancels_the_send() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    block_on(async {
        let mut a = tui_client(root).await;
        let mut b = tui_client(root).await;

        let operation = send_held(&mut a, fixture::ACCOUNT, fixture::APPROVED).await;

        // B is told about a send it did not make, with everything it needs to
        // render the countdown and to cancel it.
        let started = await_kind(&mut b, "send.hold_started").await;
        assert_eq!(
            started["operation_id"].as_str(),
            Some(operation.as_str()),
            "the event names the operation the send answered with: {started}"
        );
        assert_eq!(started["account"], fixture::ACCOUNT);
        assert_eq!(
            started["hold_secs"].as_u64(),
            Some(HOLD_SECS),
            "and the window the daemon read from `email.send_hold_secs`"
        );
        assert_eq!(
            started["origin"].as_str(),
            Some("tui"),
            "the payload says which kind of client asked"
        );

        // The same object, to a client that joined too late for the event.
        let listed = holds(&mut b, fixture::ACCOUNT).await;
        assert_eq!(listed.len(), 1, "one hold is pending: {listed:?}");
        assert_eq!(listed[0]["operation_id"].as_str(), Some(operation.as_str()));
        assert!(
            listed[0]["remaining_secs"].as_u64().unwrap_or(0) <= HOLD_SECS,
            "the remainder is what is left of the window, not the window: {}",
            listed[0]
        );
        assert!(
            listed[0]["fires_at"]
                .as_str()
                .is_some_and(|at| at.len() >= 20),
            "and a deadline a late client can render: {}",
            listed[0]
        );

        // The countdown itself, on the wire and not only in the table: B
        // renders `Sending in {n}s` off these, so a scheduler that armed the
        // hold and published no tick would leave every client's countdown
        // frozen at the window it started with. The wait is bounded by
        // `await_kind`, because a fixed sleep would either race the tick or
        // sit out the window it is trying to observe.
        let tick = await_kind(&mut b, "send.hold_tick").await;
        assert_eq!(
            tick["operation_id"].as_str(),
            Some(operation.as_str()),
            "the tick is about the hold that started: {tick}"
        );
        assert_eq!(
            tick["hold_secs"].as_u64(),
            started["hold_secs"].as_u64(),
            "and carries the same window it was armed with: {tick}"
        );
        let left = tick["remaining_secs"]
            .as_u64()
            .unwrap_or_else(|| panic!("a tick carries a remainder: {tick}"));
        assert!(
            left > 0 && left < HOLD_SECS,
            "a tick is the window counting down, so its remainder is below the {HOLD_SECS}s it \
             was armed with and above the zero an ended hold reports: {tick}"
        );

        let cancelled = b
            .call("send.cancel_hold", json!({ "operation_id": operation }))
            .await
            .expect("a client that did not send may cancel");
        assert_eq!(
            cancelled.get("cancelled").and_then(Value::as_bool),
            Some(true),
            "the cancel says it cancelled: {cancelled}"
        );

        // And A, whose window is the one showing the countdown, is told.
        let ended = await_kind(&mut a, "send.hold_cancelled").await;
        assert_eq!(ended["operation_id"].as_str(), Some(operation.as_str()));
        assert_eq!(
            ended["remaining_secs"].as_u64(),
            Some(0),
            "a hold that ended has nothing left of its window: {ended}"
        );
    });

    // Past the window, so a cancel that merely stopped the countdown and not
    // the send has had twice as long as it needed to betray itself.
    std::thread::sleep(past_the_window());

    let (status, pending) = block_on(async {
        let mut conn = tui_client(root).await;
        (
            draft_status(&mut conn, fixture::ACCOUNT, fixture::APPROVED).await,
            holds(&mut conn, fixture::ACCOUNT).await,
        )
    });

    assert_eq!(
        status.as_deref(),
        Some("approved"),
        "a cancelled send leaves the draft exactly as the approve left it"
    );
    assert!(
        pending.is_empty(),
        "and the daemon is carrying no hold for it: {pending:?}"
    );
    let events = fixture::transport_events(&log);
    assert!(
        events.is_empty(),
        "nothing reached the transport, and the fake one recorded {} event(s): {events:?}",
        events.len()
    );
    let file = fixture::drafts_dir(root, fixture::ACCOUNT).join(fixture::APPROVED_FILE);
    assert!(
        file.exists(),
        "and its file is still on disk at {}: a draft `settle_sent_draft` retired is a sent one",
        file.display()
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 3. A hold nobody cancels
// ---------------------------------------------------------------------------

/// Nobody presses `u`, so the window elapses and the draft goes.
///
/// The positive half of every negative assertion in this file: the same
/// fixture, the same ledger and the same draft, with nothing cancelling.
#[test]
fn a_hold_nobody_cancels_fires_and_retires_the_draft() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    let operation = block_on(async {
        let mut a = tui_client(root).await;
        let operation = send_held(&mut a, fixture::ACCOUNT, fixture::APPROVED).await;
        let fired = await_kind(&mut a, "send.hold_fired").await;
        assert_eq!(
            fired["operation_id"].as_str(),
            Some(operation.as_str()),
            "the hold that fired is the one that was armed: {fired}"
        );
        operation
    });

    std::thread::sleep(past_the_window());

    let (status, pending) = block_on(async {
        let mut conn = tui_client(root).await;
        (
            draft_status(&mut conn, fixture::ACCOUNT, fixture::APPROVED).await,
            holds(&mut conn, fixture::ACCOUNT).await,
        )
    });

    let events = fixture::transport_events(&log);
    assert_eq!(
        draft_submissions(&events),
        APPROVED_RECIPIENTS,
        "exactly one submission per recipient reached the transport for {operation}: {events:?}"
    );
    assert_eq!(
        status, None,
        "a fully delivered draft is retired, so `draft.list` no longer lists it"
    );
    let file = fixture::drafts_dir(root, fixture::ACCOUNT).join(fixture::APPROVED_FILE);
    assert!(
        !file.exists(),
        "file and all: {} is still there",
        file.display()
    );
    assert!(
        pending.is_empty(),
        "and a hold that fired is not a hold any more: {pending:?}"
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 4. The sender goes away and somebody else is still watching
// ---------------------------------------------------------------------------

/// A sends, A's window closes, B is still connected: the hold fires.
///
/// The opposite failure to the parity gate's fifth oracle, and the one that
/// would be easy to introduce while implementing it: a hold cancelled because
/// *its own* client disconnected would make `send.*`'s
/// `CancelScope::Durable` a lie and would lose a send the user confirmed. The
/// rule is about the *last* client, not about the sender, and
/// `tests/phase5_undo_send_hold.rs` owns that half.
#[test]
fn the_sender_may_close_its_window_while_another_client_watches() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    block_on(async {
        let mut b = tui_client(root).await;
        let mut a = tui_client(root).await;
        let operation = send_held(&mut a, fixture::ACCOUNT, fixture::APPROVED).await;
        // The `q` of the window that sent, with another window still open.
        drop(a);

        let fired = await_kind(&mut b, "send.hold_fired").await;
        assert_eq!(
            fired["operation_id"].as_str(),
            Some(operation.as_str()),
            "the hold survived the window that armed it: {fired}"
        );
    });

    std::thread::sleep(past_the_window());

    let events = fixture::transport_events(&log);
    assert_eq!(
        draft_submissions(&events),
        APPROVED_RECIPIENTS,
        "the send the user confirmed went out, once per recipient: {events:?}"
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 5. The zero window
// ---------------------------------------------------------------------------

/// `send_hold_secs = 0` is the opt-out: no wait, no hold, no event.
///
/// #0090 shipped the zero window as "send immediately" and `SND-04` keeps it.
/// A daemon that armed a zero-second hold would publish a countdown that
/// starts and ends in the same breath, which every client would have to render
/// and then immediately un-render.
#[test]
fn a_zero_window_sends_at_once_and_publishes_no_hold_event() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root, 0);
    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    block_on(async {
        let mut conn = tui_client(root).await;
        let answer = conn
            .call(
                "send.draft",
                json!({
                    "account": fixture::ACCOUNT,
                    "id": fixture::APPROVED,
                    "hold": true,
                }),
            )
            .await
            .expect("send.draft accepts a hold against a zero window");
        assert_ne!(
            answer.get("held").and_then(Value::as_bool),
            Some(true),
            "a zero window arms nothing, so the answer claims no hold: {answer}"
        );
        assert!(
            holds(&mut conn, fixture::ACCOUNT).await.is_empty(),
            "and there is nothing to cancel"
        );
        let kinds = hold_kinds_so_far(&mut conn).await;
        assert!(
            kinds.is_empty(),
            "a send that never held publishes no countdown: {kinds:?}"
        );
    });

    std::thread::sleep(past_the_window());
    let events = fixture::transport_events(&log);
    assert_eq!(
        draft_submissions(&events),
        APPROVED_RECIPIENTS,
        "and the message went once per recipient, which is what the opt-out is for: {events:?}"
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 6. The CLI still bypasses the hold
// ---------------------------------------------------------------------------

/// `mp send-approved -y` does not wait out a twenty-second window.
///
/// `ANO-7` records that the CLI send paths bypass the hold and `SND-04` keeps
/// it that way: a one-shot command whose whole point is to exit cannot park a
/// message behind a countdown nobody is there to cancel. The bypass is
/// structural rather than a flag - the CLI passes no `hold`, and `hold`
/// defaults to `false` - which is what makes this row a regression test for
/// the default and not for a code path.
///
/// `tests/daemon_send_slice.rs` has the `mp send` half
/// (`a_routed_send_delivers_without_waiting_out_a_hold`); this is the
/// `send-approved` half, and it checks the line as well as the clock, because
/// the summary wording is `mp_client::format::send_approved_summary`'s and is
/// what the pre-daemon binary printed.
#[test]
fn the_cli_send_approved_still_bypasses_the_hold() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root, CLI_HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    let started = Instant::now();
    let out = daemon.mp_routed(&["send-approved", "-y"]);
    let elapsed = started.elapsed();

    assert_eq!(
        out.status.code(),
        Some(0),
        "the batch send succeeds\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed < BYPASS_CEILING,
        "`mp send-approved -y` took {elapsed:?} against a {CLI_HOLD_SECS}s window, which is long \
         enough to have waited one out"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&fixture::approved_summary(fixture::ACCOUNT, 1, 0)),
        "and it printed the summary the oracle printed, not a countdown:\n{stdout}"
    );
    let events = fixture::transport_events(&log);
    assert!(
        !events.is_empty(),
        "the message was already on its way when the command exited: {events:?}"
    );
    let pending = block_on(async {
        let mut conn = tui_client(root).await;
        holds(&mut conn, fixture::ACCOUNT).await
    });
    assert!(
        pending.is_empty(),
        "and the CLI left no hold behind for a TUI to discover: {pending:?}"
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 7. Refusals
// ---------------------------------------------------------------------------

/// Cancelling a hold nobody holds is refused, and refused as a parameter
/// error.
///
/// Both cases are the same mistake made twice: an id that never named a hold,
/// and an id whose hold has already fired. `-32602` for both, because the
/// caller named something the daemon cannot act on, which is what the whole
/// send family answers for that (`tests/daemon_send_slice.rs`, `INVALID_PARAMS`).
#[test]
fn cancelling_a_hold_nobody_holds_is_refused() {
    let tmp = tempfile::tempdir().expect("a temporary hold root");
    let root = tmp.path();
    seed(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(&log))],
    );

    block_on(async {
        let mut conn = tui_client(root).await;

        let unknown = conn
            .call("send.cancel_hold", json!({"operation_id": "op-nothing"}))
            .await
            .expect_err("an id that never named a hold is refused");
        let code = match &unknown {
            mp_client::ClientError::Rpc(error) => error.code,
            other => panic!("expected a JSON-RPC refusal, got {other:?}"),
        };
        assert_eq!(code, -32602, "the caller named something that is not there");

        // The same refusal once the window has elapsed: a hold that fired is
        // a send in flight, and a cancel is not a recall.
        let operation = send_held(&mut conn, fixture::ACCOUNT, fixture::APPROVED).await;
        await_kind(&mut conn, "send.hold_fired").await;
        let late = conn
            .call("send.cancel_hold", json!({ "operation_id": operation }))
            .await
            .expect_err("a hold that already fired cannot be cancelled");
        assert!(
            matches!(&late, mp_client::ClientError::Rpc(error) if error.code == -32602),
            "and it is the same refusal: {late:?}"
        );
    });

    daemon.stop();
}
