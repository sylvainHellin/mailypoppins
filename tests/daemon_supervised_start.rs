//! `mp daemon start` and `mp daemon restart` under an installed login service
//! (PERSO-109).
//!
//! With the systemd unit for this data directory installed, both commands ask
//! `systemctl --user start mailypoppins.service` for the daemon instead of
//! spawning a detached one, which would leave the unit dead beside a daemon
//! systemd does not own; `mp daemon stop` stays the socket's, which the unit
//! records as a clean stop.
//!
//! No real service manager is reached. `MAILYPOPPINS_DAEMON_SERVICE_OS=linux`
//! selects the systemd half on any host, the unit is written by
//! `mp daemon install-service` under the dry-run hook into a sandbox
//! `XDG_CONFIG_HOME`, and the child's `PATH` is a sandbox directory holding a
//! fake `systemctl` that records its argv and, on `start`, does what systemd
//! would: runs `mp daemon run` in the background with the unit's environment.
//! `XDG_RUNTIME_DIR` is a sandbox directory whose `systemd/private` is a bound
//! Unix socket, which is all the probe for a reachable user manager reads.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// The systemd unit's name, as every `systemctl` line carries it.
const UNIT_NAME: &str = "mailypoppins.service";

struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "xdg", "bin", "run/systemd"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        // The socket file outlives the listener; only its type is probed.
        UnixListener::bind(root.path().join("run/systemd/private"))
            .expect("bind the sandbox user-manager socket");
        Self { root }
    }

    fn dir(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn pid_file(&self) -> PathBuf {
        self.dir("data").join("runtime").join("daemon.pid")
    }

    fn recorded_pid(&self) -> i32 {
        let raw = fs::read_to_string(self.pid_file()).expect("read daemon.pid");
        raw.trim().parse().expect("daemon.pid holds a pid")
    }

    /// An `mp` that sees the systemd half, the sandbox unit directory and the
    /// fake `systemctl` and nothing else on `PATH`.
    fn mp(&self, args: &[&str]) -> Output {
        Command::new(MP)
            .args(args)
            .env("HOME", self.dir("home"))
            .env("MAILYPOPPINS_DATA_DIR", self.dir("data"))
            .env("MAILYPOPPINS_CONFIG_DIR", self.dir("config"))
            .env("XDG_CONFIG_HOME", self.dir("xdg"))
            .env("PATH", self.dir("bin"))
            .env("XDG_RUNTIME_DIR", self.dir("run"))
            .env("MAILYPOPPINS_DAEMON_SERVICE_OS", "linux")
            .env_remove("MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .output()
            .unwrap_or_else(|e| panic!("run mp {args:?}: {e}"))
    }

    /// A fake `systemctl` that appends its argv to `bin/systemctl.log` and, on
    /// `start`, runs `mp daemon run` detached, as a `Type=simple` unit would.
    fn fake_systemctl(&self) {
        self.write_systemctl(&format!(
            "if [ \"$2\" = start ]; then\n\
             \x20 '{MP}' daemon run </dev/null >/dev/null 2>&1 &\n\
             fi\n\
             exit 0\n"
        ));
    }

    /// A fake `systemctl` that records its argv and fails every call, as one
    /// with no user manager behind it does.
    fn failing_systemctl(&self) {
        self.write_systemctl("exit 5\n");
    }

    fn write_systemctl(&self, body: &str) {
        let script = self.dir("bin").join("systemctl");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\n{body}",
                log = self.dir("bin").join("systemctl.log").display()
            ),
        )
        .expect("write the fake systemctl");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod +x");
    }

    /// `mp daemon install-service` under the dry-run hook, for `data`.
    fn install_unit(&self, data: &std::path::Path) {
        let install = Command::new(MP)
            .args(["daemon", "install-service"])
            .env("HOME", self.dir("home"))
            .env("MAILYPOPPINS_DATA_DIR", data)
            .env("MAILYPOPPINS_CONFIG_DIR", self.dir("config"))
            .env("XDG_CONFIG_HOME", self.dir("xdg"))
            .env("PATH", self.dir("bin"))
            .env("MAILYPOPPINS_DAEMON_SERVICE_OS", "linux")
            .env("MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN", "1")
            .output()
            .expect("run mp daemon install-service");
        ok(&install, "install-service");
    }

    fn systemctl_calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir("bin").join("systemctl.log"))
            .map(|raw| raw.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // The daemon the fake `systemctl` started is nobody's child; the pid
        // file is the only handle on it.
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    // SAFETY: a pid our own sandbox daemon published.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
}

fn ok(out: &Output, what: &str) -> Vec<String> {
    assert!(
        out.status.success(),
        "{what} exits 0; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn wait_gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    // SAFETY: signal 0 delivers nothing.
    while unsafe { libc::kill(pid, 0) } == 0 {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}

/// `start` and `restart` go through `systemctl --user start` when this data
/// directory's unit is installed, and `stop` does not touch `systemctl`.
#[test]
fn start_and_restart_go_through_the_installed_unit() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    assert!(
        sandbox.systemctl_calls().is_empty(),
        "the dry-run install ran nothing"
    );

    let via = format!("  systemctl --user start {UNIT_NAME}");
    let start_call = format!("--user start {UNIT_NAME}");

    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]);
    let lines = ok(&start, "start");
    let first = sandbox.recorded_pid();
    assert_eq!(
        lines,
        [
            format!("\u{2713} daemon started (pid {first})"),
            via.clone()
        ]
    );
    assert_eq!(sandbox.systemctl_calls(), std::slice::from_ref(&start_call));

    let restart = sandbox.mp(&["daemon", "restart"]);
    let lines = ok(&restart, "restart");
    let second = sandbox.recorded_pid();
    assert_ne!(second, first, "restart replaces the process");
    assert!(wait_gone(first), "the pre-restart daemon {first} exited");
    assert_eq!(
        lines,
        [
            "\u{2713} daemon stopped".to_string(),
            format!("\u{2713} daemon started (pid {second})"),
            via,
        ]
    );
    assert_eq!(
        sandbox.systemctl_calls(),
        [start_call.clone(), start_call.clone()]
    );

    let stop = sandbox.mp(&["daemon", "stop"]);
    assert_eq!(ok(&stop, "stop"), ["\u{2713} daemon stopped"]);
    assert!(wait_gone(second), "the stopped daemon {second} exited");
    assert_eq!(
        sandbox.systemctl_calls(),
        [start_call.clone(), start_call],
        "stop goes through the socket, not systemctl"
    );
}

/// Another data directory's unit is not this `mp`'s: the start is detached
/// and `systemctl` is never run, which is what keeps every other test in the
/// tree off a developer's real unit.
#[test]
fn another_data_directorys_unit_leaves_the_start_detached() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl();
    let other = sandbox.dir("other-data");
    fs::create_dir_all(&other).expect("other data dir");
    sandbox.install_unit(&other);

    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]);
    let lines = ok(&start, "start");
    let pid = sandbox.recorded_pid();
    assert_eq!(lines, [format!("\u{2713} daemon started (pid {pid})")]);
    assert!(
        String::from_utf8_lossy(&start.stderr).is_empty(),
        "and says nothing about the unit: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert!(sandbox.systemctl_calls().is_empty(), "systemctl never ran");

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(pid), "the stopped daemon {pid} exited");
}

/// This data directory's unit with `systemctl` on `PATH` but no user manager
/// behind it (no `$XDG_RUNTIME_DIR/systemd/private`): the start is detached,
/// says why on stderr, and never runs `systemctl`.
#[test]
fn no_reachable_user_manager_leaves_the_start_detached_with_a_note() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    fs::remove_file(sandbox.dir("run").join("systemd").join("private"))
        .expect("remove the sandbox user-manager socket");

    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]);
    let lines = ok(&start, "start");
    let pid = sandbox.recorded_pid();
    assert_eq!(lines, [format!("\u{2713} daemon started (pid {pid})")]);
    let stderr = String::from_utf8_lossy(&start.stderr);
    assert!(
        stderr.contains("note: ")
            && stderr.contains("no systemd user manager is reachable")
            && stderr.contains("systemd/private does not exist"),
        "{stderr}"
    );
    assert!(sandbox.systemctl_calls().is_empty(), "systemctl never ran");

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(pid), "the stopped daemon {pid} exited");
}

/// A `systemctl --user start` that fails after `restart` has stopped the
/// daemon is a note and a detached start, never a restart that ends with no
/// daemon running.
#[test]
fn a_failing_service_start_falls_back_to_a_detached_daemon() {
    let sandbox = Sandbox::new();
    sandbox.failing_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    let start_call = format!("--user start {UNIT_NAME}");

    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]);
    let lines = ok(&start, "start");
    let first = sandbox.recorded_pid();
    assert_eq!(lines, [format!("\u{2713} daemon started (pid {first})")]);
    let stderr = String::from_utf8_lossy(&start.stderr);
    assert!(
        stderr.contains("note: ")
            && stderr.contains(&format!(
                "`systemctl --user start {UNIT_NAME}` exited with 5"
            )),
        "{stderr}"
    );
    assert_eq!(sandbox.systemctl_calls(), std::slice::from_ref(&start_call));

    let restart = sandbox.mp(&["daemon", "restart"]);
    let lines = ok(&restart, "restart");
    let second = sandbox.recorded_pid();
    assert_ne!(second, first, "restart replaces the process");
    assert!(wait_gone(first), "the pre-restart daemon {first} exited");
    assert_eq!(
        lines,
        [
            "\u{2713} daemon stopped".to_string(),
            format!("\u{2713} daemon started (pid {second})"),
        ]
    );

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(second), "the stopped daemon {second} exited");
}
