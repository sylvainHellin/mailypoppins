//! `mp daemon start` and `mp daemon restart` under an installed login service
//! (PERSO-109), and a client's on-demand start under one (PERSO-219).
//!
//! With the systemd unit for this data directory installed, both commands ask
//! systemd for the daemon instead of spawning a detached one, which would
//! leave the unit dead beside a daemon systemd does not own: `start` with
//! `systemctl --user start mailypoppins.service`, and `restart`, after its
//! socket stop, with `systemctl --user restart mailypoppins.service`, since
//! systemd may still count the unit active then. `mp daemon stop` stays the
//! socket's, which the unit records as a clean stop.
//!
//! No real service manager is reached. `MAILYPOPPINS_DAEMON_SERVICE_OS=linux`
//! selects the systemd half on any host, the unit is written by
//! `mp daemon install-service` under the dry-run hook into a sandbox
//! `XDG_CONFIG_HOME`, and the child's `PATH` is a sandbox directory holding a
//! fake `systemctl` that records its argv and, on `start` and `restart`, does
//! what systemd would: runs `mp daemon run` in the background with the unit's
//! environment.
//! `XDG_RUNTIME_DIR` is a sandbox directory whose `systemd/private` is a bound
//! Unix socket, which is all the probe for a reachable user manager reads.
//!
//! A daemon that is slow to answer is not a real one: [`SlowDaemon`] binds the
//! sandbox socket once the fake `systemctl` has been asked for the daemon and
//! answers `daemon.status` only later, as a daemon opening a large store does.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// The systemd unit's name, as every `systemctl` line carries it.
const UNIT_NAME: &str = "mailypoppins.service";

/// The line `start` and `restart` print on stderr once the answer window has
/// passed while a daemon holds the socket without answering yet.
const STILL_OPENING: &str = "daemon is up and not answering yet, waiting";

/// What the failing fake `systemctl` prints on stderr, as systemd's own does
/// for a unit that hit its start limit.
const FAILED_JOB: &str = "Job for mailypoppins.service failed: start request repeated too quickly";

/// What the failing fake `systemctl` prints on stdout, which systemd's own
/// does not, so that a leak there shows too.
const FAILED_STDOUT: &str = "fake systemctl: the job failed";

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

    /// The socket a daemon for the sandbox data directory binds, in a runtime
    /// directory created at the daemon's mode.
    fn socket(&self) -> PathBuf {
        let runtime = self.dir("data").join("runtime");
        fs::create_dir_all(&runtime).expect("runtime dir");
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        runtime.join("daemon.sock")
    }

    fn recorded_pid(&self) -> i32 {
        let raw = fs::read_to_string(self.pid_file()).expect("read daemon.pid");
        raw.trim().parse().expect("daemon.pid holds a pid")
    }

    /// An `mp` that sees the systemd half, the sandbox unit directory and the
    /// fake `systemctl` and nothing else on `PATH`.
    fn mp(&self, args: &[&str]) -> Output {
        self.command(args)
            .output()
            .unwrap_or_else(|e| panic!("run mp {args:?}: {e}"))
    }

    /// [`Sandbox::mp`] for a command that needs a daemon, with on-demand
    /// starting on and a budget a cold daemon meets on a loaded host.
    fn mp_on_demand(&self, args: &[&str]) -> Output {
        self.command(args)
            .env_remove("MAILYPOPPINS_DAEMON_AUTOSTART")
            .env("MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS", "15000")
            .output()
            .unwrap_or_else(|e| panic!("run mp {args:?}: {e}"))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(MP);
        command
            .args(args)
            .env("HOME", self.dir("home"))
            .env("MAILYPOPPINS_DATA_DIR", self.dir("data"))
            .env("MAILYPOPPINS_CONFIG_DIR", self.dir("config"))
            .env("XDG_CONFIG_HOME", self.dir("xdg"))
            .env("PATH", self.dir("bin"))
            .env("XDG_RUNTIME_DIR", self.dir("run"))
            .env("MAILYPOPPINS_DAEMON_SERVICE_OS", "linux")
            .env_remove("MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START");
        command
    }

    /// A fake `systemctl` that appends its argv to `bin/systemctl.log` and, on
    /// `start` and `restart`, runs `mp daemon run` detached, as a `Type=simple`
    /// unit would; it keeps no state, so the unit always reads inactive.
    fn fake_systemctl(&self) {
        self.write_systemctl(&format!(
            "if [ \"$2\" = start ] || [ \"$2\" = restart ]; then\n\
             \x20 '{MP}' daemon run </dev/null >/dev/null 2>&1 &\n\
             fi\n\
             exit 0\n"
        ));
    }

    /// A fake `systemctl` that keeps the unit's state as systemd does: the
    /// unit is active while `bin/unit.mainpid` names its main process, a
    /// `start` of an active unit is a no-op, and the manager, which is the
    /// daemon's parent, marks the unit inactive only `latency` after the
    /// daemon exited, as systemd does once it has processed the `SIGCHLD`.
    /// `stop` and `restart` signal the main process and wait for the unit to
    /// go inactive; `enable --now` starts it as `start` does, and `is-active`
    /// and `show -p MainPID --value` answer from the same state, the latter
    /// with `0` for an inactive unit as systemd does.
    fn stateful_systemctl(&self, latency: Duration) {
        let state = self.dir("bin").join("unit.mainpid");
        let state = state.display();
        let latency = latency.as_secs_f64();
        self.write_systemctl(&format!(
            "PATH=/usr/bin:/bin\n\
             state='{state}'\n\
             do_start() {{\n\
             \x20 [ -s \"$state\" ] && return 0\n\
             \x20 ( '{MP}' daemon run </dev/null >/dev/null 2>&1 &\n\
             \x20   echo $! > \"$state\"; wait $!; sleep {latency}; rm -f \"$state\" ) \
                  </dev/null >/dev/null 2>&1 &\n\
             \x20 while [ ! -s \"$state\" ]; do sleep 0.05; done\n\
             }}\n\
             do_stop() {{\n\
             \x20 [ -s \"$state\" ] || return 0\n\
             \x20 kill -TERM \"$(cat \"$state\")\" 2>/dev/null\n\
             \x20 while [ -e \"$state\" ]; do sleep 0.05; done\n\
             }}\n\
             case \"$2\" in\n\
             \x20 start) do_start ;;\n\
             \x20 stop) do_stop ;;\n\
             \x20 restart) do_stop; do_start ;;\n\
             \x20 enable) if [ \"$3\" = --now ]; then do_start; fi ;;\n\
             \x20 is-active) if [ -s \"$state\" ]; then echo active; else echo inactive; exit 3; fi ;;\n\
             \x20 show) if [ -s \"$state\" ]; then cat \"$state\"; else echo 0; fi ;;\n\
             esac\n\
             exit 0\n"
        ));
    }

    /// The main pid the stateful fake `systemctl` holds the unit active with,
    /// or `None` when the unit is inactive.
    fn unit_main_pid(&self) -> Option<i32> {
        fs::read_to_string(self.dir("bin").join("unit.mainpid"))
            .ok()
            .and_then(|raw| raw.trim().parse().ok())
    }

    /// A fake `systemctl` that records its argv and starts nothing: the daemon
    /// it is asked for, if any, is the test's [`SlowDaemon`].
    fn recording_systemctl(&self) {
        self.write_systemctl("exit 0\n");
    }

    /// A fake `systemctl` that records its argv and fails every call, as one
    /// whose unit hit its start limit does, saying so on stderr as systemd
    /// does ([`FAILED_JOB`]) and on stdout ([`FAILED_STDOUT`]), so a test sees
    /// which of the client's streams the manager's output reached.
    fn failing_systemctl(&self) {
        self.write_systemctl(&format!(
            "echo '{FAILED_JOB}' >&2\n\
             echo '{FAILED_STDOUT}'\n\
             exit 5\n"
        ));
    }

    /// Everything the sandbox's log files hold, the client's and the
    /// daemon's, which share the data directory's log directory.
    fn logs(&self) -> String {
        let Ok(entries) = fs::read_dir(self.dir("data").join("logs")) else {
            return String::new();
        };
        entries
            .filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok())
            .collect()
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

    /// A fake `launchctl` that appends its argv to `bin/launchctl.log`, answers
    /// `print` as for an agent launchd holds loaded and runs no process of,
    /// and on `bootstrap` runs `mp daemon run` detached, as loading a
    /// `RunAtLoad` agent would.
    fn fake_launchctl(&self) {
        let script = self.dir("bin").join("launchctl");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo \"$@\" >> '{log}'\n\
                 if [ \"$1\" = bootstrap ]; then\n\
                 \x20 '{MP}' daemon run </dev/null >/dev/null 2>&1 &\n\
                 fi\n\
                 exit 0\n",
                log = self.dir("bin").join("launchctl.log").display()
            ),
        )
        .expect("write the fake launchctl");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod +x");
    }

    fn launchctl_calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir("bin").join("launchctl.log"))
            .map(|raw| raw.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// [`Sandbox::mp`] on the launchd half.
    fn mp_darwin(&self, args: &[&str]) -> Output {
        self.command(args)
            .env("MAILYPOPPINS_DAEMON_SERVICE_OS", "darwin")
            .output()
            .unwrap_or_else(|e| panic!("run mp {args:?}: {e}"))
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

    /// [`Sandbox::systemctl_calls`] without the `show` probes, which change
    /// nothing: what systemd was asked to do.
    fn systemctl_actions(&self) -> Vec<String> {
        self.systemctl_calls()
            .into_iter()
            .filter(|call| !call.starts_with("--user show "))
            .collect()
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

/// A stand-in for a daemon that binds its socket at once and answers only
/// later, as one does while it opens a large store (PERSO-109: the home
/// server's daemon answered 11.6 s after it bound, against a 10 s wait).
///
/// It binds once the fake `systemctl` has been called, so the command's own
/// first probe, before it asks the service manager, finds nothing, and then
/// answers every `daemon.status` from `silent` on, or never with `None`.
struct SlowDaemon {
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SlowDaemon {
    fn spawn(sandbox: &Sandbox, silent: Option<Duration>) -> Self {
        let asked = sandbox.dir("bin").join("systemctl.log");
        let socket = sandbox.socket();
        let done = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            let pause = || std::thread::sleep(Duration::from_millis(10));
            while !asked.exists() {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                pause();
            }
            let listener = UnixListener::bind(&socket).expect("bind the slow daemon's socket");
            let bound = Instant::now();
            while silent.is_none_or(|silent| bound.elapsed() < silent) {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                pause();
            }
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => answer_status(stream),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => pause(),
                    Err(e) => panic!("the slow daemon's accept failed: {e}"),
                }
            }
        });
        Self {
            done,
            thread: Some(thread),
        }
    }
}

impl Drop for SlowDaemon {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answer the one `daemon.status` request on `stream`, if it holds one: a
/// connection the command gave up on reads as closed, and is dropped.
fn answer_status(stream: UnixStream) {
    stream.set_nonblocking(false).expect("blocking stream");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let request: serde_json::Value = serde_json::from_str(&line).expect("a JSON-RPC request");
    assert_eq!(request["method"], "daemon.status", "{line}");
    let answer = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": {"pid": std::process::id()},
    });
    let _ = (&stream).write_all(format!("{answer}\n").as_bytes());
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

/// `start` goes through `systemctl --user start` and `restart` through
/// `systemctl --user restart` when this data directory's unit is installed,
/// and `stop` does not touch `systemctl`.
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
    let restart_call = format!("--user restart {UNIT_NAME}");

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
            format!("  systemctl --user restart {UNIT_NAME}"),
        ]
    );
    assert_eq!(
        sandbox.systemctl_calls(),
        [start_call.clone(), restart_call.clone()]
    );

    let stop = sandbox.mp(&["daemon", "stop"]);
    assert_eq!(ok(&stop, "stop"), ["\u{2713} daemon stopped"]);
    assert!(wait_gone(second), "the stopped daemon {second} exited");
    assert_eq!(
        sandbox.systemctl_calls(),
        [start_call, restart_call],
        "stop goes through the socket, not systemctl"
    );
}

/// A command that needs a daemon and finds none starts it through the
/// installed unit, as `mp daemon start` does, and spawns no detached daemon
/// that the unit would then crash-loop against (PERSO-219); the start prints
/// nothing into the command's own output.
#[test]
fn an_on_demand_start_goes_through_the_installed_unit() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));

    let out = sandbox.mp_on_demand(&["account", "list", "--daemon"]);
    assert_eq!(
        ok(&out, "account list"),
        ["No accounts configured"],
        "the command's answer and nothing of the start's"
    );
    assert_eq!(
        sandbox.systemctl_calls(),
        [format!("--user start {UNIT_NAME}")],
        "the daemon is the unit's: systemctl was asked for it"
    );

    let pid = sandbox.recorded_pid();
    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(pid), "the stopped daemon {pid} exited");
}

/// A `systemctl --user start` that fails during an on-demand start falls
/// back to a detached daemon, so the command is still served, and what the
/// manager printed reaches neither of the command's streams: it goes into the
/// note the client logs, since a TUI reconnecting in raw mode or a script
/// reading the command's output must not get it.
#[test]
fn a_failing_on_demand_service_start_falls_back_to_a_detached_daemon() {
    let sandbox = Sandbox::new();
    sandbox.failing_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));

    let out = sandbox.mp_on_demand(&["account", "list", "--daemon"]);
    // The sandbox has no config file, so the command's own warnings about
    // that are on stderr; nothing of the start may be.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(FAILED_JOB) && !stderr.contains("systemctl"),
        "nothing of the failed start reaches the command's stderr:\n{stderr}"
    );
    assert_eq!(ok(&out, "account list"), ["No accounts configured"]);
    assert_eq!(
        sandbox.systemctl_calls(),
        [format!("--user start {UNIT_NAME}")]
    );
    let logs = sandbox.logs();
    assert!(
        logs.lines().any(|line| line.contains(&format!(
            "`systemctl --user start {UNIT_NAME}` exited with 5"
        )) && line.contains(FAILED_JOB)
            && line.contains(FAILED_STDOUT)
            && line.contains("so this daemon starts outside the service")),
        "the logged note carries what the manager said:\n{logs}"
    );

    let pid = sandbox.recorded_pid();
    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(pid), "the stopped daemon {pid} exited");
}

/// `mp daemon install-service` over a daemon already running hands it over to
/// the unit: it stops the daemon over its socket and has systemd start the
/// unit, instead of enabling a unit whose daemon cannot bind the socket the
/// running one holds (PERSO-219). A second install, over the unit's own
/// daemon with the unit file unchanged, leaves that daemon running and says
/// so: the handover is for a daemon outside the service, which the install
/// tells apart by the unit's main pid.
#[test]
fn install_over_a_running_daemon_hands_it_over_to_the_unit() {
    let sandbox = Sandbox::new();
    sandbox.stateful_systemctl(Duration::from_secs(1));
    let unit = sandbox
        .dir("xdg")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME);
    let show = format!("--user show -p MainPID --value {UNIT_NAME}");
    let reload = "--user daemon-reload".to_string();
    let enable = format!("--user enable {UNIT_NAME}");
    let restart = format!("--user restart {UNIT_NAME}");

    ok(
        &sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]),
        "start",
    );
    let detached = sandbox.recorded_pid();
    assert!(
        sandbox.systemctl_calls().is_empty(),
        "with no unit the daemon is detached"
    );

    let install = sandbox.mp(&["daemon", "install-service"]);
    let lines = ok(&install, "install-service");
    assert!(wait_gone(detached), "the detached daemon {detached} exited");
    let served = sandbox.recorded_pid();
    assert_ne!(served, detached, "the unit runs a daemon of its own");
    assert_eq!(
        sandbox.unit_main_pid(),
        Some(served),
        "the unit is active with that daemon as its main process"
    );
    assert_eq!(
        lines,
        [
            format!("\u{2713} wrote {}", unit.display()),
            format!("  systemctl {reload}"),
            format!("  systemctl {enable}"),
            format!("  the running daemon (pid {detached}) is handed over to the service"),
            "\u{2713} daemon stopped".to_string(),
            format!("\u{2713} daemon started (pid {served})"),
            format!("  systemctl {restart}"),
        ]
    );
    assert_eq!(
        sandbox.systemctl_actions(),
        [reload.clone(), enable.clone(), restart.clone()]
    );

    let again = sandbox.mp(&["daemon", "install-service"]);
    let lines = ok(&again, "install-service again");
    assert_eq!(
        lines,
        [
            format!("\u{2713} {} is already installed", unit.display()),
            format!("  systemctl {reload}"),
            format!("  systemctl {enable}"),
            format!("  the service's own daemon (pid {served}) is running and is left as it is"),
        ]
    );
    assert_eq!(
        sandbox.recorded_pid(),
        served,
        "the daemon was not replaced"
    );
    assert_eq!(
        sandbox.unit_main_pid(),
        Some(served),
        "and the unit still runs it"
    );
    assert_eq!(
        sandbox.systemctl_actions()[3..],
        [reload, enable],
        "the re-install re-enables the unit and restarts nothing"
    );
    assert_eq!(
        sandbox
            .systemctl_calls()
            .iter()
            .filter(|call| **call == show)
            .count(),
        2,
        "each install asked systemd for the unit's main pid"
    );

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(served), "the stopped daemon {served} exited");
}

/// `mp daemon install-service --force` over the unit's own running daemon with
/// a unit file that differs rewrites the file, reloads systemd before anything
/// starts, and restarts the unit, so the daemon that runs afterwards is the
/// new definition's.
#[test]
fn a_forced_install_over_the_units_own_daemon_restarts_it_with_the_new_file() {
    let sandbox = Sandbox::new();
    sandbox.stateful_systemctl(Duration::from_secs(1));
    sandbox.install_unit(&sandbox.dir("data"));
    let unit = sandbox
        .dir("xdg")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME);

    ok(
        &sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]),
        "start",
    );
    let served = sandbox.recorded_pid();
    assert_eq!(sandbox.unit_main_pid(), Some(served), "the unit runs it");
    let edited = format!(
        "{}# a hand edit\n",
        fs::read_to_string(&unit).expect("read the unit")
    );
    fs::write(&unit, edited).expect("edit the unit");

    let install = sandbox.mp(&["daemon", "install-service", "--force"]);
    let lines = ok(&install, "install-service --force");
    assert!(
        wait_gone(served),
        "the old definition's daemon {served} exited"
    );
    let second = sandbox.recorded_pid();
    assert_eq!(
        sandbox.unit_main_pid(),
        Some(second),
        "the unit runs the new one"
    );
    assert!(
        !fs::read_to_string(&unit)
            .expect("read the unit")
            .contains("# a hand edit"),
        "the file was rewritten"
    );
    assert_eq!(
        lines,
        [
            format!("\u{2713} wrote {}", unit.display()),
            "  systemctl --user daemon-reload".to_string(),
            format!("  systemctl --user enable {UNIT_NAME}"),
            format!("  the service's own daemon (pid {served}) is restarted to run the new file"),
            "\u{2713} daemon stopped".to_string(),
            format!("\u{2713} daemon started (pid {second})"),
            format!("  systemctl --user restart {UNIT_NAME}"),
        ]
    );
    assert_eq!(
        sandbox.systemctl_calls(),
        [
            format!("--user start {UNIT_NAME}"),
            format!("--user show -p MainPID --value {UNIT_NAME}"),
            "--user daemon-reload".to_string(),
            format!("--user enable {UNIT_NAME}"),
            format!("--user restart {UNIT_NAME}"),
        ],
        "systemd reads the new file before the restart"
    );

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(second), "the stopped daemon {second} exited");
}

/// `mp daemon install-service` with nothing running enables the unit and
/// starts it in one `systemctl --user enable --now`, as before PERSO-219.
#[test]
fn install_with_no_daemon_running_enables_and_starts_the_unit() {
    let sandbox = Sandbox::new();
    sandbox.stateful_systemctl(Duration::from_secs(1));

    let install = sandbox.mp(&["daemon", "install-service"]);
    let lines = ok(&install, "install-service");
    assert_eq!(
        lines[1..],
        [
            "  systemctl --user daemon-reload".to_string(),
            format!("  systemctl --user enable --now {UNIT_NAME}"),
        ]
    );
    assert!(sandbox.unit_main_pid().is_some(), "the unit is active");

    // The unit's daemon is the subshell's child and binds in its own time.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !sandbox.pid_file().exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let pid = sandbox.recorded_pid();
    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(pid), "the stopped daemon {pid} exited");
}

/// `mp daemon restart` under the unit brings the unit back active with the
/// new daemon as its main process, even though systemd still counts the unit
/// active for a moment after the old daemon exited (PERSO-109, checks 2 and 3
/// of 2026-11-12): a `systemctl --user start` sent in that moment is a no-op,
/// and the restart then fails with exit 4 and leaves the unit dead.
#[test]
fn restart_brings_the_unit_back_although_systemd_still_counts_it_active() {
    let sandbox = Sandbox::new();
    sandbox.stateful_systemctl(Duration::from_secs(2));
    sandbox.install_unit(&sandbox.dir("data"));

    ok(
        &sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]),
        "start",
    );
    let first = sandbox.recorded_pid();
    assert_eq!(
        sandbox.unit_main_pid(),
        Some(first),
        "the unit runs the daemon"
    );

    let restart = sandbox.mp(&["daemon", "restart"]);
    let lines = ok(&restart, "restart");
    let second = sandbox.recorded_pid();
    assert_ne!(second, first, "restart replaces the process");
    assert_eq!(
        lines[..2],
        [
            "\u{2713} daemon stopped".to_string(),
            format!("\u{2713} daemon started (pid {second})"),
        ]
    );
    assert_eq!(
        sandbox.unit_main_pid(),
        Some(second),
        "the unit is active with the new daemon as its main process"
    );

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(second), "the stopped daemon {second} exited");
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

/// A `systemctl --user start` that fails is a note and a detached start, and
/// so is a `systemctl --user restart` that fails after `restart` has stopped
/// the daemon, never a restart that ends with no daemon running.
#[test]
fn a_failing_service_start_falls_back_to_a_detached_daemon() {
    let sandbox = Sandbox::new();
    sandbox.failing_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    let start_call = format!("--user start {UNIT_NAME}");

    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "15"]);
    let lines = ok(&start, "start");
    let first = sandbox.recorded_pid();
    assert_eq!(
        lines,
        [
            FAILED_STDOUT.to_string(),
            format!("\u{2713} daemon started (pid {first})")
        ],
        "the manager's own output shows, as the command ran it"
    );
    let stderr = String::from_utf8_lossy(&start.stderr);
    assert!(
        stderr.contains(FAILED_JOB)
            && stderr.contains("note: ")
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
            FAILED_STDOUT.to_string(),
            format!("\u{2713} daemon started (pid {second})"),
        ]
    );
    let stderr = String::from_utf8_lossy(&restart.stderr);
    assert!(
        stderr.contains(FAILED_JOB)
            && stderr.contains("note: ")
            && stderr.contains(&format!(
                "`systemctl --user restart {UNIT_NAME}` exited with 5"
            )),
        "{stderr}"
    );
    assert_eq!(
        sandbox.systemctl_calls(),
        [start_call, format!("--user restart {UNIT_NAME}")]
    );

    ok(&sandbox.mp(&["daemon", "stop"]), "stop");
    assert!(wait_gone(second), "the stopped daemon {second} exited");
}

/// A daemon the service started that binds its socket at once but answers
/// only after the answer window, because it is still starting up, is a
/// daemon that came up: `start` says once that it is waiting and exits 0 when
/// the daemon answers (PERSO-109, 2026-11-12: exit 4 after 10 s for a daemon
/// that answered 11.6 s after it bound).
#[test]
fn start_waits_past_the_window_for_a_daemon_still_opening_its_stores() {
    let sandbox = Sandbox::new();
    sandbox.recording_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    let _daemon = SlowDaemon::spawn(&sandbox, Some(Duration::from_secs(7)));

    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "1"]);
    let lines = ok(&start, "start");
    assert_eq!(
        lines,
        [
            "\u{2713} daemon started".to_string(),
            format!("  systemctl --user start {UNIT_NAME}"),
        ]
    );
    let stderr = String::from_utf8_lossy(&start.stderr);
    assert_eq!(stderr.matches(STILL_OPENING).count(), 1, "{stderr}");
}

/// `restart` takes `start`'s `--timeout-secs` and waits for a slow daemon the
/// same way.
#[test]
fn restart_waits_past_the_window_for_a_daemon_still_opening_its_stores() {
    let sandbox = Sandbox::new();
    sandbox.recording_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    let _daemon = SlowDaemon::spawn(&sandbox, Some(Duration::from_secs(7)));

    let restart = sandbox.mp(&["daemon", "restart", "--timeout-secs", "1"]);
    let lines = ok(&restart, "restart");
    assert_eq!(
        lines,
        [
            "\u{2713} daemon started".to_string(),
            format!("  systemctl --user restart {UNIT_NAME}"),
        ]
    );
    let stderr = String::from_utf8_lossy(&restart.stderr);
    assert_eq!(stderr.matches(STILL_OPENING).count(), 1, "{stderr}");
}

/// A service start after which nothing binds the socket is exit 4 at the end
/// of the answer window, not at the ceiling a bound daemon gets.
#[test]
fn a_service_start_that_binds_nothing_fails_at_the_window() {
    let sandbox = Sandbox::new();
    sandbox.recording_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));

    let began = Instant::now();
    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "1"]);
    assert_eq!(start.status.code(), Some(4), "{start:?}");
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "{:?}",
        began.elapsed()
    );
    let stderr = String::from_utf8_lossy(&start.stderr);
    assert!(stderr.contains("did not answer within 1s"), "{stderr}");
    assert!(!stderr.contains(STILL_OPENING), "{stderr}");
}

/// A socket file a dead daemon left behind refuses the connect, so it is no
/// daemon at all: exit 4 at the end of the answer window.
#[test]
fn a_stale_socket_is_not_a_daemon_still_opening_its_stores() {
    let sandbox = Sandbox::new();
    sandbox.recording_systemctl();
    sandbox.install_unit(&sandbox.dir("data"));
    let socket = sandbox.socket();
    drop(UnixListener::bind(&socket).expect("bind, then leave the file behind"));
    assert!(socket.exists(), "the stale socket file stays");

    let began = Instant::now();
    let start = sandbox.mp(&["daemon", "start", "--timeout-secs", "1"]);
    assert_eq!(start.status.code(), Some(4), "{start:?}");
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "{:?}",
        began.elapsed()
    );
    let stderr = String::from_utf8_lossy(&start.stderr);
    assert!(stderr.contains("did not answer within 1s"), "{stderr}");
    assert!(!stderr.contains(STILL_OPENING), "{stderr}");
}

/// `mp daemon install-service --force` on macOS over a running daemon, with
/// an older plist that launchd holds loaded, boots the agent out and
/// bootstraps the new plist after the socket stop, so the agent that runs is
/// the new file's: `launchctl kickstart -k` would restart the definition
/// launchd loaded and never read the new file.
#[test]
fn a_forced_install_over_a_loaded_agent_loads_the_new_plist() {
    let sandbox = Sandbox::new();
    sandbox.fake_launchctl();
    let plist = sandbox
        .dir("home")
        .join("Library")
        .join("LaunchAgents")
        .join("dev.mailypoppins.daemon.plist");
    // SAFETY: `getuid` takes no argument and cannot fail.
    let label = format!("gui/{}/dev.mailypoppins.daemon", unsafe { libc::getuid() });
    let domain = format!("gui/{}", unsafe { libc::getuid() });

    ok(
        &sandbox.mp_darwin(&["daemon", "start", "--timeout-secs", "15"]),
        "start",
    );
    let detached = sandbox.recorded_pid();
    assert!(
        sandbox.launchctl_calls().is_empty(),
        "with no agent the daemon is detached"
    );
    let dry = sandbox
        .command(&["daemon", "install-service"])
        .env("MAILYPOPPINS_DAEMON_SERVICE_OS", "darwin")
        .env("MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN", "1")
        .output()
        .expect("run mp daemon install-service");
    ok(&dry, "dry-run install-service");
    let older = format!(
        "{}<!-- an older version -->\n",
        fs::read_to_string(&plist).expect("read the plist")
    );
    fs::write(&plist, older).expect("age the plist");

    let install = sandbox.mp_darwin(&["daemon", "install-service", "--force"]);
    let lines = ok(&install, "install-service --force");
    assert!(wait_gone(detached), "the detached daemon {detached} exited");
    let served = sandbox.recorded_pid();
    assert_ne!(served, detached, "the agent runs a daemon of its own");
    assert_eq!(
        lines,
        [
            format!("\u{2713} wrote {}", plist.display()),
            format!("  the running daemon (pid {detached}) is handed over to the service"),
            "\u{2713} daemon stopped".to_string(),
            format!("\u{2713} daemon started (pid {served})"),
            format!("  launchctl bootout {label}"),
            format!("  launchctl bootstrap {domain} {}", plist.display()),
        ]
    );
    assert_eq!(
        sandbox.launchctl_calls(),
        [
            format!("print {label}"),
            format!("print {label}"),
            format!("bootout {label}"),
            format!("bootstrap {domain} {}", plist.display()),
        ],
        "launchd is asked whether it holds the agent and which pid it runs, \
         then unloads it and loads the new file"
    );

    ok(&sandbox.mp_darwin(&["daemon", "stop"]), "stop");
    assert!(wait_gone(served), "the stopped daemon {served} exited");
}
