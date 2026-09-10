//! On-demand daemon start, the no-daemon list, and the client's path rule
//! (#0123, plan unit P4-U2).
//!
//! Phase 4 makes the daemon the answer to almost every command, and a design
//! that asked the user to start it first would be a worse tool than the one it
//! replaces. So a client that finds nothing listening starts one, waits a
//! bounded time, and only then gives up. Three properties have to hold, and
//! this file pins each of them:
//!
//! 1. **It starts one, and exactly one.** A normal client against a cold root
//!    ends with a daemon serving that root, which the test then stops the way
//!    a user would.
//! 2. **It gives up loudly.** With `MAILYPOPPINS_DAEMON_AUTOSTART=0`, or with
//!    a daemon that cannot become ready, the run exits 4 and the message
//!    carries the socket path and the literal `mp daemon run`. An exit code on
//!    its own would leave the user with nothing to type next.
//! 3. **It leaves the no-daemon list alone.** `mp daemon *`, `mp dump-keys`,
//!    `mp --help`, `mp --version` and `mp config path` run with nothing
//!    listening and start nothing, because each of them either *is* the
//!    lifecycle surface or answers from a compiled-in structure.
//!
//! # Why `mp config init` is not on that list
//!
//! It writes `config.toml`, and the daemon owns configuration: an init that
//! wrote the file behind a running daemon's back would leave the daemon
//! describing a state that no longer exists until something invalidated it.
//! The plan calls this out by name, and the assertion here is against the pure
//! function rather than against a run, because `mp config init` prompts and a
//! test that answered its prompts would be testing the prompts.
//!
//! # The routing hook
//!
//! `MAILYPOPPINS_DAEMON_REQUIRE=1` makes a command that answered from its own
//! process exit nonzero saying so. It exists so that a parity assertion, once
//! a slice migrates a command, proves the command *routed* rather than proving
//! two in-process runs agree with each other. It is checked at the single
//! client entry point, so it cannot drift away from what actually routes.
//!
//! # Why the daemon is started from `/`
//!
//! A long-lived daemon is started from wherever the machine happened to start
//! it: a login agent's cwd, `/`, a directory that has since been deleted. If
//! any relative path reached it, the answer would depend on that. The client
//! is the process with a meaningful working directory, so it absolutises every
//! user-supplied path before the path crosses the socket, and this file
//! asserts that rule at the seam (`mp save`'s destination) and end to end
//! where it can.
//!
//! A draft's `attachments:` entries are the one path that cannot follow that
//! rule: no attachment path crosses the wire (the client sends a selector), so
//! there is nothing for the client to rewrite. They are anchored to the draft
//! file's own directory instead, which is the one location both processes
//! agree on, and that seam is asserted here too.
//!
//! # Process hygiene
//!
//! Every daemon this file causes to exist is stopped before the test returns,
//! through `mp daemon stop` rather than a signal: an auto-started daemon
//! detaches into its own session and is nobody's child, which is exactly what
//! keeps it alive after the client that wanted it has exited.

mod support;

use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tempfile::TempDir;

use mailypoppins::daemon::client::{absolutise_in, needs_daemon, AUTOSTART_TIMEOUT};
use mailypoppins::send::resolve_attachment_paths;

use support::parity::{
    daemon_is_listening, daemon_pid_file, mp_autostart, mp_no_daemon, pgrep_line_for, socket_path,
    stop_daemon, DaemonFixture, EXIT_UNAVAILABLE,
};

/// Long enough for a real daemon to bind on this tree (it is well under a
/// second), short enough that a test which expects a failure does not pay the
/// five-second default.
const READY_BUDGET_MS: u64 = 8_000;

/// The bound a failing auto-start is given, and the number the elapsed-time
/// assertion is written against.
const FAIL_BUDGET_MS: u64 = 800;

fn root() -> TempDir {
    TempDir::new().expect("a temporary auto-start root")
}

/// The command every auto-start case runs: routed today, harmless on an empty
/// config, and it exits 0 with `No accounts configured`.
const ROUTED: [&str; 3] = ["--daemon", "account", "list"];

// ---------------------------------------------------------------------------
// 1. It starts one
// ---------------------------------------------------------------------------

#[test]
fn a_client_with_no_daemon_starts_one_and_the_command_succeeds() {
    let tmp = root();
    assert!(
        !daemon_is_listening(tmp.path()),
        "the root is cold, which is what makes the assertion below mean something"
    );

    let out = mp_autostart(&ROUTED, tmp.path(), READY_BUDGET_MS, None);

    assert_eq!(
        out.status.code(),
        Some(0),
        "the client started a daemon and got its answer\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "No accounts configured\n",
        "and the answer is the daemon's, not a fallback's"
    );
    assert!(
        daemon_is_listening(tmp.path()),
        "the daemon outlived the client that started it: it is detached, which is the point"
    );

    let pid = daemon_pid_file(tmp.path()).expect("the daemon wrote its pid file");
    assert!(
        pgrep_line_for(pid).is_some(),
        "and the process list holds exactly that pid"
    );

    let stopped = stop_daemon(tmp.path());
    assert_eq!(stopped.status.code(), Some(0), "`mp daemon stop` ends it");
    assert_eq!(
        pgrep_line_for(pid),
        None,
        "this test leaves no daemon behind"
    );
}

/// A second client against the same root finds the first client's daemon and
/// starts nothing: the auto-start is a repair, not a per-command cost.
#[test]
fn a_second_client_reuses_the_daemon_the_first_one_started() {
    let tmp = root();
    let first = mp_autostart(&ROUTED, tmp.path(), READY_BUDGET_MS, None);
    assert_eq!(first.status.code(), Some(0), "the first client started one");
    let pid = daemon_pid_file(tmp.path()).expect("a pid file");

    let second = mp_autostart(&ROUTED, tmp.path(), READY_BUDGET_MS, None);
    assert_eq!(second.status.code(), Some(0), "the second one is served");
    assert_eq!(
        daemon_pid_file(tmp.path()),
        Some(pid),
        "by the same daemon: a second one would have rewritten the pid file"
    );

    stop_daemon(tmp.path());
    assert_eq!(pgrep_line_for(pid), None, "and nothing is left running");
}

// ---------------------------------------------------------------------------
// 2. It gives up loudly
// ---------------------------------------------------------------------------

/// The diagnostic every exit-4 path owes the user: what to type, and where the
/// socket it could not reach is.
fn assert_exit_four_diagnostic(stderr: &str, root: &Path) {
    assert!(
        stderr.contains("mp daemon run"),
        "the message prints the exact command to run: {stderr}"
    );
    assert!(
        stderr.contains(&socket_path(root).display().to_string()),
        "and the socket it could not reach, {}: {stderr}",
        socket_path(root).display()
    );
}

#[test]
fn auto_start_turned_off_exits_four_naming_the_command_and_the_socket() {
    let tmp = root();
    let out = mp_no_daemon(&ROUTED, tmp.path());

    assert_eq!(
        out.status.code(),
        Some(EXIT_UNAVAILABLE),
        "a routed command with nothing listening and no auto-start exits {EXIT_UNAVAILABLE}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MAILYPOPPINS_DAEMON_AUTOSTART"),
        "and says which switch kept it from starting one: {stderr}"
    );
    assert_exit_four_diagnostic(&stderr, tmp.path());
    assert!(
        !socket_path(tmp.path()).exists(),
        "nothing was started, so nothing bound the socket"
    );
    assert!(
        out.stdout.is_empty(),
        "and it produced no answer of its own: there is no fallback"
    );
}

/// A spawn that cannot become ready: `MAILYPOPPINS_DAEMON_FAIL_START=1` makes
/// `mp daemon run` exit after logging and before the bind, which is the
/// deterministic version of a daemon that dies on startup.
#[test]
fn an_auto_start_whose_daemon_dies_exits_four() {
    let tmp = root();
    let out = mp_autostart_failing(&ROUTED, tmp.path(), FAIL_BUDGET_MS);

    assert_eq!(
        out.status.code(),
        Some(EXIT_UNAVAILABLE),
        "a daemon that cannot start is the same answer as one that is not there\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_exit_four_diagnostic(&stderr, tmp.path());
    assert!(
        !daemon_is_listening(tmp.path()),
        "and the corpse is not serving anything"
    );
}

/// The bound itself. With the start lock held by this test, the client's
/// auto-start takes the "another starter is ahead of us" branch and waits for
/// a readiness that never comes, which is the one case that spends the whole
/// budget rather than failing early on a dead child.
#[test]
fn an_auto_start_that_never_becomes_ready_gives_up_after_the_bound() {
    let tmp = root();
    let _held = StartLock::take(tmp.path());

    let started = Instant::now();
    let out = mp_autostart(&ROUTED, tmp.path(), FAIL_BUDGET_MS, None);
    let elapsed = started.elapsed();

    assert_eq!(
        out.status.code(),
        Some(EXIT_UNAVAILABLE),
        "the wait ended in exit {EXIT_UNAVAILABLE}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed.as_millis() >= u128::from(FAIL_BUDGET_MS),
        "it waited the bound it was given ({FAIL_BUDGET_MS} ms), not less: {elapsed:?}"
    );
    assert!(
        elapsed < AUTOSTART_TIMEOUT,
        "and the environment hook really shortened it: {elapsed:?} should be well under the \
         {AUTOSTART_TIMEOUT:?} default"
    );
    assert_exit_four_diagnostic(&String::from_utf8_lossy(&out.stderr), tmp.path());
    assert!(
        !daemon_is_listening(tmp.path()),
        "nothing was left serving the root"
    );
}

// ---------------------------------------------------------------------------
// 3. The no-daemon list
// ---------------------------------------------------------------------------

/// Every entry of the list, run against a cold root. None of them may need a
/// daemon and none of them may produce one.
///
/// `mp daemon status` stands for `mp daemon *`: it is the member of that family
/// that answers without side effects, where `run` would block and `start`
/// would do the very thing this test forbids.
#[test]
fn every_no_daemon_command_runs_with_nothing_listening_and_starts_nothing() {
    let tmp = root();

    for (args, expected) in [
        (vec!["daemon", "status"], Some(1)),
        (vec!["dump-keys"], Some(0)),
        (vec!["--help"], Some(0)),
        (vec!["--version"], Some(0)),
        (vec!["config", "path"], Some(0)),
    ] {
        // Auto-start is left **on**: the claim is that these commands never
        // reach the client entry point, not that a switch stopped them.
        let out = mp_autostart(&args, tmp.path(), FAIL_BUDGET_MS, None);
        assert_eq!(
            out.status.code(),
            expected,
            "`mp {}` answers on its own\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !socket_path(tmp.path()).exists(),
            "`mp {}` started a daemon, and it is on the no-daemon list",
            args.join(" ")
        );
        assert!(
            !daemon_is_listening(tmp.path()),
            "`mp {}` left something answering the socket",
            args.join(" ")
        );
        // Scoped to this root's pid file rather than to "`pgrep` is empty",
        // for the reason the harness gives: the suite runs on several threads
        // and a sibling test's daemon is legitimately in the process list. A
        // daemon started for this root would have written its pid here, and a
        // `pgrep` line for that pid is the confirmation.
        if let Some(pid) = daemon_pid_file(tmp.path()) {
            panic!(
                "`mp {}` started a daemon for this root: pid {pid}, {:?}",
                args.join(" "),
                pgrep_line_for(pid)
            );
        }
    }
}

/// `mp daemon status` with nothing running is the case that would deadlock if
/// the lifecycle family were ever put on the wrong side of the policy: a
/// status that auto-started a daemon in order to report that none was running
/// would always report one.
#[test]
fn daemon_status_reports_an_absence_instead_of_repairing_it() {
    let tmp = root();
    let out = mp_autostart(&["daemon", "status"], tmp.path(), FAIL_BUDGET_MS, None);

    assert_eq!(out.status.code(), Some(1), "no daemon is running");
    assert!(
        !daemon_is_listening(tmp.path()),
        "and the report did not become its own repair"
    );
}

#[test]
fn the_no_daemon_list_is_exactly_the_five_entries_the_plan_fixes() {
    for (command, sub) in [
        ("daemon", Some("run")),
        ("daemon", Some("start")),
        ("daemon", Some("status")),
        ("daemon", Some("stop")),
        ("daemon", Some("restart")),
        ("dump-keys", None),
        ("--help", None),
        ("--version", None),
        ("config", Some("path")),
    ] {
        assert!(
            !needs_daemon(Some(command), sub),
            "`mp {command} {sub:?}` is on the no-daemon list"
        );
    }
}

/// The exclusion the plan states in the same breath as the list. Asserted
/// against the pure function, because `mp config init` prompts: a test that
/// ran it would be answering a dialogue rather than checking a policy.
#[test]
fn config_init_is_not_on_the_no_daemon_list() {
    assert!(
        needs_daemon(Some("config"), Some("init")),
        "the daemon owns configuration, so an init runs as an ordinary client"
    );
    assert!(
        needs_daemon(Some("config"), Some("add-account")),
        "and so does adding an account"
    );
    assert!(
        !needs_daemon(Some("config"), Some("path")),
        "while its sibling that reads nothing stays off the daemon"
    );
}

// ---------------------------------------------------------------------------
// 4. The client owns path resolution
// ---------------------------------------------------------------------------

/// `mp save`'s destination default is the current directory, and the current
/// directory that counts is the client's. Asserted on the helper the command
/// applies, because `mp save` does not route until P4-U8.
#[test]
fn the_save_destination_is_anchored_to_the_clients_working_directory() {
    let cwd = Path::new("/home/user/mail");
    assert_eq!(
        absolutise_in(Path::new("."), cwd),
        PathBuf::from("/home/user/mail/."),
        "the `-o`-less default resolves where the user is standing, not where the daemon was \
         started"
    );
    assert_eq!(
        absolutise_in(Path::new("out/attachments"), cwd),
        PathBuf::from("/home/user/mail/out/attachments"),
        "and so does a relative -o"
    );
    assert_eq!(
        absolutise_in(Path::new("/srv/shared"), cwd),
        PathBuf::from("/srv/shared"),
        "an absolute -o is left exactly as typed"
    );
}

/// A draft's `attachments:` entries resolve against the draft file's own
/// directory, and leave it as absolute paths.
///
/// The anchor is explicit rather than read from `current_dir()`, because every
/// send runs inside the daemon now and the daemon's cwd is nobody's choice.
/// This is the accepted divergence from the pre-daemon binary, which anchored
/// to the sender's cwd; see `docs/tickets/0123-cli-cutover.md`.
#[test]
fn resolve_attachment_paths_yields_absolute_paths_for_relative_entries() {
    let dir = root();
    let file = dir.path().join("report.pdf");
    fs::write(&file, b"%PDF-1.4").expect("write the attachment");

    let resolved =
        resolve_attachment_paths(&["report.pdf".to_string()], dir.path()).expect("resolve");
    assert_eq!(
        resolved,
        vec![file.clone()],
        "a relative entry is anchored to the draft's directory and comes back absolute"
    );

    let absolute = resolve_attachment_paths(&[file.display().to_string()], Path::new("/nowhere"))
        .expect("resolve");
    assert_eq!(
        absolute,
        vec![file.clone()],
        "an absolute entry is untouched, whatever the anchor"
    );

    // A folder entry expands to its files, and every one of them is absolute
    // too: the expansion happens after the anchoring, not instead of it.
    let expanded = resolve_attachment_paths(&[dir.path().display().to_string()], Path::new("/"))
        .expect("resolve");
    assert_eq!(expanded, vec![file]);
    assert!(
        expanded.iter().all(|p| p.is_absolute()),
        "every expanded path is absolute: {expanded:?}"
    );
}

/// The end-to-end version of the same rule: a daemon started from `/` and a
/// client standing in a temp directory, with `mp save` writing into the
/// client's directory rather than into the daemon's.
///
/// Live from P4-U8, which migrated `mp save` onto the daemon and gave this
/// root the seed it was missing: the fixture holds `bericht@example.com` with
/// two attachments, and the command that fetches them now crosses the socket.
#[test]
fn mp_save_writes_into_the_clients_cwd_with_the_daemon_started_from_root() {
    let tmp = root();
    let standing_in = root();
    support::mutation_fixture::seed(tmp.path());
    let fixture = DaemonFixture::start_in(tmp.path(), Some(Path::new("/")));

    let out = fixture.mp_in(
        standing_in.path(),
        &["save", "mp://alpha/inbox/bericht@example.com"],
    );
    fixture.stop();

    assert_eq!(out.status.code(), Some(0));
    let written: Vec<_> = fs::read_dir(standing_in.path())
        .expect("read the client's directory")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert!(
        !written.is_empty(),
        "the attachment landed where the client was standing, not where the daemon was started"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// [`mp_autostart`] with `MAILYPOPPINS_DAEMON_FAIL_START=1`, so the daemon the
/// client spawns exits before it binds.
fn mp_autostart_failing(args: &[&str], root: &Path, budget_ms: u64) -> std::process::Output {
    let mut cmd = std::process::Command::new(support::parity::MP);
    support::parity::sandbox_env(&mut cmd, root);
    cmd.env(
        support::parity::AUTOSTART_TIMEOUT_ENV,
        budget_ms.to_string(),
    )
    .env("MAILYPOPPINS_DAEMON_FAIL_START", "1")
    .stdin(std::process::Stdio::null())
    .args(args)
    .output()
    .unwrap_or_else(|e| panic!("run `mp {}` with a failing start: {e}", args.join(" ")))
}

/// The daemon start lock, held for the life of the value.
///
/// Holding it from a test is how "a start that never becomes ready" is
/// produced without a daemon that hangs: the client's auto-start cannot take
/// the lock, concludes another starter is ahead of it, and waits out its bound
/// for a readiness nobody will deliver.
struct StartLock(#[allow(dead_code)] fs::File);

impl StartLock {
    fn take(root: &Path) -> Self {
        let dir = root.join("runtime");
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        let path = dir.join("daemon.start.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        // Safety: `flock` on a descriptor this process owns; the kernel
        // releases it when the file closes, however this process ends.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(
            rc,
            0,
            "this test must be the one holding {}",
            path.display()
        );
        StartLock(file)
    }
}
