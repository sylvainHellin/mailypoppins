//! Graceful shutdown, over a socket (P6-U3, ticket #0125).
//!
//! The plan's sentence, verbatim: *"Stops accepting commands, settles or
//! checkpoints active operations, closes watchers and connections, removes
//! **only** its own socket, releases locks. An active hold is settled or
//! cancelled, never silently dropped. When the last client exits mid-hold the
//! daemon cancels the hold and leaves the draft approved. `mp daemon stop`
//! names the operations that prevented a clean stop."*
//!
//! Every row here is a real `mp daemon run` over a real socket, because every
//! clause of that sentence is about a process ending: what its last frames
//! were, what it left on disk, what it let go of. None of it can be said in
//! process.
//!
//! # The contract this file pins
//!
//! Nothing here is a Rust name. The whole contract is on the wire and on
//! stdout, so **this file compiles against the tree as committed** and every
//! row fails at runtime, naming what is missing. The implementer (P6-U4) does
//! not edit it; they make it pass.
//!
//! ```text
//! daemon.stop {grace_secs?: u64}      -> {stopping: true, grace_secs, pending: [op…]}
//!     grace_secs absent -> DEFAULT_STOP_GRACE_SECS (10); 0 means no waiting at all.
//!     `pending` is what the registry still has live once the holds are cancelled,
//!     each entry the `operation.status` object (`snapshot.operations`' own shape),
//!     in start order.
//!
//! state.event {kind: "daemon.shutting_down",
//!              payload: {grace_secs, pending: [op…]}}   -> every bootstrapped client
//!
//! daemon.stopped {instance_id, clean: bool, unsettled: [op…]}  -> a notification on the
//!     connection that asked, as the last frame before the daemon closes it.
//!
//! -32009 shutting_down, "the daemon is shutting down"   -> every other method, including
//!     `initialize`, from the instant the stop is accepted. `daemon.status` and a repeated
//!     `daemon.stop` keep answering.
//!
//! mp daemon stop [--grace-secs N] [--timeout-secs N]
//!     clean:    "\u{2713} daemon stopped"
//!     unclean:  "\u{2717} daemon stopped, {n} operation[s] did not settle within {g}s"
//!               followed by one "  {method} ({operation_id})" per unsettled operation.
//!     Exit 0 either way: the daemon did stop, which is what was asked.
//! ```
//!
//! # The order the daemon does it in, which is what makes the rows decidable
//!
//! 1. mark shutting down - from here every other method is `-32009`;
//! 2. cancel every armed hold, publishing `send.hold_cancelled`, drafts left `approved`;
//! 3. publish `daemon.shutting_down` to every bootstrapped connection;
//! 4. answer the `daemon.stop` with the effective grace and what is still live;
//! 5. wait up to the grace for those operations to settle, cancelling whatever is left;
//! 6. stop the watchers and the account runtimes, releasing the engine locks;
//! 7. send `daemon.stopped` on the asking connection and close every connection;
//! 8. unlink its own socket, `daemon.pid` and `daemon.json`, and exit 0.
//!
//! A hold is therefore **never** in `pending` or in `unsettled`: it is
//! cancelled at step 2 rather than waited out, which is the plan's "settled or
//! cancelled, never silently dropped" for a window whose whole point is that
//! the user may still stop it. With nothing live at step 4 the daemon does not
//! enter the grace at all, which is why every existing suite's
//! `mp daemon stop` stays as fast as it is today.
//!
//! # What this file deliberately does not pin
//!
//! - **The last client exiting mid-hold.** `tests/phase5_undo_send_hold.rs`
//!   (P5-U9, oracle (e)) asserts it from the outside and P6-U2 landed the rule
//!   in `server.rs::handle_connection`. A shutdown is a different event and
//!   gets its own rows below.
//! - **A stale socket or pidfile after a SIGKILL.**
//!   `tests/daemon_runtime_paths.rs` owns the probe
//!   (`a_socket_with_no_listener_probes_stale`, `a_stale_socket_is_removed`)
//!   and `tests/daemon_lifecycle.rs` owns the sweep on the next start. A
//!   killed daemon runs none of the eight steps above, so it is not this
//!   unit's subject.
//! - **A socket owned by another uid.**
//!   `tests/daemon_runtime_paths.rs::a_socket_owned_by_another_uid_probes_unsafe_and_is_never_removed`
//!   pins it, and it is a property of the *probe*, which shutdown does not
//!   run. What shutdown owes is the row below: it unlinks three paths under
//!   its own runtime directory and nothing else.
//! - **The engine lock taken and released across a kill.**
//!   `tests/phase5_parity_gate.rs::the_engine_the_legacy_lock_suite_assumes_is_the_daemon`
//!   pins that. The row here is the graceful path, where the lock is released
//!   by step 6 rather than by the kernel reaping the process.
//!
//! # Why `test.operation` and not a real one
//!
//! The grace rows need work whose duration is an argument. `test.operation`
//! (`MAILYPOPPINS_DAEMON_FAKE_OPERATIONS=1`, `tests/daemon_operations.rs`) is
//! `steps` x `step_ms` of durable work that observes its cancellation token,
//! so "settles inside the grace" and "outlasts the grace" are the same method
//! with two numbers. A real `sync.full` would make the row a measurement of
//! the machine.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, Notification};

use mailypoppins::engine_lock::EngineLock;

use support::parity::{mp_command, pgrep_line_for, socket_path, DaemonFixture};
use support::send_fixture as fixture;

/// The grace a `daemon.stop` that named none gets, in seconds.
const DEFAULT_GRACE_SECS: u64 = 10;

/// The hook that registers `test.operation`.
const FAKE_OPERATIONS_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_OPERATIONS";

/// The undo-send window the hold rows configure.
///
/// Long enough that no row can lose the race to a hold firing on its own: the
/// question in every one of them is what the *shutdown* did to the hold.
const HOLD_SECS: u64 = 20;

/// How long a row waits for one notification before failing.
const EVENT_DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// Ceiling on a wait for a process, a file or a lock to go.
const DEADLINE: Duration = Duration::from_secs(20);

/// The keys of one operation as `operation.status` renders it, sorted.
///
/// `pending` and `unsettled` carry this object and not a summary of it: a GUI
/// that lists what is still running at shutdown renders it with the code it
/// already has for `snapshot.operations`.
const OPERATION_KEYS: [&str; 7] = [
    "error",
    "method",
    "operation_id",
    "progress",
    "result",
    "scope",
    "state",
];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A root with no configuration at all: zero accounts, no store, no lock.
///
/// What the operation rows want. A daemon over an empty root still serves
/// `initialize`, `state.bootstrap` and the fake operation, and starts no
/// runtime that would make the row about an account.
fn bare_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary shutdown root")
}

/// Seed the send fixture under `root` with an undo-send window of `secs`.
///
/// The `[email]` table is prepended, for the reason
/// `tests/daemon_send_hold.rs` records: `config.toml` is a list of
/// `[[accounts]]` array tables and a top-level table written after one of them
/// belongs to the last account rather than to the document.
fn seed_with_hold(root: &Path, secs: u64) {
    fixture::seed(root);
    let path = root.join("config.toml");
    let existing = fs::read_to_string(&path).expect("the fixture wrote a config.toml");
    fs::write(
        &path,
        format!("[email]\nsend_hold_secs = {secs}\n\n{existing}"),
    )
    .expect("write config.toml");
}

/// A daemon over `root` that serves `test.operation`.
fn daemon_with_operations(root: &Path) -> DaemonFixture {
    DaemonFixture::start_with(root, None, &[(FAKE_OPERATIONS_ENV, "1")])
}

/// A daemon over `root` whose transport is the fake one writing to `log`.
fn daemon_with_transport(root: &Path, log: &Path) -> DaemonFixture {
    DaemonFixture::start_with(
        root,
        None,
        &[(fixture::FAKE_TRANSPORT_ENV, &fixture::fake_transport(log))],
    )
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

/// A connection that never handshakes, which is what `mp daemon stop` is.
///
/// `daemon.status` and `daemon.stop` are lifecycle surface and answer ahead of
/// the gate, which is what lets these commands end a daemon whose protocol
/// range the caller cannot negotiate.
async fn lifecycle_client(root: &Path) -> Connection {
    Connection::connect(&socket_path(root))
        .await
        .expect("connecting to a live daemon socket succeeds")
}

/// A client of the kind the TUI is: handshaken, bootstrapped, subscribed.
async fn tui_client(root: &Path) -> Connection {
    let mut conn = lifecycle_client(root).await;
    handshake(&mut conn, root)
        .await
        .expect("a compatible handshake succeeds");
    conn.call("state.bootstrap", json!({}))
        .await
        .expect("a bootstrap registers this connection for events");
    conn
}

/// The handshake itself, as a `Result`, so a row can assert it is refused.
async fn handshake(conn: &mut Connection, root: &Path) -> Result<(), ClientError> {
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
    .map(|_| ())
}

/// The next notification, or a failure naming what never arrived.
async fn next_notification(conn: &mut Connection, what: &str) -> Notification {
    tokio::time::timeout(EVENT_DEADLINE, conn.next_notification())
        .await
        .unwrap_or_else(|_| panic!("no {what} arrived within {EVENT_DEADLINE:?}"))
        .unwrap_or_else(|| panic!("the daemon closed the connection before any {what}"))
}

/// Read notifications until one carries `kind`, and answer its payload.
///
/// Skips whatever else the daemon publishes on the way, which is how every
/// event row in this tree is written: the fan-out is shared and a row may not
/// depend on being the only thing happening.
async fn await_kind(conn: &mut Connection, kind: &str) -> Value {
    let deadline = Instant::now() + EVENT_DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no {kind} event arrived within {EVENT_DEADLINE:?}"
        );
        let notification = next_notification(conn, kind).await;
        if notification.params["kind"] == kind {
            return notification.params["payload"].clone();
        }
    }
}

/// Everything left on this connection, in order, until the daemon closes it.
///
/// A shutdown must end in an EOF and not in a connection the client is left
/// holding: the `None` this stops on is the assertion.
async fn drain_to_eof(conn: &mut Connection) -> Vec<Notification> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + EVENT_DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "the daemon never closed the connection: {} notification(s) so far, last {:?}",
            seen.len(),
            seen.last().map(|n: &Notification| n.method.clone())
        );
        match tokio::time::timeout(EVENT_DEADLINE, conn.next_notification()).await {
            Ok(Some(notification)) => seen.push(notification),
            Ok(None) => return seen,
            Err(_) => panic!("the daemon neither spoke nor closed within {EVENT_DEADLINE:?}"),
        }
    }
}

/// The JSON-RPC error a refused call carried.
fn refusal(result: Result<Value, ClientError>, what: &str) -> mp_protocol::RpcError {
    match result {
        Err(ClientError::Rpc(error)) => error,
        Err(other) => panic!("{what} failed without a JSON-RPC error: {other}"),
        Ok(value) => panic!("{what} was answered rather than refused: {value}"),
    }
}

/// An object's keys, sorted.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("an object, and this is {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// The three files a daemon writes into `<data_dir>/runtime`.
fn runtime_files(root: &Path) -> [PathBuf; 3] {
    let runtime = root.join("runtime");
    [
        runtime.join("daemon.sock"),
        runtime.join("daemon.pid"),
        runtime.join("daemon.json"),
    ]
}

/// Poll `cond` until it holds, or fail after [`DEADLINE`].
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !cond() {
        assert!(Instant::now() < deadline, "{what} (waited {DEADLINE:?})");
        std::thread::sleep(TICK);
    }
}

/// Wait until the daemon over `root` has finished step 8, and answer nothing.
///
/// The *files* are what a shutdown is watched by, here and in
/// `mp daemon stop`'s own loop. The process is deliberately not: a daemon
/// [`DaemonFixture`] spawned is this test binary's child, so between its exit
/// and the fixture's `wait` it is a zombie, and `kill(pid, 0)` answers for a
/// zombie exactly as it does for a live process. What "no process is left" is
/// asserted with is [`pgrep_line_for`], which reads a command line a zombie
/// no longer has.
fn wait_until_stopped(root: &Path) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let left: Vec<String> = runtime_files(root)
            .into_iter()
            .filter(|path| path.exists())
            .map(|path| path.display().to_string())
            .collect();
        if left.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon did not finish stopping within {DEADLINE:?}: {left:?} still there"
        );
        std::thread::sleep(TICK);
    }
}

/// Whether `account`'s engine lock under `root` can be taken right now.
fn engine_lock_is_free(root: &Path, account: &str) -> bool {
    let path = fixture::engine_lock_path(root, account);
    match EngineLock::try_acquire_at(&path, account) {
        Ok(Some(held)) => {
            drop(held);
            true
        }
        Ok(None) => false,
        Err(e) => panic!("the engine lock {} is not readable: {e}", path.display()),
    }
}

/// Start a durable `test.operation` of `steps` x `step_ms` and answer its id.
async fn start_operation(conn: &mut Connection, steps: u64, step_ms: u64) -> String {
    let answer = conn
        .call(
            "test.operation",
            json!({"steps": steps, "step_ms": step_ms}),
        )
        .await
        .expect("the fake operation method answers at once");
    answer["operation_id"]
        .as_str()
        .expect("an operation answers with its id")
        .to_string()
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
    answer["operation_id"]
        .as_str()
        .expect("a send answers with an operation id")
        .to_string()
}

/// `stdout` as lines, with the trailing newline dropped.
fn stdout_lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// `mp daemon stop` against `root`, with whatever extra arguments.
fn mp_daemon_stop(root: &Path, args: &[&str]) -> Output {
    mp_command(root)
        .args(["daemon", "stop"])
        .args(args)
        .output()
        .expect("run mp daemon stop")
}

// ===========================================================================
// 1. A stop with nothing active
// ===========================================================================

/// The whole clean shutdown, on the connection that asked for it.
///
/// Three facts in one story because they are one exchange: the answer names
/// the grace it will honour and the empty list of work it has to settle, the
/// last frame is the daemon's own report, and then the socket is closed rather
/// than left open for a client to discover by writing into it.
///
/// It also pins that an idle daemon **does not wait**: with nothing live there
/// is nothing to settle, so the grace is a ceiling that is never reached, and
/// every existing suite's `mp daemon stop` costs what it costs today.
#[test]
fn a_stop_with_nothing_active_answers_at_once_and_reports_itself_before_closing() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = DaemonFixture::start(root);

    let started = Instant::now();
    let (answer, tail) = block_on(async {
        let mut conn = lifecycle_client(root).await;
        let answer = conn
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers before it shuts anything down");
        (answer, drain_to_eof(&mut conn).await)
    });

    assert_eq!(
        keys(&answer),
        ["grace_secs", "pending", "stopping"],
        "the stop answer names the grace it will honour and the work it has to settle: {answer}"
    );
    assert_eq!(answer["stopping"], json!(true));
    assert_eq!(
        answer["grace_secs"],
        json!(DEFAULT_GRACE_SECS),
        "a stop that named no grace gets the default: {answer}"
    );
    assert_eq!(
        answer["pending"],
        json!([]),
        "and an idle daemon has nothing to settle: {answer}"
    );

    let report = tail
        .last()
        .unwrap_or_else(|| panic!("the daemon closed the connection without reporting: {tail:?}"));
    assert_eq!(
        report.method, "daemon.stopped",
        "the last frame on the asking connection is the daemon's own report: {tail:?}"
    );
    assert_eq!(
        keys(&report.params),
        ["clean", "instance_id", "unsettled"],
        "the report says which daemon stopped and what it could not settle: {}",
        report.params
    );
    assert_eq!(
        report.params["clean"],
        json!(true),
        "a stop with nothing active is clean: {}",
        report.params
    );
    assert_eq!(report.params["unsettled"], json!([]));

    assert!(
        started.elapsed() < Duration::from_secs(DEFAULT_GRACE_SECS),
        "an idle daemon does not sit out its grace: the stop took {:?} of a {DEFAULT_GRACE_SECS}s \
         ceiling",
        started.elapsed()
    );

    daemon.stop();
}

/// The daemon leaves no socket, no pid file, no instance file and no process.
///
/// `tests/daemon_lifecycle.rs` pins the socket alone for the shutdown that
/// existed before this unit; what the graceful one owes is all three, because
/// step 8 unlinks all three and a watcher or a runtime that outlived the
/// process would be visible as a `mp daemon` still in the process list.
#[test]
fn a_clean_stop_leaves_no_runtime_file_and_no_process() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = DaemonFixture::start(root);
    let pid = daemon.pid();

    for path in runtime_files(root) {
        assert!(
            path.exists(),
            "a running daemon has written {}",
            path.display()
        );
    }

    block_on(async {
        let mut conn = lifecycle_client(root).await;
        conn.call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");
        drain_to_eof(&mut conn).await;
    });

    wait_until_stopped(root);
    assert_eq!(
        pgrep_line_for(pid),
        None,
        "and nothing of it is left in the process list: the watchers and the account runtimes \
         ended with it"
    );
}

/// A graceful stop releases the engine locks its runtimes held.
///
/// The kill path is `tests/phase5_parity_gate.rs`'s
/// `the_engine_the_legacy_lock_suite_assumes_is_the_daemon`, where the kernel
/// releases the descriptor because the process died. This is the other one:
/// step 6 stops the runtimes, so the lock is free because the daemon let go of
/// it, and the next `mp sync` has an engine to be. No assertion can tell the
/// two apart from outside - the process ends either way - so what this row
/// owes is that the graceful path, which runs eight steps the kill path runs
/// none of, ends with the lock free all the same.
#[test]
fn a_graceful_stop_releases_the_engine_locks_the_runtimes_held() {
    let tmp = bare_root();
    let root = tmp.path();
    fixture::seed(root);
    let account = fixture::ACCOUNT;

    assert!(
        engine_lock_is_free(root, account),
        "nothing holds {}'s engine lock before a daemon starts",
        account
    );

    let daemon = DaemonFixture::start(root);
    wait_until(
        "the daemon took the account's engine lock, which is what makes it the engine",
        || !engine_lock_is_free(root, account),
    );

    block_on(async {
        let mut conn = lifecycle_client(root).await;
        conn.call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");
        drain_to_eof(&mut conn).await;
    });

    wait_until_stopped(root);
    wait_until(
        &format!(
            "a stopped daemon holds no engine lock, and {} is still taken",
            fixture::engine_lock_path(root, account).display()
        ),
        || engine_lock_is_free(root, account),
    );
    let _ = daemon;
}

/// Every bootstrapped client is told, and then its socket closes.
///
/// A client that learned about the shutdown only by writing into a closed
/// socket would report an I/O failure where the daemon did something orderly,
/// and a GUI would have nothing to put on screen. The event is a `state.event`
/// like any other, so no client needs a second code path to receive it.
#[test]
fn every_bootstrapped_client_is_told_before_its_socket_closes() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = DaemonFixture::start(root);

    let payload = block_on(async {
        let mut watcher = tui_client(root).await;
        let mut stopper = lifecycle_client(root).await;
        stopper
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");

        let payload = await_kind(&mut watcher, "daemon.shutting_down").await;
        let after = drain_to_eof(&mut watcher).await;
        assert!(
            after
                .iter()
                .all(|n| n.params["kind"] != json!("daemon.shutting_down")),
            "one announcement, not a stream of them: {after:?}"
        );
        payload
    });

    assert_eq!(
        keys(&payload),
        ["grace_secs", "pending"],
        "the announcement carries the grace and the work it covers: {payload}"
    );
    assert_eq!(payload["grace_secs"], json!(DEFAULT_GRACE_SECS));
    assert_eq!(payload["pending"], json!([]));

    daemon.stop();
}

// ===========================================================================
// 2. A shutting-down daemon stops accepting commands
// ===========================================================================

/// From the instant the stop is accepted, everything but the two lifecycle
/// methods is `-32009`.
///
/// Three refusals, one rule. A handshaken connection is refused on its next
/// call, a connection that arrives during the grace is refused at its
/// `initialize` rather than being allowed to negotiate its way into a daemon
/// that is going away, and `daemon.status` keeps answering because that is how
/// `mp daemon stop` watches the shutdown it asked for.
///
/// The grace is the default here and the row never waits it out: the daemon is
/// held open by a twenty-second operation, the assertions take milliseconds,
/// and the fixture's `Drop` ends the process.
#[test]
fn a_shutting_down_daemon_refuses_every_command_but_the_two_lifecycle_ones() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = daemon_with_operations(root);

    block_on(async {
        let mut worker = tui_client(root).await;
        start_operation(&mut worker, 40, 500).await;

        let mut client = tui_client(root).await;
        let mut stopper = lifecycle_client(root).await;
        stopper
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");

        let refused = refusal(
            client.call("state.bootstrap", json!({})).await,
            "a domain call during the grace",
        );
        assert_eq!(
            refused.code,
            ErrorCode::ShuttingDown.code(),
            "a daemon that is going away refuses work as `{}`: {refused:?}",
            ErrorCode::ShuttingDown.name()
        );
        assert_eq!(
            refused.message, "the daemon is shutting down",
            "and says so in one sentence a client can print: {refused:?}"
        );

        let mut latecomer = lifecycle_client(root).await;
        let refused = refusal(
            handshake(&mut latecomer, root).await.map(|()| json!({})),
            "an initialize during the grace",
        );
        assert_eq!(
            refused.code,
            ErrorCode::ShuttingDown.code(),
            "a client that arrives during the grace is refused at the handshake rather than \
             admitted to a daemon that is leaving: {refused:?}"
        );

        let status = latecomer
            .call("daemon.status", json!({}))
            .await
            .expect("daemon.status keeps answering during the grace: it is how a stop is watched");
        assert!(
            status.get("instance_id").is_some(),
            "and answers the same object it always did: {status}"
        );

        let again = stopper
            .call("daemon.stop", json!({}))
            .await
            .expect("a second stop is answered rather than refused");
        assert_eq!(
            again["stopping"],
            json!(true),
            "a stop of a stopping daemon is not an error: {again}"
        );
    });

    drop(daemon);
}

// ===========================================================================
// 3. The grace
// ===========================================================================

/// Work that finishes inside the grace makes the stop clean.
///
/// The positive half of the pair below: the same method, the same shutdown,
/// and an operation short enough to settle. `pending` names it at step 4,
/// nothing is left at step 5, and the report says so.
#[test]
fn an_operation_that_finishes_inside_the_grace_stops_cleanly() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = daemon_with_operations(root);

    let started = Instant::now();
    let (operation, answer, tail) = block_on(async {
        let mut worker = tui_client(root).await;
        let operation = start_operation(&mut worker, 2, 300).await;

        let mut stopper = lifecycle_client(root).await;
        let answer = stopper
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");
        (operation, answer, drain_to_eof(&mut stopper).await)
    });

    let pending = answer["pending"]
        .as_array()
        .unwrap_or_else(|| panic!("pending is an array: {answer}"));
    assert_eq!(
        pending.len(),
        1,
        "the operation in flight is what the daemon has to settle: {answer}"
    );
    assert_eq!(
        keys(&pending[0]),
        OPERATION_KEYS,
        "and it travels as the `operation.status` object, not as a summary: {}",
        pending[0]
    );
    assert_eq!(pending[0]["operation_id"], json!(operation));
    assert_eq!(pending[0]["method"], json!("test.operation"));

    let report = tail
        .last()
        .unwrap_or_else(|| panic!("the daemon reported before closing: {tail:?}"));
    assert_eq!(report.method, "daemon.stopped");
    assert_eq!(
        report.params["clean"],
        json!(true),
        "work that settled inside the grace is a clean stop: {}",
        report.params
    );
    assert_eq!(report.params["unsettled"], json!([]));
    assert!(
        started.elapsed() < Duration::from_secs(DEFAULT_GRACE_SECS),
        "and the daemon left as soon as the work was done rather than at the ceiling: {:?}",
        started.elapsed()
    );

    daemon.stop();
}

/// Work the grace does not outlast is named, by the daemon and by the command.
///
/// The plan's last clause: *"`mp daemon stop` names the operations that
/// prevented a clean stop."* Two lines on stdout and nothing else, because a
/// user who has to read a paragraph to learn which sync was killed will not
/// read it.
///
/// Exit 0: the daemon stopped, which is what was asked. A nonzero code here
/// would make `mp daemon restart` refuse to start the replacement, and would
/// turn "a sync was still running" into a failure of the command that
/// succeeded.
#[test]
fn an_operation_the_grace_does_not_outlast_is_named_by_mp_daemon_stop() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = daemon_with_operations(root);

    let operation = block_on(async {
        let mut worker = tui_client(root).await;
        start_operation(&mut worker, 40, 500).await
    });

    let out = mp_daemon_stop(root, &["--grace-secs", "1"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a stop that had to cut work short is still a stop; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout_lines(&out),
        vec![
            "\u{2717} daemon stopped, 1 operation did not settle within 1s".to_string(),
            format!("  test.operation ({operation})"),
        ],
        "the command names what prevented a clean stop, one line each; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    assert!(
        !socket_path(root).exists(),
        "a forced stop is still a stop: the command returned and the socket is gone"
    );
    assert_eq!(
        pgrep_line_for(daemon.pid()),
        None,
        "and the process is gone with it rather than left holding cancelled work"
    );
}

/// A clean stop prints the line it has always printed.
///
/// The guard on the row above: a wording introduced for the unclean case that
/// leaked into the clean one would change what every user sees on every stop.
#[test]
fn a_clean_stop_prints_the_line_it_has_always_printed() {
    let tmp = bare_root();
    let root = tmp.path();
    let daemon = DaemonFixture::start(root);

    let out = mp_daemon_stop(root, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stop against a live daemon exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout_lines(&out),
        vec!["\u{2713} daemon stopped".to_string()],
        "a clean stop says one thing and nothing else; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        pgrep_line_for(daemon.pid()),
        None,
        "and the command returned only once the daemon was really gone"
    );
}

// ===========================================================================
// 4. An active hold
// ===========================================================================

/// A stop cancels an armed hold and leaves the draft approved.
///
/// *"An active hold is settled or cancelled, never silently dropped."* A
/// window the user can still close is not work to be waited out: a daemon that
/// sat out the remaining seconds and then sent would send mail nobody could
/// stop any more, because the client that would have pressed `u` is losing its
/// socket in the same second.
///
/// So the hold is cancelled at step 2, the clients are told with the event
/// they already render, the draft is exactly what the approve left, the
/// transport hears nothing, and the hold is in neither `pending` nor
/// `unsettled`: it was settled, not abandoned.
///
/// The watcher stays connected throughout, so the last-client rule
/// (`server.rs::handle_connection`, P6-U2) is not what cancels here.
#[test]
fn a_stop_cancels_an_armed_hold_and_leaves_the_draft_approved() {
    let tmp = bare_root();
    let root = tmp.path();
    seed_with_hold(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = daemon_with_transport(root, &log);

    let (operation, answer, report) = block_on(async {
        let mut sender = tui_client(root).await;
        let mut watcher = tui_client(root).await;
        let operation = send_held(&mut sender, fixture::ACCOUNT, fixture::APPROVED).await;
        await_kind(&mut watcher, "send.hold_started").await;

        let mut stopper = lifecycle_client(root).await;
        let answer = stopper
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");

        let cancelled = await_kind(&mut watcher, "send.hold_cancelled").await;
        assert_eq!(
            cancelled["operation_id"],
            json!(operation),
            "the hold that was cancelled is the one that was armed: {cancelled}"
        );
        assert_eq!(
            cancelled["remaining_secs"],
            json!(0),
            "a hold that ended has nothing left of its window: {cancelled}"
        );
        await_kind(&mut watcher, "daemon.shutting_down").await;
        drain_to_eof(&mut watcher).await;

        let report = drain_to_eof(&mut stopper)
            .await
            .last()
            .cloned()
            .expect("the daemon reported before closing");
        (operation, answer, report)
    });

    assert_eq!(
        answer["pending"],
        json!([]),
        "a hold is cancelled before the grace is computed, so it is never work to wait for: \
         {answer}"
    );
    assert_eq!(
        report.params["unsettled"],
        json!([]),
        "and never work that prevented a clean stop either: {}",
        report.params
    );
    assert_eq!(report.params["clean"], json!(true));

    wait_until_stopped(root);
    let _ = daemon;

    let file = fixture::drafts_dir(root, fixture::ACCOUNT).join(fixture::APPROVED_FILE);
    let document = fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("a cancelled hold leaves {} on disk: {e}", file.display()));
    assert!(
        document.contains("status: approved"),
        "and leaves it exactly as the approve left it, ready to send again ({operation}):\n\
         {document}"
    );
    let events = fixture::transport_events(&log);
    assert!(
        events.is_empty(),
        "nothing reached the transport, and the fake one recorded {} event(s): {events:?}",
        events.len()
    );
}

// ===========================================================================
// 5. Only its own files
// ===========================================================================

/// The graceful path unlinks its own three runtime files and nobody else's.
///
/// `tests/daemon_lifecycle.rs::stop_removes_only_its_own_socket` pins the
/// socket across the shutdown that existed before this unit. This row is the
/// one the new path needs: a stop that ran the whole eight-step sequence, with
/// an operation settled inside the grace, still touches nothing outside its
/// own runtime directory, and the second daemon is answering afterwards rather
/// than merely still having a file.
#[test]
fn a_graceful_stop_removes_its_own_runtime_files_and_no_others() {
    let mine = bare_root();
    let other = bare_root();
    let daemon = daemon_with_operations(mine.path());
    let neighbour = DaemonFixture::start(other.path());

    block_on(async {
        let mut worker = tui_client(mine.path()).await;
        start_operation(&mut worker, 2, 100).await;
        let mut stopper = lifecycle_client(mine.path()).await;
        stopper
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers");
        drain_to_eof(&mut stopper).await;
    });

    wait_until_stopped(mine.path());
    let _ = daemon;
    for path in runtime_files(other.path()) {
        assert!(
            path.exists(),
            "and left the other daemon's {} alone",
            path.display()
        );
    }

    let status = block_on(async {
        let mut conn = lifecycle_client(other.path()).await;
        conn.call("daemon.status", json!({}))
            .await
            .expect("the other daemon is still answering")
    });
    assert!(
        status.get("instance_id").is_some(),
        "and is still the daemon it was: {status}"
    );

    neighbour.stop();
}

// ===========================================================================
// 6. SIGTERM
// ===========================================================================

/// SIGTERM is `daemon.stop` with the default grace and no one to answer.
///
/// A service manager stops a unit with a signal, so a shutdown that was only
/// graceful over the socket would be graceful only when a human typed the
/// command. Same announcement, same hold cancellation, same files removed,
/// same exit code; the only thing missing is the `daemon.stopped` report,
/// which has no connection to travel on.
#[test]
fn sigterm_ends_the_daemon_exactly_as_daemon_stop_does() {
    let tmp = bare_root();
    let root = tmp.path();
    seed_with_hold(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = daemon_with_transport(root, &log);
    let pid = daemon.pid();

    let payload = block_on(async {
        let mut sender = tui_client(root).await;
        let mut watcher = tui_client(root).await;
        let operation = send_held(&mut sender, fixture::ACCOUNT, fixture::APPROVED).await;
        await_kind(&mut watcher, "send.hold_started").await;

        // SAFETY: SIGTERM to a pid this test spawned and still owns.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };

        let cancelled = await_kind(&mut watcher, "send.hold_cancelled").await;
        assert_eq!(
            cancelled["operation_id"],
            json!(operation),
            "a signal settles the hold exactly as the method does: {cancelled}"
        );
        let payload = await_kind(&mut watcher, "daemon.shutting_down").await;
        drain_to_eof(&mut watcher).await;
        payload
    });

    assert_eq!(
        payload["grace_secs"],
        json!(DEFAULT_GRACE_SECS),
        "a signal carries no parameters, so the grace is the default: {payload}"
    );

    wait_until_stopped(root);
    assert_eq!(
        pgrep_line_for(pid),
        None,
        "SIGTERM ends the process like any other stop"
    );

    let file = fixture::drafts_dir(root, fixture::ACCOUNT).join(fixture::APPROVED_FILE);
    let document =
        fs::read_to_string(&file).unwrap_or_else(|e| panic!("the draft is still on disk: {e}"));
    assert!(
        document.contains("status: approved"),
        "and the draft the hold was about is still approved:\n{document}"
    );
    assert!(
        fixture::transport_events(&log).is_empty(),
        "with nothing sent behind the user's back"
    );
}

// ===========================================================================
// 7. The bootstrap a client takes mid-hold
// ===========================================================================

/// A client that connects mid-hold finds the hold in its bootstrap.
///
/// P6-U2 left `snapshot.holds` hard-coded to `[]` in
/// `src/daemon/state/snapshot.rs` and recorded it as a follow-up; this is the
/// row that closes it, and it belongs to shutdown because it is the same
/// question. A client that joins while a window is running must be able to
/// render the countdown and press `u`, and a client that joins while the
/// daemon is stopping must be able to see that the window is gone. Both are
/// "the snapshot tells the truth about the holds", and only the first needs a
/// row: the second is the empty array every other row here already asserts.
///
/// The object is the one `send.hold_status` answers, byte for byte, which is
/// what the P6-U1 contract promised: *"a client that joined after the
/// countdown started renders the same object the event carries."*
#[test]
fn a_client_that_bootstraps_mid_hold_finds_the_hold_in_its_snapshot() {
    let tmp = bare_root();
    let root = tmp.path();
    seed_with_hold(root, HOLD_SECS);
    let log = fixture::transport_log(root);
    let daemon = daemon_with_transport(root, &log);

    block_on(async {
        let mut sender = tui_client(root).await;
        let operation = send_held(&mut sender, fixture::ACCOUNT, fixture::APPROVED).await;
        await_kind(&mut sender, "send.hold_started").await;

        // A second client, connecting and bootstrapping after the countdown
        // started: exactly the GUI that was launched mid-window.
        let mut latecomer = lifecycle_client(root).await;
        handshake(&mut latecomer, root)
            .await
            .expect("a compatible handshake");
        let bootstrap = latecomer
            .call("state.bootstrap", json!({}))
            .await
            .expect("state.bootstrap answers");
        let holds = bootstrap["snapshot"]["holds"]
            .as_array()
            .unwrap_or_else(|| panic!("the snapshot carries a holds array: {bootstrap}"))
            .clone();
        assert_eq!(
            holds.len(),
            1,
            "a client that joined mid-hold is told about the window it cannot otherwise know \
             about: {bootstrap}"
        );
        assert_eq!(holds[0]["operation_id"], json!(operation));
        assert_eq!(holds[0]["account"], json!(fixture::ACCOUNT));
        assert_eq!(holds[0]["draft_id"], json!(fixture::APPROVED));
        assert_eq!(holds[0]["hold_secs"], json!(HOLD_SECS));
        assert!(
            holds[0]["remaining_secs"].as_u64().unwrap_or(0) > 0,
            "with the remainder the daemon owns rather than one it computed: {}",
            holds[0]
        );

        let listing = latecomer
            .call("send.hold_status", json!({"account": fixture::ACCOUNT}))
            .await
            .expect("send.hold_status answers");
        assert_eq!(
            listing["holds"],
            Value::Array(holds),
            "and it is the very object the query answers, so one renderer serves both: {listing}"
        );
    });

    daemon.stop();
}

/// A client that asks the daemon to stop and hangs up at once - a Ctrl-C on
/// `mp daemon stop`, a call that timed out - owes nothing any more: the daemon
/// does not sit out the report ceiling waiting for a frame nobody will read,
/// and does not warn about a client that never took its report.
#[test]
fn a_stop_whose_client_hangs_up_at_once_does_not_wait_for_its_report() {
    use std::io::Write;

    let tmp = bare_root();
    let root = tmp.path();
    let daemon = DaemonFixture::start(root);

    let started = Instant::now();
    {
        let mut stream = std::os::unix::net::UnixStream::connect(socket_path(root))
            .expect("connecting to the daemon socket");
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"daemon.stop\",\"params\":{}}\n")
            .expect("writing the stop frame");
    }
    wait_until_stopped(root);
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(1500),
        "a stop whose asker hung up waited {took:?}, which is the report ceiling"
    );

    let logs: String = fs::read_dir(root.join("logs"))
        .map(|dir| {
            dir.flatten()
                .filter_map(|entry| fs::read_to_string(entry.path()).ok())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !logs.contains("never took its report"),
        "the daemon warned about a report its hung-up client could not take:\n{logs}"
    );

    daemon.stop();
}
