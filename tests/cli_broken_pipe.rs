//! A closed stdout ends any `mp` command quietly, the way it ends `cat`:
//! `mp list-messages --json | head` stops when `head` has read its lines,
//! with no panic message on stderr and the SIGPIPE status a shell reports as
//! 141. One rule in `main`, so every subcommand gets it.

mod support;

use std::os::unix::process::ExitStatusExt;
use std::process::{Output, Stdio};

use tempfile::TempDir;

use support::mutation_fixture as fixture;
use support::parity::{mp_command, DaemonFixture, REQUIRE_ENV};

/// `mp <args>` with its stdout a pipe whose reading end is closed before the
/// command writes a byte.
/// `routed` asks the command to prove it went through the daemon.
fn into_a_closed_pipe(root: &std::path::Path, args: &[&str], routed: bool) -> Output {
    let mut command = mp_command(root);
    if routed {
        command.env(REQUIRE_ENV, "1");
    }
    let mut child = command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn `mp {}`: {e}", args.join(" ")));
    drop(child.stdout.take());
    child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("wait for `mp {}`: {e}", args.join(" ")))
}

fn assert_ended_by_the_closed_pipe(args: &[&str], out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "`mp {}` reported the closed pipe:\n{stderr}",
        args.join(" ")
    );
    assert_eq!(
        out.status.signal(),
        Some(libc::SIGPIPE),
        "`mp {}` ends as a closed pipe ends a command: {:?}\n{stderr}",
        args.join(" "),
        out.status
    );
}

#[test]
fn a_closed_stdout_ends_a_command_quietly() {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());
    let _daemon = DaemonFixture::start(tmp.path());
    for args in [
        &["list-messages", "--json"][..],
        &["list-messages"][..],
        &[
            "show",
            &fixture::message_selector(fixture::ACCOUNT, "inbox", fixture::SHARED),
        ][..],
    ] {
        let out = into_a_closed_pipe(tmp.path(), args, true);
        assert_ended_by_the_closed_pipe(args, &out);
    }
}

/// A command that never reaches the daemon ends the same way.
#[test]
fn a_closed_stdout_ends_an_offline_command_the_same_way() {
    let tmp = TempDir::new().expect("a temporary root");
    let args = ["completions", "zsh"];
    let out = into_a_closed_pipe(tmp.path(), &args, false);
    assert_ended_by_the_closed_pipe(&args, &out);
}
