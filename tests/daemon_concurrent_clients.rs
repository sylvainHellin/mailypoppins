//! Several clients against one daemon at the same instant (PERSO-223,
//! PERSO-224).
//!
//! Phase 4's gate asked for two `mp` processes driven concurrently against one
//! daemon, and Phase 5 for a TUI and a CLI at the same instant; before this
//! file the suite had sequential reuse
//! (`a_second_client_reuses_the_daemon_the_first_one_started`), two clients
//! racing to *start* one (`two_concurrent_starts_yield_one_pid_file_and_one_instance_id`)
//! and the manual key walk. Each row here starts every client before it waits
//! on any, so their calls overlap on the daemon's socket, and compares every
//! answer with the one the same command gives alone.
//!
//! The TUI half goes through the binary's own connector
//! (`mailypoppins::daemon::client::tui_connector`), which is also what decides
//! the kind a TUI announces at its handshake: a TUI that says `cli` is never
//! elected the presenter of a reminder or a new-mail notification
//! (`CanonicalState::presenter`).
//!
//! # Process environment
//!
//! `Session::connect` reads the ambient `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR`, so the one row that opens a session sets them on
//! this process; it is the only row that reads the ambient environment, and
//! every child is given its own explicitly ([`mp_command`]), so the rows need
//! no lock between them.

mod support;

use std::process::{Child, Output, Stdio};
use std::time::Instant;

use serde_json::json;

use mailypoppins::tui::session::Session;

use support::parity::{mp_command, DaemonFixture, DEADLINE, TICK};
use support::sync_fixture;

/// How many copies of each command run at once.
const COPIES: usize = 4;

/// Read-only commands whose answer is fixed by the seeded fixture, so a copy
/// run beside others must print what a lone run prints.
const COMMANDS: [&[&str]; 3] = [
    &["account", "list"],
    &["list-messages"],
    &["list-messages", "--mailbox", "Team/Reports"],
];

/// Spawn `mp args` against `root` with its output piped, without waiting.
fn spawn(root: &std::path::Path, args: &[&str]) -> Child {
    mp_command(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn mp {args:?}: {e}"))
}

/// Wait for `child` under [`DEADLINE`], killing it rather than hanging.
fn finish(mut child: Child, args: &[&str]) -> Output {
    let start = Instant::now();
    while child.try_wait().expect("polling an mp run").is_none() {
        if start.elapsed() >= DEADLINE {
            let _ = child.kill();
            panic!("mp {args:?} did not exit within {DEADLINE:?}");
        }
        std::thread::sleep(TICK);
    }
    child.wait_with_output().expect("collecting an mp run")
}

/// Every command of [`COMMANDS`] run alone, one after the other: the oracle
/// each concurrent copy is compared with.
fn lone_runs(daemon: &DaemonFixture) -> Vec<Output> {
    let lone: Vec<Output> = COMMANDS.iter().map(|args| daemon.mp(args)).collect();
    for (args, out) in COMMANDS.iter().zip(&lone) {
        assert!(
            out.status.success() && !out.stdout.is_empty(),
            "mp {args:?} alone: {:?}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    lone
}

/// [`COPIES`] of every command of [`COMMANDS`], all started before any is
/// waited on.
fn spawn_all(daemon: &DaemonFixture) -> Vec<Child> {
    let mut running = Vec::new();
    for _ in 0..COPIES {
        for args in COMMANDS {
            running.push(spawn(daemon.root(), args));
        }
    }
    running
}

/// Each concurrent run exits as the lone one did and prints the same bytes.
fn check(running: Vec<Child>, lone: &[Output]) {
    for (index, child) in running.into_iter().enumerate() {
        let args = COMMANDS[index % COMMANDS.len()];
        let out = finish(child, args);
        let alone = &lone[index % COMMANDS.len()];
        assert_eq!(
            out.status.code(),
            alone.status.code(),
            "mp {args:?} beside others: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&alone.stdout),
            "mp {args:?} beside others printed something else"
        );
    }
}

/// Twelve `mp` processes at once against one daemon each answer exactly as
/// they do alone, and the daemon they shared is still the one that started.
#[test]
fn concurrent_cli_commands_each_answer_as_they_do_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    sync_fixture::seed(tmp.path());
    let daemon = DaemonFixture::start(tmp.path());

    let lone = lone_runs(&daemon);
    check(spawn_all(&daemon), &lone);
    assert_eq!(
        support::parity::daemon_pid_file(tmp.path()),
        Some(daemon.pid()),
        "the clients shared the running daemon rather than starting another"
    );

    daemon.stop();
}

/// A TUI session announces itself as a TUI, and keeps answering while CLI
/// commands run beside it, each of which answers as it does alone.
#[test]
fn a_tui_session_and_cli_commands_share_one_daemon_at_the_same_instant() {
    let tmp = tempfile::tempdir().expect("tempdir");
    sync_fixture::seed(tmp.path());
    let root = tmp.path().to_path_buf();
    // `Session::connect` reads these; see the module header.
    std::env::set_var("HOME", &root);
    std::env::set_var("MAILYPOPPINS_DATA_DIR", &root);
    std::env::set_var("MAILYPOPPINS_CONFIG_DIR", &root);
    std::env::set_var("MAILYPOPPINS_DAEMON_AUTOSTART", "0");

    let daemon = DaemonFixture::start(&root);
    let mut session = Session::connect(mailypoppins::daemon::client::tui_connector())
        .expect("the session thread comes up");
    let accounts = session
        .call("account.list", json!({}))
        .expect("a live session answers");

    let logs = session
        .call("diagnostic.logs", json!({"lines": 500}))
        .expect("the daemon reads its own log");
    let logs = logs.to_string();
    assert!(
        logs.contains("a tui client completed initialize"),
        "the TUI's handshake announces a TUI, so the daemon can elect it a presenter: {logs}"
    );

    let lone = lone_runs(&daemon);
    let mut running = spawn_all(&daemon);
    // The session calls while the commands are in flight, and stops once they
    // have all exited, so the two overlap rather than take turns.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = session
            .call("account.list", json!({}))
            .expect("the session answers while commands run beside it");
        assert_eq!(answer, accounts, "and answers what it answered alone");
        let in_flight = running
            .iter_mut()
            .any(|child| child.try_wait().expect("polling an mp run").is_none());
        if !in_flight {
            break;
        }
        assert!(Instant::now() < deadline, "the commands did not finish");
    }
    check(running, &lone);

    // The plan's runtime assertion (PERSO-223): the client process, here the
    // one holding the TUI session, has no `store.lock` open. The daemon holds
    // every account's, so taking one would fail rather than prove anything;
    // the open descriptors are what a client that reached for it would show.
    if let Ok(fds) = std::fs::read_dir("/proc/self/fd") {
        let held: Vec<_> = fds
            .filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
            .filter(|target| target.ends_with("store.lock"))
            .collect();
        assert!(held.is_empty(), "the client holds an engine lock: {held:?}");
    }

    session.close();
    daemon.stop();
}
