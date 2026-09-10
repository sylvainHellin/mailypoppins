//! The parity harness, tested against itself (#0123, plan unit P4-U1).
//!
//! Phase 4 migrates command after command onto the daemon, and every one of
//! those slices is gated on byte parity with the pre-daemon binary. The
//! comparison is only worth something if the harness that makes it is known to
//! be honest, so this file pins three properties of `tests/support/parity.rs`
//! before any command has moved:
//!
//! 1. **It agrees where it must.** Three commands that Phase 4 has not touched
//!    (`mp --version`, `mp config path`, `mp list-mailboxes`) answer
//!    identically from the current binary and from the `pre-daemon` oracle,
//!    with a daemon running beside them. Nothing routes to the daemon yet, so
//!    the diff is empty by construction, which is exactly the point: a harness
//!    that could not produce an empty diff here would report a difference for
//!    every slice and prove nothing about any of them.
//! 2. **It disagrees when it must, readably.** A doctored [`Output`] compared
//!    against a real one must fail, and the failure must name the stream, carry
//!    both values and show a diff. A harness whose failure says only
//!    `assertion failed` costs the reader the whole slice.
//! 3. **It leaks no daemon.** Neither an explicit
//!    [`DaemonFixture::stop`] nor a `Drop` after a panic may leave a process
//!    behind, because the next test's fixture would bind against a socket the
//!    previous one still owns.
//!
//! # Why these three commands
//!
//! The plan (P4-U2) fixes a no-daemon list - `mp daemon *`, `mp dump-keys`,
//! `mp --help`, `mp --version`, `mp config path` - so two of these three stay
//! oracle-comparable for the whole migration. `mp list-mailboxes` is the third
//! kind: a store-reading command that Phase 4 does migrate, sampled here while
//! it still runs in process, so the same assertion re-run after the migration
//! is a real gate rather than a tautology.
//!
//! Their output on an empty sandbox is fixture-relative (it names the config
//! file that is not there) and deterministic, and both binaries are told the
//! same root, so every path in it is the same string on both sides. The version
//! string is `mailypoppins 0.9.0` on both, because the tag and the working tree
//! carry the same `version` in `Cargo.toml`; the two commands that would embed
//! a build-specific string, `mp --help` and `mp dump-keys`, are covered by
//! `tests/cli_help_snapshot.rs` and `docs/baselines/pre-daemon/tui-keys.json`
//! instead and are not duplicated here.

mod support;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

use support::parity::{
    assert_byte_identical, oracle, oracle_bin, process_is_alive, socket_path, DaemonFixture,
};

/// The commands compared in the agreement test, with the name each assertion
/// reports.
const UNMIGRATED: [&[&str]; 3] = [&["--version"], &["config", "path"], &["list-mailboxes"]];

fn root() -> TempDir {
    TempDir::new().expect("a temporary parity root")
}

/// Every `mp daemon run` this host can see, as `pgrep -af` reports it: one
/// line per process, pid first.
fn daemon_pgrep_lines() -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .args(["-af", "[m]p daemon"])
        .output()
        .expect("run pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// The `pgrep` line for `pid`, if the process list still holds one.
///
/// The assertion is scoped to one pid rather than to "the list is empty"
/// because it is neither: this binary runs its tests on several threads, so a
/// sibling test's fixture is legitimately in the list while this one checks,
/// and so is a daemon the developer started by hand. Neither is a leak this
/// fixture caused, and a set difference taken across two `pgrep` calls races
/// the siblings instead.
fn pgrep_line_for(pid: u32) -> Option<String> {
    daemon_pgrep_lines().into_iter().find(|line| {
        line.split_whitespace()
            .next()
            .and_then(|first| first.parse::<u32>().ok())
            == Some(pid)
    })
}

// ---------------------------------------------------------------------------
// 1. The harness agrees where it must
// ---------------------------------------------------------------------------

#[test]
fn the_oracle_binary_exists_and_reports_the_same_version_as_the_build_under_test() {
    let path = oracle_bin();
    assert!(
        path.is_file(),
        "the oracle resolved to {}, which is not a file",
        path.display()
    );

    let tmp = root();
    let theirs = oracle(&["--version"], tmp.path());
    assert!(
        theirs.status.success(),
        "the oracle at {} could not run `mp --version`: {}",
        path.display(),
        String::from_utf8_lossy(&theirs.stderr)
    );
    let rendered = String::from_utf8_lossy(&theirs.stdout).trim().to_string();
    assert_eq!(
        rendered,
        format!("mailypoppins {}", env!("CARGO_PKG_VERSION")),
        "the `pre-daemon` tag and this working tree carry the same package version, which is what \
         lets `mp --version` be one of the parity commands; if this ever fails, the version was \
         bumped and `--version` must leave the UNMIGRATED list rather than be normalised away"
    );
}

#[test]
fn an_unmigrated_command_answers_identically_with_a_daemon_running() {
    let tmp = root();
    let fixture = DaemonFixture::start(tmp.path());
    assert!(
        socket_path(tmp.path()).exists(),
        "the fixture reported ready, so the socket it waited on is there"
    );

    for args in UNMIGRATED {
        let ours = fixture.mp(args);
        let theirs = oracle(args, tmp.path());
        let label = args.join(" ");
        let compared = catch_unwind(AssertUnwindSafe(|| assert_byte_identical(&ours, &theirs)));
        assert!(
            compared.is_ok(),
            "`mp {label}` has not been migrated, so the daemon-era binary and the pre-daemon \
             oracle must still answer identically: {}",
            message_of(compared.unwrap_err())
        );
    }

    fixture.stop();
}

#[test]
fn the_two_binaries_see_the_same_root_so_paths_are_not_a_false_difference() {
    let tmp = root();
    let ours = DaemonFixture::start(tmp.path());
    let mine = ours.mp(&["config", "path"]);
    let theirs = oracle(&["config", "path"], tmp.path());
    ours.stop();

    let rendered = String::from_utf8_lossy(&mine.stdout);
    let expected = tmp.path().join("config.toml");
    assert!(
        rendered.contains(&expected.display().to_string()),
        "`mp config path` prints a path inside the fixture root, which is what makes the \
         comparison literal rather than normalised: wanted {}, got {rendered}",
        expected.display()
    );
    assert_byte_identical(&mine, &theirs);
}

// ---------------------------------------------------------------------------
// 2. The harness disagrees when it must, readably
// ---------------------------------------------------------------------------

fn message_of(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "<a panic payload that is neither &str nor String>".to_string()
}

fn expect_mismatch(left: &Output, right: &Output) -> String {
    let outcome = catch_unwind(AssertUnwindSafe(|| assert_byte_identical(left, right)));
    let payload = outcome.expect_err("two different outputs must not compare byte-identical");
    message_of(payload)
}

fn doctored(base: &Output, stdout: &str) -> Output {
    Output {
        status: base.status,
        stdout: stdout.as_bytes().to_vec(),
        stderr: base.stderr.clone(),
    }
}

#[test]
fn an_injected_stdout_difference_names_the_stream_and_shows_a_diff() {
    let tmp = root();
    let real = oracle(&["--help"], tmp.path());
    let text = String::from_utf8(real.stdout.clone()).expect("`mp --help` is UTF-8");

    // One line changed, deep inside a long screen: the case a bare
    // `assert_eq!` renders as two unreadable walls of text.
    let injected = text.replacen("Commands:", "Commandz:", 1);
    assert_ne!(injected, text, "the doctoring actually changed something");
    let message = expect_mismatch(&real, &doctored(&real, &injected));

    for wanted in [
        "not byte-identical",
        "stdout differs",
        "diff (-left +right)",
        "-Commands:",
        "+Commandz:",
    ] {
        assert!(
            message.contains(wanted),
            "the failure must be readable without rerunning anything: no {wanted:?} in\n{message}"
        );
    }
    assert!(
        !message.contains("stderr differs"),
        "only the stream that differs is reported: \n{message}"
    );
}

#[test]
fn an_injected_stderr_difference_is_reported_separately_from_stdout() {
    let tmp = root();
    let real = oracle(&["--version"], tmp.path());
    let doctored = Output {
        status: real.status,
        stdout: real.stdout.clone(),
        stderr: b"a warning the other build never printed\n".to_vec(),
    };
    let message = expect_mismatch(&real, &doctored);

    assert!(
        message.contains("stderr differs"),
        "the stream is named: \n{message}"
    );
    assert!(
        message.contains("a warning the other build never printed"),
        "and the value that differs is in the message: \n{message}"
    );
    assert!(
        !message.contains("stdout differs"),
        "stdout matched, so it is not reported: \n{message}"
    );
}

#[test]
fn an_injected_exit_code_difference_is_reported_even_when_both_streams_match() {
    let tmp = root();
    let zero = oracle(&["--version"], tmp.path());
    let nonzero = oracle(&["list-mailboxes"], tmp.path());
    assert_eq!(zero.status.code(), Some(0));
    assert_eq!(
        nonzero.status.code(),
        Some(1),
        "`mp list-mailboxes` on an empty sandbox fails, which is what gives this test a real \
         nonzero status rather than a synthesised one"
    );

    let same_streams = Output {
        status: nonzero.status,
        stdout: zero.stdout.clone(),
        stderr: zero.stderr.clone(),
    };
    let message = expect_mismatch(&zero, &same_streams);

    assert!(
        message.contains("exit code differs"),
        "the exit code is compared on its own: \n{message}"
    );
    assert!(
        message.contains("left  = 0") && message.contains("right = 1"),
        "and both codes are in the message: \n{message}"
    );
}

#[test]
fn all_three_streams_differing_are_reported_together() {
    let tmp = root();
    let real = oracle(&["--version"], tmp.path());
    let other = oracle(&["list-mailboxes"], tmp.path());
    let message = expect_mismatch(&real, &other);
    assert!(
        message.contains("(3 of 3 differ)"),
        "a caller fixes every difference at once rather than one rerun at a time: \n{message}"
    );
}

// ---------------------------------------------------------------------------
// 3. The fixture leaks no daemon
// ---------------------------------------------------------------------------

#[test]
fn stopping_the_fixture_leaves_no_daemon_behind() {
    let tmp = root();
    let fixture = DaemonFixture::start(tmp.path());
    let pid = fixture.pid();
    assert!(
        process_is_alive(pid),
        "the fixture reported ready, so its daemon is running"
    );
    assert!(
        pgrep_line_for(pid).is_some(),
        "and `pgrep -af '[m]p daemon'` lists it, which is what makes the assertion after the \
         stop mean something"
    );

    fixture.stop();

    assert!(
        !process_is_alive(pid),
        "`stop` waits for the daemon to be gone rather than for the kill to return"
    );
    assert_eq!(
        pgrep_line_for(pid),
        None,
        "and the process list no longer holds it"
    );
}

#[test]
fn dropping_the_fixture_without_stopping_it_leaves_no_daemon_behind() {
    let tmp = root();
    let pid = {
        let fixture = DaemonFixture::start(tmp.path());
        assert!(pgrep_line_for(fixture.pid()).is_some(), "the daemon is up");
        fixture.pid()
        // dropped here, as it would be if an assertion above had panicked
    };

    // `Drop` reaps the child, so the pid is free the instant the drop returns.
    assert!(
        !process_is_alive(pid),
        "a test that never called `stop` must not leak a daemon: Drop kills and reaps"
    );
    assert_eq!(pgrep_line_for(pid), None, "and nothing is left in the list");
}

#[test]
fn a_panic_inside_a_test_body_still_takes_the_daemon_down() {
    let tmp = root();
    let pid_cell = std::sync::Arc::new(std::sync::Mutex::new(0u32));
    let seen = std::sync::Arc::clone(&pid_cell);

    let outcome = catch_unwind(AssertUnwindSafe(move || {
        let fixture = DaemonFixture::start(tmp.path());
        *seen.lock().expect("the pid cell") = fixture.pid();
        panic!("the failure a real assertion would raise");
    }));
    assert!(outcome.is_err(), "the body panicked, as it was told to");

    let pid = *pid_cell.lock().expect("the pid cell");
    assert!(pid > 1, "the fixture started before the panic");
    assert!(
        !process_is_alive(pid),
        "unwinding ran the fixture's Drop, which killed the daemon"
    );
    assert_eq!(
        pgrep_line_for(pid),
        None,
        "and nothing this test started is still listed"
    );
}

#[test]
fn a_second_fixture_can_bind_the_same_root_after_the_first_has_stopped() {
    let tmp = root();
    let first = DaemonFixture::start(tmp.path());
    let first_pid = first.pid();
    first.stop();

    let second = DaemonFixture::start(tmp.path());
    assert_ne!(second.pid(), first_pid, "a second daemon, not the first");
    assert!(
        socket_path(tmp.path()).exists(),
        "which bound the same socket path the first one unlinked on its way out"
    );
    second.stop();
}

#[test]
fn the_socket_lives_under_the_root_the_plan_fixes() {
    let tmp = root();
    assert_eq!(
        socket_path(tmp.path()),
        Path::new(tmp.path()).join("runtime").join("daemon.sock"),
        "the runtime layout plan section 3.0 fixes, which tests may hard-code"
    );
}
