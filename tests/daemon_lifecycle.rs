//! Daemon lifecycle commands: `run`, `start`, `status`, `stop`, `restart`
//! (#0120, unit P2-U6).
//!
//! This file is a **contract test**: it is written before `mp daemon` exists,
//! against the CLI surface fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.3 (unit P2-U6) and
//! the conventions in section 3.0. It compiles under `--features daemon`
//! today, because it only spawns the `mp` binary, but every test in it must
//! fail against the pre-daemon binary (clap rejects the unknown `daemon`
//! subcommand). An implementer does not edit this file; they make it pass.
//!
//! # Surface under test
//!
//! ```text
//! mp daemon run       [--foreground-logs]   # never connects to another daemon
//! mp daemon start     [--timeout-secs N]    # detached; returns after readiness; default 10
//! mp daemon status    [--json]
//! mp daemon stop      [--timeout-secs N]
//! mp daemon restart
//! ```
//!
//! `mp daemon status --json` prints exactly one JSON object:
//!
//! ```json
//! {"running":bool,"instance_id":str|null,"app_version":str|null,
//!  "protocol":{"min":u32,"max":u32}|null,"pid":u32|null,"started_at":str|null,
//!  "data_dir":str,"config_dir":str,
//!  "accounts":[{"name":str,"state":"opening"|"ready"|"blocked"}]}
//! ```
//!
//! Exit codes (plan section 3.0): `0` success, `1` generic error, `3`
//! incompatible daemon, `4` daemon unavailable / failed to start. `status` on a
//! dead daemon is `1`; `stop` on a dead daemon is `0` with `no daemon running`
//! on stderr; `start` against a live daemon is `0` without spawning.
//!
//! # Contract points this file pins beyond the plan text
//!
//! - **The child-death hook.** "A `start` whose child dies" needs a
//!   deterministic failure. This file pins a test-only environment hook,
//!   `MAILYPOPPINS_DAEMON_FAIL_START=1`: `mp daemon run` initialises logging,
//!   then exits nonzero *before* binding the socket. Nothing else in the tree
//!   reads it and no flag exposes it, so `mp --help` does not move. `mp daemon
//!   start` must then exit `4` and print, on stderr, the daemon log path and
//!   the literal `mp daemon run`.
//! - **`status --json` when nothing runs** still prints the full object:
//!   `running:false`, the five nullable fields `null`, `data_dir` /
//!   `config_dir` resolved as usual, and `accounts` an empty array. The plan
//!   makes those fields nullable but does not say the object shrinks, so it
//!   does not.
//! - **`data_dir` / `config_dir` in the JSON** are the daemon's resolved
//!   absolute paths, i.e. exactly what `$MAILYPOPPINS_DATA_DIR` and
//!   `$MAILYPOPPINS_CONFIG_DIR` name (canonicalised: macOS `TempDir` paths go
//!   through `/private`).
//! - **`accounts` with no accounts.** Every test that asserts on the array
//!   uses a tree with no `config.toml`, where the answer is unambiguously
//!   `[]` whatever the runtimes are doing. The MIG-04 test
//!   has a configured account and deliberately asserts nothing about the array,
//!   so the implementer stays free to list configured-but-not-started accounts.
//!
//! # Process hygiene
//!
//! Every process this file starts is killed before the test returns, including
//! on panic: everything spawned goes into a [`Proc`] whose `Drop` kills and
//! reaps it, and a detached daemon (spawned by `mp daemon start`, reparented
//! away from the test process) is killed by [`Sandbox`]'s `Drop`, which reads
//! `daemon.pid`. Every wait is a bounded poll against a deadline; nothing in
//! this file sleeps and hopes.
//!
//! Tests do not touch the test process's own environment: each one passes
//! `HOME`, `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` to the child
//! through `Command::env`, so they are safe to run in parallel.

use std::fs;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use mp_protocol::{PROTOCOL_MAX, PROTOCOL_MIN};

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: socket appearing, socket disappearing, a
/// child exiting, a `start` returning. Generous, because it is a ceiling and
/// never a sleep -- the polls below return as soon as the condition holds.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory, plus a kill list.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so a detached daemon
/// started by `mp daemon start` cannot outlive the test that started it, even
/// when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn runtime_dir(&self) -> PathBuf {
        self.data_dir().join("runtime")
    }

    fn socket(&self) -> PathBuf {
        self.runtime_dir().join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.pid")
    }

    fn instance_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.json")
    }

    fn logs_dir(&self) -> PathBuf {
        self.data_dir().join("logs")
    }

    /// An `mp` invocation pointed at this sandbox, with every daemon opt-in
    /// explicitly cleared so an inherited variable cannot change the outcome.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START");
        cmd
    }

    /// Like [`Sandbox::cmd`], but without `$MAILYPOPPINS_CONFIG_DIR`, so the
    /// config directory falls back to `$HOME/.config/mailypoppins` and the
    /// MIG-04 migration is *not* suppressed (`src/config.rs`: an explicit
    /// override disables it).
    fn cmd_home_config(&self) -> Command {
        let mut cmd = self.cmd();
        cmd.env_remove("MAILYPOPPINS_CONFIG_DIR");
        cmd
    }

    /// Spawn `mp daemon run` in the background, killed on drop.
    fn spawn_run(&self) -> Proc {
        self.spawn_run_with(self.cmd())
    }

    /// Spawn `mp daemon run` from a caller-shaped command (used by the MIG-04
    /// test, which needs `$MAILYPOPPINS_CONFIG_DIR` unset).
    fn spawn_run_with(&self, mut cmd: Command) -> Proc {
        let child = cmd
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        Proc::new(child)
    }

    /// `mp daemon status --json`, parsed. Returns the exit code beside it,
    /// because the two are asserted together everywhere.
    fn status_json(&self) -> (i32, Value) {
        let out = self
            .cmd()
            .args(["daemon", "status", "--json"])
            .output()
            .expect("run mp daemon status --json");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "`mp daemon status --json` did not print one JSON object: {e}\n\
                 stdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (code(&out.status), value)
    }

    fn socket_is_live(&self) -> bool {
        UnixStream::connect(self.socket()).is_ok()
    }

    /// Block until the socket accepts a connection, or fail the test.
    fn wait_socket_live(&self) {
        assert!(
            wait_until(|| self.socket_is_live()),
            "the daemon socket {} never accepted a connection within {DEADLINE:?}",
            self.socket().display()
        );
    }

    /// Block until the socket path is gone, or fail the test.
    fn wait_socket_absent(&self) {
        assert!(
            wait_until(|| !self.socket().exists()),
            "the daemon socket {} was still on disk after {DEADLINE:?}",
            self.socket().display()
        );
    }

    /// The pid recorded in `daemon.pid`, which the plan calls diagnostic-only
    /// but which every crash-recovery path reads.
    fn recorded_pid(&self) -> i32 {
        let raw = fs::read_to_string(self.pid_file()).expect("read daemon.pid");
        raw.trim()
            .parse()
            .unwrap_or_else(|e| panic!("daemon.pid must hold a bare decimal pid, got {raw:?}: {e}"))
    }

    /// `instance_id` out of `daemon.json`.
    fn recorded_instance_id(&self) -> String {
        let raw = fs::read_to_string(self.instance_file()).expect("read daemon.json");
        let meta: Value = serde_json::from_str(&raw).expect("daemon.json is JSON");
        meta["instance_id"]
            .as_str()
            .unwrap_or_else(|| panic!("daemon.json has a string instance_id, got {raw}"))
            .to_string()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A daemon started by `mp daemon start` is not our child: it detached
        // and was reparented. The pid file is the only handle on it.
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    signal(pid, libc::SIGKILL);
                }
            }
        }
    }
}

/// A spawned `mp` process, killed and reaped on drop, so a panicking assertion
/// never leaves a daemon behind.
struct Proc(Option<Child>);

impl Proc {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn pid(&self) -> i32 {
        self.0.as_ref().expect("process owned").id() as i32
    }

    /// Wait for the process to exit, bounded by [`DEADLINE`].
    fn wait_exit(&mut self) -> ExitStatus {
        let child = self.0.as_mut().expect("process owned");
        let start = Instant::now();
        loop {
            match child.try_wait().expect("try_wait") {
                Some(status) => return status,
                None if start.elapsed() >= DEADLINE => {
                    panic!("the process did not exit within {DEADLINE:?}")
                }
                None => std::thread::sleep(TICK),
            }
        }
    }

    /// Wait for exit and drain the piped stderr, for the commands whose
    /// diagnostic text is part of the contract.
    fn wait_with_stderr(&mut self) -> (i32, String) {
        let status = self.wait_exit();
        let mut buf = String::new();
        if let Some(pipe) = self.0.as_mut().expect("process owned").stderr.as_mut() {
            let _ = pipe.read_to_string(&mut buf);
        }
        (code(&status), buf)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Poll `cond` every [`TICK`] until it holds or [`DEADLINE`] expires.
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= DEADLINE {
            return false;
        }
        std::thread::sleep(TICK);
    }
}

fn signal(pid: i32, sig: i32) {
    // Safety: `kill` on a pid we started, or on one read from our own pid file.
    unsafe { libc::kill(pid, sig) };
}

/// The exit code, or `-1` for death by signal (which no assertion here wants).
fn code(status: &ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Assert the full `status --json` object shape from the unit contract, and
/// nothing weaker: every key present, every type as specified.
fn assert_status_shape(v: &Value) {
    let obj = v.as_object().expect("status --json prints an object");
    let expected = [
        "running",
        "instance_id",
        "app_version",
        "protocol",
        "pid",
        "started_at",
        "data_dir",
        "config_dir",
        "accounts",
    ];
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut want = expected;
    want.sort_unstable();
    assert_eq!(
        keys,
        want.to_vec(),
        "status --json has exactly the keys the contract lists"
    );

    assert!(v["running"].is_boolean(), "running is a bool");
    assert!(v["data_dir"].is_string(), "data_dir is a string");
    assert!(v["config_dir"].is_string(), "config_dir is a string");
    let accounts = v["accounts"].as_array().expect("accounts is an array");
    for account in accounts {
        assert!(account["name"].is_string(), "account.name is a string");
        let state = account["state"]
            .as_str()
            .expect("account.state is a string");
        assert!(
            matches!(state, "opening" | "ready" | "blocked"),
            "account.state is one of opening/ready/blocked, got {state:?}"
        );
    }

    if v["running"].as_bool().unwrap() {
        assert!(v["instance_id"].is_string(), "running: instance_id is set");
        assert!(v["app_version"].is_string(), "running: app_version is set");
        assert!(v["pid"].is_u64(), "running: pid is a number");
        assert!(v["started_at"].is_string(), "running: started_at is set");
        assert_eq!(
            v["protocol"]["min"].as_u64(),
            Some(u64::from(PROTOCOL_MIN)),
            "running: protocol.min is PROTOCOL_MIN"
        );
        assert_eq!(
            v["protocol"]["max"].as_u64(),
            Some(u64::from(PROTOCOL_MAX)),
            "running: protocol.max is PROTOCOL_MAX"
        );
    } else {
        for key in [
            "instance_id",
            "app_version",
            "protocol",
            "pid",
            "started_at",
        ] {
            assert!(
                v[key].is_null(),
                "not running: {key} is null, got {}",
                v[key]
            );
        }
        assert!(
            accounts.is_empty(),
            "not running: accounts is an empty array, got {}",
            v["accounts"]
        );
    }
}

/// `TempDir` paths differ from the daemon's resolved paths on macOS (`/var` is
/// a symlink to `/private/var`), so compare canonicalised.
fn assert_same_path(reported: &Value, expected: &Path, label: &str) {
    let reported = reported
        .as_str()
        .unwrap_or_else(|| panic!("{label} is a string, got {reported}"));
    let lhs = fs::canonicalize(reported).unwrap_or_else(|_| PathBuf::from(reported));
    let rhs = fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
    assert_eq!(lhs, rhs, "{label} names the sandbox directory");
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// `mp daemon run` in a temp data root binds the socket and answers `status`.
#[test]
fn run_binds_the_socket_and_answers_status() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.spawn_run();
    sandbox.wait_socket_live();

    // Plain `status` is the exit-code surface: 0 while a daemon runs.
    let plain = sandbox
        .cmd()
        .args(["daemon", "status"])
        .output()
        .expect("run mp daemon status");
    assert_eq!(
        code(&plain.status),
        0,
        "`mp daemon status` exits 0 against a live daemon; stderr: {}",
        String::from_utf8_lossy(&plain.stderr)
    );

    let (exit, status) = sandbox.status_json();
    assert_eq!(exit, 0, "`status --json` exits 0 against a live daemon");
    assert_status_shape(&status);
    assert_eq!(status["running"], Value::Bool(true));
    assert_eq!(
        status["pid"].as_i64(),
        Some(i64::from(daemon.pid())),
        "status reports the pid of the running daemon"
    );
    assert_eq!(
        status["app_version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        "status reports this build's version"
    );
    assert_same_path(&status["data_dir"], &sandbox.data_dir(), "data_dir");
    assert_same_path(&status["config_dir"], &sandbox.config_dir(), "config_dir");

    // The runtime files the plan pins, written by the daemon that is serving.
    assert_eq!(
        sandbox.recorded_pid(),
        daemon.pid(),
        "daemon.pid holds the serving daemon's pid"
    );
    assert_eq!(
        sandbox.recorded_instance_id(),
        status["instance_id"]
            .as_str()
            .expect("instance_id")
            .to_string(),
        "daemon.json and `status --json` agree on instance_id"
    );
}

/// `status` with nothing running: exit 1, and the JSON object still complete.
#[test]
fn status_without_a_daemon_exits_1_and_reports_not_running() {
    let sandbox = Sandbox::new();

    let plain = sandbox
        .cmd()
        .args(["daemon", "status"])
        .output()
        .expect("run mp daemon status");
    assert_eq!(
        code(&plain.status),
        1,
        "`mp daemon status` exits 1 when no daemon runs"
    );

    let (exit, status) = sandbox.status_json();
    assert_eq!(exit, 1, "`status --json` exits 1 when no daemon runs");
    assert_status_shape(&status);
    assert_eq!(status["running"], Value::Bool(false));
    assert_same_path(&status["data_dir"], &sandbox.data_dir(), "data_dir");
    assert_same_path(&status["config_dir"], &sandbox.config_dir(), "config_dir");
}

/// A daemon with no `config.toml` starts anyway and reports zero accounts.
///
/// The plan's Daemon-lifecycle section: "A missing `config.toml` is not a
/// startup failure: the daemon serves with zero accounts".
#[test]
fn run_without_a_config_file_serves_zero_accounts() {
    let sandbox = Sandbox::new();
    assert!(
        !sandbox.config_dir().join("config.toml").exists(),
        "the sandbox starts without a config file"
    );

    let _daemon = sandbox.spawn_run();
    sandbox.wait_socket_live();

    let (exit, status) = sandbox.status_json();
    assert_eq!(exit, 0, "a config-less daemon is a running daemon");
    assert_status_shape(&status);
    assert_eq!(status["running"], Value::Bool(true));
    assert_eq!(
        status["accounts"],
        Value::Array(Vec::new()),
        "no config.toml means `\"accounts\": []`"
    );
}

/// `run` performs MIG-04 before the first config load: a legacy
/// `~/.config/email` in the sandbox `HOME` is moved to
/// `~/.config/mailypoppins`, contents untouched.
///
/// `$MAILYPOPPINS_CONFIG_DIR` is deliberately unset here, because an explicit
/// override suppresses the migration (`src/config.rs`).
#[test]
fn run_performs_the_legacy_config_dir_migration() {
    let sandbox = Sandbox::new();
    let legacy = sandbox.home().join(".config").join("email");
    let migrated = sandbox.home().join(".config").join("mailypoppins");
    fs::create_dir_all(&legacy).expect("legacy config dir");
    fs::write(
        legacy.join("config.toml"),
        "[[accounts]]\nname = \"legacy\"\ndefault_from = \"me@example.com\"\n",
    )
    .expect("legacy config");
    fs::write(legacy.join("marker.txt"), "moved, not rewritten\n").expect("marker");

    let mut daemon = sandbox.spawn_run_with(sandbox.cmd_home_config());
    sandbox.wait_socket_live();

    assert!(
        !legacy.exists(),
        "the legacy directory {} is gone after startup",
        legacy.display()
    );
    assert!(
        migrated.join("config.toml").exists(),
        "the config moved to {}",
        migrated.display()
    );
    assert_eq!(
        fs::read_to_string(migrated.join("marker.txt")).expect("marker survived"),
        "moved, not rewritten\n",
        "MIG-04 renames the directory and rewrites nothing inside it"
    );

    // The migrated config is the one the daemon reports.
    let (exit, status) = sandbox
        .cmd_home_config()
        .args(["daemon", "status", "--json"])
        .output()
        .map(|out| {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let value: Value = serde_json::from_str(stdout.trim())
                .unwrap_or_else(|e| panic!("status --json parse: {e}\nstdout: {stdout}"));
            (code(&out.status), value)
        })
        .expect("run mp daemon status --json");
    assert_eq!(exit, 0, "the migrated daemon is running");
    assert_status_shape(&status);
    assert_same_path(&status["config_dir"], &migrated, "config_dir");

    signal(daemon.pid(), libc::SIGTERM);
    assert_eq!(
        code(&daemon.wait_exit()),
        0,
        "SIGTERM shuts the daemon down"
    );
    sandbox.wait_socket_absent();
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

/// Two concurrent `mp daemon start` produce exactly one daemon: one pid file,
/// one instance id, one process. A third, sequential `start` against the live
/// daemon returns 0 without spawning anything.
#[test]
fn two_concurrent_starts_yield_one_pid_file_and_one_instance_id() {
    let sandbox = Sandbox::new();

    let spawn_start = || {
        Proc::new(
            sandbox
                .cmd()
                .args(["daemon", "start", "--timeout-secs", "15"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn mp daemon start"),
        )
    };
    let mut first = spawn_start();
    let mut second = spawn_start();

    let (second_code, second_err) = second.wait_with_stderr();
    let (first_code, first_err) = first.wait_with_stderr();

    assert_eq!(first_code, 0, "first start exits 0; stderr: {first_err}");
    assert_eq!(second_code, 0, "second start exits 0; stderr: {second_err}");

    sandbox.wait_socket_live();

    // Exactly one pid file, naming a live process.
    let pid = sandbox.recorded_pid();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "daemon.pid {pid} names a live process"
    );
    let runtime_entries: Vec<String> = fs::read_dir(sandbox.runtime_dir())
        .expect("runtime dir")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with("daemon.pid"))
        .collect();
    assert_eq!(
        runtime_entries.len(),
        1,
        "exactly one pid file in the runtime dir, found {runtime_entries:?}"
    );

    let instance = sandbox.recorded_instance_id();
    let (exit, status) = sandbox.status_json();
    assert_eq!(exit, 0);
    assert_status_shape(&status);
    assert_eq!(
        status["instance_id"].as_str(),
        Some(instance.as_str()),
        "one instance id across both starters"
    );
    assert_eq!(status["pid"].as_i64(), Some(i64::from(pid)));

    // A start against a live daemon: 0, and the same instance.
    let third = sandbox
        .cmd()
        .args(["daemon", "start"])
        .output()
        .expect("third start");
    assert_eq!(
        code(&third.status),
        0,
        "start against a live daemon exits 0; stderr: {}",
        String::from_utf8_lossy(&third.stderr)
    );
    assert_eq!(
        sandbox.recorded_instance_id(),
        instance,
        "start against a live daemon spawns nothing"
    );
}

/// A `start` whose child dies reports the daemon log path and exits 4.
///
/// The failure is forced through the test-only hook pinned in this file's
/// header: `MAILYPOPPINS_DAEMON_FAIL_START=1` makes `mp daemon run` exit
/// nonzero after logging is up and before the socket is bound.
#[test]
fn a_start_whose_child_dies_prints_the_log_path_and_exits_4() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .cmd()
        .env("MAILYPOPPINS_DAEMON_FAIL_START", "1")
        .args(["daemon", "start", "--timeout-secs", "5"])
        .output()
        .expect("run mp daemon start");

    assert_eq!(
        code(&out.status),
        4,
        "a daemon that fails to start exits 4 (plan section 3.0); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let logs = sandbox.logs_dir();
    assert!(
        stderr.contains(&logs.display().to_string()),
        "the failure names the daemon log path under {}; stderr: {stderr}",
        logs.display()
    );
    assert!(
        stderr.contains("mp daemon run"),
        "the failure names the explicit foreground command; stderr: {stderr}"
    );
    assert!(
        !sandbox.socket_is_live(),
        "a failed start leaves no listening socket"
    );
}

// ---------------------------------------------------------------------------
// stop and restart
// ---------------------------------------------------------------------------

/// `stop` removes its own socket and leaves another daemon's socket alone.
///
/// Two sandboxes, two data roots, two daemons: stopping one must not disturb
/// the other. The plan: shutdown "removes only its own socket".
#[test]
fn stop_removes_only_its_own_socket() {
    let mine = Sandbox::new();
    let other = Sandbox::new();

    let mut daemon = mine.spawn_run();
    let _other_daemon = other.spawn_run();
    mine.wait_socket_live();
    other.wait_socket_live();

    let stop = mine
        .cmd()
        .args(["daemon", "stop", "--timeout-secs", "15"])
        .output()
        .expect("run mp daemon stop");
    assert_eq!(
        code(&stop.status),
        0,
        "stop against a live daemon exits 0; stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    mine.wait_socket_absent();
    assert_eq!(
        code(&daemon.wait_exit()),
        0,
        "a stopped daemon exits 0 by itself"
    );

    assert!(
        other.socket_is_live(),
        "the other daemon's socket at {} is untouched",
        other.socket().display()
    );
    let (exit, status) = other.status_json();
    assert_eq!(exit, 0, "the other daemon is still running");
    assert_status_shape(&status);
    assert_eq!(status["running"], Value::Bool(true));

    // And the stopped one is really gone.
    let (exit, status) = mine.status_json();
    assert_eq!(exit, 1, "the stopped daemon reports not running");
    assert_eq!(status["running"], Value::Bool(false));
}

/// `stop` with nothing running: exit 0, `no daemon running` on stderr.
#[test]
fn stop_without_a_daemon_exits_0_and_says_no_daemon_running() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .cmd()
        .args(["daemon", "stop"])
        .output()
        .expect("run mp daemon stop");
    assert_eq!(
        code(&out.status),
        0,
        "stop on a dead daemon exits 0 (plan section 3.3)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("no daemon running"),
        "stop on a dead daemon says `no daemon running` on stderr; stderr: {stderr}"
    );
}

/// `restart` replaces the daemon: a new `instance_id`, a live socket, and the
/// old process gone.
#[test]
fn restart_yields_a_new_instance_id() {
    let sandbox = Sandbox::new();

    let start = sandbox
        .cmd()
        .args(["daemon", "start", "--timeout-secs", "15"])
        .output()
        .expect("run mp daemon start");
    assert_eq!(
        code(&start.status),
        0,
        "start exits 0; stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    sandbox.wait_socket_live();

    let before_instance = sandbox.recorded_instance_id();
    let before_pid = sandbox.recorded_pid();

    let restart = sandbox
        .cmd()
        .args(["daemon", "restart"])
        .output()
        .expect("run mp daemon restart");
    assert_eq!(
        code(&restart.status),
        0,
        "restart exits 0; stderr: {}",
        String::from_utf8_lossy(&restart.stderr)
    );
    sandbox.wait_socket_live();

    let (exit, status) = sandbox.status_json();
    assert_eq!(exit, 0, "a restarted daemon is running");
    assert_status_shape(&status);
    let after_instance = status["instance_id"]
        .as_str()
        .expect("instance_id")
        .to_string();
    assert_ne!(
        after_instance, before_instance,
        "restart yields a new instance_id"
    );
    assert_eq!(
        sandbox.recorded_instance_id(),
        after_instance,
        "daemon.json carries the new instance_id"
    );

    let after_pid = sandbox.recorded_pid();
    assert_ne!(after_pid, before_pid, "restart replaces the process");
    assert!(
        wait_until(|| unsafe { libc::kill(before_pid, 0) } != 0),
        "the pre-restart daemon {before_pid} exited"
    );
}

// ---------------------------------------------------------------------------
// signals
// ---------------------------------------------------------------------------

/// SIGTERM to a foreground daemon: exit 0, socket unlinked.
#[test]
fn sigterm_to_a_foreground_daemon_exits_0_and_unlinks_the_socket() {
    let sandbox = Sandbox::new();
    let mut daemon = sandbox.spawn_run();
    sandbox.wait_socket_live();

    signal(daemon.pid(), libc::SIGTERM);

    let status = daemon.wait_exit();
    assert_eq!(
        code(&status),
        0,
        "SIGTERM is a graceful shutdown, not a crash: exit 0"
    );
    sandbox.wait_socket_absent();

    let (exit, json) = sandbox.status_json();
    assert_eq!(exit, 1, "after SIGTERM no daemon runs");
    assert_eq!(json["running"], Value::Bool(false));
}
