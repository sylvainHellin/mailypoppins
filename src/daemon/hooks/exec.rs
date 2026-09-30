//! Running a hook's command (#0135).
//!
//! No shell: `exec` is an argv, so nothing in a subject or an address can
//! become syntax. The message goes in as JSON on stdin, a few `MP_HOOK_*`
//! variables name it, the working directory is the user's home, and the
//! command is killed when its timeout runs out. What it wrote on stdout and
//! stderr is kept, bounded, for the log and for `mp hooks replay`.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;

/// How much of each output stream is kept: the tail, where an error usually is.
const OUTPUT_TAIL: usize = 4096;

/// What one run did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutcome {
    /// `exit 0`, `exit 3`, `killed by signal 9`, `timed out after 60s`, or
    /// `did not start: <why>`.
    pub outcome: String,
    /// True for exit 0 only.
    pub ok: bool,
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
}

/// The variables a hook's command sees besides the daemon's own environment.
pub struct HookEnv<'a> {
    pub hook: &'a str,
    pub account: &'a str,
    pub mailbox: &'a str,
    pub message_id: &'a str,
    pub selector: &'a str,
    /// The directory holding `message.json`, `message.eml`, `message.md` and
    /// `attachments/`, removed once the command exits.
    pub dir: &'a Path,
    pub replay: bool,
}

/// Run `argv` with `stdin` on its standard input, for at most `timeout`.
pub async fn run(
    argv: &[String],
    stdin: &[u8],
    env: &HookEnv<'_>,
    timeout: Duration,
) -> RunOutcome {
    let started = Instant::now();
    let failed = |why: String| RunOutcome {
        outcome: format!("did not start: {why}"),
        ok: false,
        duration: started.elapsed(),
        stdout: String::new(),
        stderr: String::new(),
    };
    let Some((program, args)) = argv.split_first() else {
        return failed("exec is empty".to_string());
    };
    let program = shellexpand::tilde(program).into_owned();
    let mut command = tokio::process::Command::new(&program);
    command
        .args(args)
        .env("MP_HOOK_NAME", env.hook)
        .env("MP_HOOK_ACCOUNT", env.account)
        .env("MP_HOOK_MAILBOX", env.mailbox)
        .env("MP_HOOK_MESSAGE_ID", env.message_id)
        .env("MP_HOOK_SELECTOR", env.selector)
        .env("MP_HOOK_DIR", env.dir)
        .env("MP_HOOK_REPLAY", if env.replay { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A timed-out run is dropped mid-wait, and the drop is what kills it.
        .kill_on_drop(true);
    if let Some(home) = dirs::home_dir() {
        command.current_dir(home);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return failed(format!("{program}: {e}")),
    };
    if let Some(mut pipe) = child.stdin.take() {
        // A command that never reads its stdin closes the pipe early; that is
        // its business, not a failed run.
        let _ = pipe.write_all(stdin).await;
        drop(pipe);
    }
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let status = output.status;
            let outcome = match status.code() {
                Some(code) => format!("exit {code}"),
                None => {
                    use std::os::unix::process::ExitStatusExt;
                    format!("killed by signal {}", status.signal().unwrap_or_default())
                }
            };
            RunOutcome {
                outcome,
                ok: status.success(),
                duration: started.elapsed(),
                stdout: tail(&output.stdout),
                stderr: tail(&output.stderr),
            }
        }
        Ok(Err(e)) => RunOutcome {
            outcome: format!("could not be waited for: {e}"),
            ok: false,
            duration: started.elapsed(),
            stdout: String::new(),
            stderr: String::new(),
        },
        Err(_) => RunOutcome {
            outcome: format!("timed out after {}s", timeout.as_secs()),
            ok: false,
            duration: started.elapsed(),
            stdout: String::new(),
            stderr: String::new(),
        },
    }
}

/// The last [`OUTPUT_TAIL`] bytes of a stream, as text.
fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end();
    if text.len() <= OUTPUT_TAIL {
        return text.to_string();
    }
    let mut start = text.len() - OUTPUT_TAIL;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &Path) -> HookEnv<'_> {
        HookEnv {
            hook: "h",
            account: "alpha",
            mailbox: "inbox",
            message_id: "<m@x>",
            selector: "mp://alpha/inbox/m@x",
            dir,
            replay: false,
        }
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()]
    }

    #[tokio::test]
    async fn stdin_and_the_environment_reach_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &sh("read line; echo \"$line $MP_HOOK_NAME $MP_HOOK_ACCOUNT $MP_HOOK_MESSAGE_ID\"; echo oops >&2"),
            b"{\"x\":1}\n",
            &env(dir.path()),
            Duration::from_secs(10),
        )
        .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.outcome, "exit 0");
        assert_eq!(out.stdout, "{\"x\":1} h alpha <m@x>");
        assert_eq!(out.stderr, "oops");
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &sh("exit 3"),
            b"",
            &env(dir.path()),
            Duration::from_secs(10),
        )
        .await;
        assert!(!out.ok);
        assert_eq!(out.outcome, "exit 3");
    }

    #[tokio::test]
    async fn a_command_past_its_timeout_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &sh("sleep 30"),
            b"",
            &env(dir.path()),
            Duration::from_millis(300),
        )
        .await;
        assert!(!out.ok);
        assert!(out.outcome.starts_with("timed out"), "{out:?}");
        assert!(out.duration < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_missing_program_did_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &["/nonexistent/hook".to_string()],
            b"",
            &env(dir.path()),
            Duration::from_secs(1),
        )
        .await;
        assert!(!out.ok);
        assert!(out.outcome.starts_with("did not start"), "{out:?}");
    }

    #[test]
    fn the_tail_keeps_the_end_of_a_long_stream() {
        let long = "a".repeat(OUTPUT_TAIL * 2) + "END";
        let kept = tail(long.as_bytes());
        assert!(kept.ends_with("END"));
        assert!(kept.len() <= OUTPUT_TAIL + 3);
    }
}
