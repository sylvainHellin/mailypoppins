//! A daemon killed mid-session, and the client that survives it (P5-U7, ticket
//! #0124).
//!
//! The socket-level half of the P5-U7 contract. Its in-process half is
//! `src/tui/events_tests.rs`, which drives `App::apply_event` and
//! `tui::events::drain` over a dispatcher in this process; three things cannot
//! be reached from there and are here instead:
//!
//! 1. **Account runtimes are on by default.** Whether `mp daemon run` starts a
//!    runtime is a property of the executable and of its environment, and
//!    [`sandbox_env`] removes `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES` from every
//!    child it spawns, so a fixture here cannot accidentally be given the
//!    opt-in plan section 3.7 says this unit drops.
//! 2. **The watcher lives in the runtime and publishes what it found.** A tick
//!    reaches a subscribed client as a `sync.completed` event carrying
//!    `new_inbox_mail`, which is where the desktop notification of #0009 comes
//!    from once `imap_watch` and the Graph poller have left `src/tui/`.
//! 3. **A killed daemon is survived.** A real `mp daemon run` is killed with
//!    `SIGKILL`, a real [`Session`] notices, a second daemon is started, and
//!    the session reconnects and bootstraps again against the new instance.
//!
//! # Why this file compiles today and fails anyway
//!
//! Every name it uses exists at `51e2622`: [`Session`], [`Session::call`], the
//! parity harness and the sync fixture. Unlike the T unit's in-process module
//! it is therefore not a compile error, and it must not be one - `tests/*.rs`
//! is autodiscovered and the `daemon` cargo feature that used to gate a
//! contract file is gone since P4-U1, so a `tests/` file that did not compile
//! would take `cargo test --workspace` down with it. What it does instead is
//! **fail at runtime**, on the three assertions above, each of which describes
//! behaviour P5-U8 has to build:
//!
//! - the account stays `opening` for ever without the environment variable;
//! - a `sync.completed` payload carries no `new_inbox_mail` member;
//! - a session whose daemon died never calls again, because nothing reconnects.
//!
//! The `new_inbox_mail` assertion reads the raw payload rather than a decoded
//! [`SyncCompleted`] for the same reason: the field is the contract and does
//! not exist yet, so naming it in Rust would turn this file into the compile
//! error it may not be.
//!
//! # No direct fallback
//!
//! The plan's phrase for the failure this unit must not introduce: a client
//! that answers a dead daemon by opening the store itself. It is asserted as a
//! **lock**, not as an intention. An account's engine lock is
//! `<root>/accounts/<account>/store.lock`, `flock`-held for a runtime's whole
//! lifetime, and `flock` is per open file description rather than per process,
//! so a second description of the same file conflicts even inside one process.
//! During the outage the daemon is gone and the kernel has released its lock,
//! so the test process can take it - unless the client under test took it
//! first, which is exactly the fallback. The source-level half of the same
//! question (no `open_store` reachable from an event path) is the residue gate
//! in `src/tui/events_tests.rs`.
//!
//! # Process environment
//!
//! [`Session::connect`] reads the ambient `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR`, so the recovery row sets them on **this** process
//! rather than on a child. Nothing else in this file reads the process
//! environment - [`DaemonFixture`] sets every child's explicitly and
//! [`Connection::connect`] takes a path - so the three rows do not have to be
//! serialised against each other. A row added here that reads the ambient
//! environment would have to take a lock first.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use mailypoppins::engine_lock::EngineLock;
use mailypoppins::tui::session::Session;

use mp_client::{ClientInfo, ClientKind, Connection, Identity};

use support::parity::{
    daemon_pid_file, process_is_alive, sandbox_env, socket_path, DaemonFixture, DEADLINE, TICK,
};
use support::sync_fixture;

/// The seeded fixture's first account, which is the one every row is about.
const ACCOUNT: &str = sync_fixture::ACCOUNT;

/// How long a row waits for a state a daemon reaches on its own schedule: a
/// runtime coming up, a fake tick being published, a session reconnecting.
///
/// A ceiling and never a sleep; every wait below polls and returns as soon as
/// it can.
const SETTLE: Duration = DEADLINE;

// ---------------------------------------------------------------------------
// 1. Account runtimes are on by default
// ---------------------------------------------------------------------------

/// A daemon started with no environment variable at all serves a ready account
/// and holds its engine lock.
///
/// Plan section 3.7: this unit *"turns on account runtimes by default (drops
/// `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`)"*. Until then the same fixture
/// leaves every account `opening` for ever, which is what
/// `tests/daemon_account_runtime.rs`'s
/// `without_the_environment_opt_in_the_daemon_starts_no_runtime_and_takes_no_engine_lock`
/// asserts today - that test is the one edit P5-U8 owes under `tests/`, because
/// the behaviour it pins is the behaviour this unit removes.
///
/// The engine lock is half the assertion and not decoration: an account
/// reported `ready` by a daemon that holds no lock is a daemon claiming to be
/// an engine it is not, and the TUI would then be watching a mailbox nobody
/// syncs.
#[test]
fn an_account_comes_up_ready_without_the_runtimes_environment_variable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    sync_fixture::seed(tmp.path());
    let daemon = DaemonFixture::start(tmp.path());

    let state = settled_account_state(&daemon, ACCOUNT);
    assert_eq!(
        state, "ready",
        "an account whose lock is free comes up ready with no opt-in to give it"
    );
    assert!(
        !engine_lock_is_free(tmp.path(), ACCOUNT),
        "a live runtime holds its account's engine lock"
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 2. The watcher lives in the runtime, and its tick carries the arrivals
// ---------------------------------------------------------------------------

/// A runtime's tick reaches a subscribed client as an event, and the event
/// carries what arrived.
///
/// The fixture has no IMAP server (`tests/support/sync_fixture.rs` explains
/// why there is none anywhere in this repository), so the tick is forced
/// through `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME`, exactly as
/// `tests/daemon_sync_outcome.rs` and `tests/daemon_sync_slice.rs` do. What is
/// being proved is the path - runtime, event, payload - and not the sync.
///
/// The hook is armed **without** `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`, which
/// is the second half of the row: `sync_outcome::fake_sync_outcomes` gates
/// itself on that variable today and must stop, because after this unit there
/// is no variable to gate on.
///
/// `new_inbox_mail` is the contract. Before this unit the TUI learnt about new
/// mail from its own `imap_watch` thread, and P5-U6 put the arrival list on the
/// *answer* to whoever asked for a pass, recording that *"a pass through an
/// account runtime's tick reports `[]` ... that is P5-U8's to fix when the
/// notification moves onto the event stream"*. Nobody asks for a runtime's
/// tick, so the event is the only carrier the notification has left.
#[test]
fn a_runtime_tick_reaches_a_subscribed_client_with_its_arrivals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    sync_fixture::seed(tmp.path());
    let outcome = json!([{
        "account": ACCOUNT,
        "severity": "ok",
        "saved": 1,
        "skipped": 0,
        "flags_updated": 0,
        "pruned": 0,
        "prunes_deferred": 0,
        "uid_rebound": 0,
        "uidvalidity_resets": 0,
        "bodies_truncated": 0,
        "non_converging": [],
        "failed_mutations": 0,
        "error": null,
        "new_inbox_mail": [{"from": "Ada <ada@example.com>", "subject": "Analytical engine"}],
    }]);
    let daemon = DaemonFixture::start_with(
        tmp.path(),
        None,
        &[(
            "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME",
            &outcome.to_string(),
        )],
    );

    let payload = runtime(&daemon.root().to_path_buf(), |root| async move {
        let mut conn = subscribed(&root).await;
        let deadline = Instant::now() + SETTLE;
        loop {
            assert!(
                Instant::now() < deadline,
                "no sync.completed event arrived within {SETTLE:?}"
            );
            let notification = tokio::time::timeout(SETTLE, conn.next_notification())
                .await
                .expect("the daemon keeps the stream open")
                .expect("the daemon keeps the connection open");
            if notification.params["kind"] == "sync.completed" {
                return notification.params["payload"].clone();
            }
        }
    });

    assert_eq!(
        payload["account"], ACCOUNT,
        "the tick names the account it belongs to"
    );
    assert_eq!(
        payload["new_inbox_mail"],
        json!([{"from": "Ada <ada@example.com>", "subject": "Analytical engine"}]),
        "the arrivals ride the tick's own event, because nobody asked for the pass"
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// 3. Kill the daemon, restart it, and recover with no direct fallback
// ---------------------------------------------------------------------------

/// A session whose daemon is killed reports the loss, opens no store while it
/// is gone, and bootstraps again against the daemon that replaces it.
///
/// The four facts, in the order the row establishes them:
///
/// - a live session answers a method call;
/// - after `SIGKILL` it refuses one, rather than serving a stale answer or
///   hanging until its 30 s ceiling;
/// - **while there is no daemon it takes no engine lock**, which is the "no
///   direct fallback" the plan requires, asserted as the lock the fallback
///   would have to hold;
/// - once a daemon is listening again the session reconnects on its own and its
///   `state.bootstrap` answers with a **different** `instance_id`, which is the
///   only thing that makes the new daemon's revisions comparable at all
///   (`docs/daemon-protocol.md`: an event from an unfamiliar instance is
///   refused whatever its number).
///
/// A restart rather than an auto-start: `mp daemon run` is the daemon the user
/// or the service manager brings back, and pinning the reconnect against a
/// daemon this test started keeps the row about the client's recovery instead
/// of about the on-demand start policy, which `tests/daemon_autostart.rs` owns.
#[test]
fn a_killed_daemon_is_survived_and_the_session_bootstraps_against_its_replacement() {
    let tmp = tempfile::tempdir().expect("tempdir");
    sync_fixture::seed(tmp.path());
    let root = tmp.path().to_path_buf();
    // `Session::connect` reads these; see the module header.
    std::env::set_var("HOME", &root);
    std::env::set_var("MAILYPOPPINS_DATA_DIR", &root);
    std::env::set_var("MAILYPOPPINS_CONFIG_DIR", &root);
    std::env::set_var("MAILYPOPPINS_DAEMON_AUTOSTART", "0");

    let first = DaemonFixture::start(&root);
    let mut session = Session::connect().expect("the session thread comes up");
    let before = instance_of(&session).expect("a live daemon bootstraps");

    let pid = daemon_pid_file(&root).expect("a running daemon writes its pid file");
    assert_eq!(pid, first.pid(), "the pid file names the daemon we started");
    // SAFETY: `kill` takes a pid and a signal number and cannot fault. The pid
    // is this test's own child, read back from the file it wrote.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    // `stop` reaps the child, which is what turns a zombie into a pid nobody
    // holds; `process_is_alive` is signal 0 and answers `true` for an unreaped
    // one, so the order here is not decoration.
    first.stop();
    assert!(!process_is_alive(pid), "the daemon we killed is gone");

    assert!(
        session.call("daemon.status", json!({})).is_err(),
        "a session whose daemon is gone refuses rather than answering from somewhere else"
    );
    assert!(
        engine_lock_is_free(&root, ACCOUNT),
        "no daemon is running, so a held engine lock can only be this client's: \
         that is the direct fallback P5-U7 forbids"
    );

    let second = DaemonFixture::start(&root);
    let after = wait_for(SETTLE, "the session to reconnect", || instance_of(&session));

    assert_ne!(
        after, before,
        "a restart is a new instance, and a revision from it is only comparable \
         after a fresh bootstrap"
    );
    assert_eq!(
        second.mp(&["daemon", "status", "--json"]).status.code(),
        Some(0),
        "the replacement is the daemon the session found"
    );

    session.close();
    second.stop();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The `instance_id` a `state.bootstrap` through `session` answers with, or
/// `None` when the session cannot reach a daemon.
fn instance_of(session: &Session) -> Option<String> {
    let answer = session.call("state.bootstrap", json!({})).ok()?;
    answer["instance_id"].as_str().map(str::to_string)
}

/// `daemon.status --json`'s state for one account, once it has left `opening`.
fn settled_account_state(daemon: &DaemonFixture, account: &str) -> String {
    wait_for(SETTLE, "the account runtime to settle", || {
        let out = daemon.mp(&["daemon", "status", "--json"]);
        let status: Value = serde_json::from_slice(&out.stdout).ok()?;
        let state = status["accounts"]
            .as_array()?
            .iter()
            .find(|entry| entry["name"] == account)?["state"]
            .as_str()?
            .to_string();
        (state != "opening").then_some(state)
    })
}

/// Whether `account`'s engine lock can be taken, which is `true` exactly when
/// nothing holds it.
fn engine_lock_is_free(root: &Path, account: &str) -> bool {
    let path = sync_fixture::engine_lock_path(root, account);
    match EngineLock::try_acquire_at(&path, account) {
        Ok(Some(held)) => {
            drop(held);
            true
        }
        Ok(None) => false,
        Err(e) => panic!("the engine lock {} is not readable: {e}", path.display()),
    }
}

/// A connection that has handshaken and bootstrapped, which is what makes it a
/// subscriber (`docs/daemon-protocol.md`: register first, then capture).
async fn subscribed(root: &Path) -> Connection {
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

/// Run one async body on a runtime of its own, so the file needs no
/// `#[tokio::test]` on rows that are entirely synchronous.
fn runtime<T, F, Fut>(root: &std::path::PathBuf, body: F) -> T
where
    F: FnOnce(std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(body(root.clone()))
}

/// Poll `produce` until it answers, failing the test rather than the suite's
/// patience if it never does.
fn wait_for<T>(budget: Duration, what: &str, mut produce: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(value) = produce() {
            return value;
        }
        assert!(Instant::now() < deadline, "waited {budget:?} for {what}");
        std::thread::sleep(TICK);
    }
}

/// Never called; it exists so an unused import cannot hide a helper this file
/// needs. [`sandbox_env`] is the environment every child of the parity harness
/// runs under, and the reason row 1 and row 2 are honest: it removes
/// `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES` from the child, so neither of them
/// can be given the opt-in by accident.
#[allow(dead_code)]
fn the_sandbox_removes_every_daemon_hook(cmd: &mut std::process::Command, root: &Path) {
    sandbox_env(cmd, root);
}
