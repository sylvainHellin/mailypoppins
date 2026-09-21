//! Login-start service units: `mp daemon install-service` and
//! `mp daemon uninstall-service` (#0125, unit P6-U5, `LIF-06`).
//!
//! This file is a **contract test**: it is written before the two subcommands
//! exist, against the surface fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.8 (P6-U5/P6-U6) and
//! pinned in full in `docs/tickets/0125-daemon-hardening.md`. It compiles
//! against the tree as committed - it spawns the `mp` binary and imports two
//! names that already exist - and every row that is contract rather than
//! regression fails at runtime, today with clap's `unrecognized subcommand`.
//! An implementer does not edit this file; they make it pass.
//!
//! # Surface under test
//!
//! ```text
//! mp daemon install-service   [--force] [--check]
//! mp daemon uninstall-service
//! ```
//!
//! Both are local commands. They write a file and call the platform's service
//! manager; **no daemon needs to run**, no socket is opened and no wire method
//! is added, which is why this whole file is socket-free. The parity matrix's
//! `LIF-06` row said `daemon.install_service` / `daemon.remove_service`; a
//! method would mean asking a running daemon to arrange for a daemon to run,
//! so the row is corrected to name the commands instead (P6-U5 edits it).
//!
//! # The two environment hooks this file pins
//!
//! Both follow the `MAILYPOPPINS_DAEMON_*` family `docs/daemon-operations.md`
//! documents, and neither has a flag, so `mp --help` does not move.
//!
//! - `MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN=1` - render, write and remove the
//!   unit exactly as usual, but **execute no `systemctl` and no `launchctl`**.
//!   The command still prints the lines it would have run, plus one line
//!   saying nothing was run. This is what keeps the suite off the developer's
//!   own user session: the file goes into a `TempDir`, and the service manager
//!   is never invoked.
//! - `MAILYPOPPINS_DAEMON_SERVICE_OS=linux|darwin` - which half to install.
//!   Unset means this build's target OS. An unrecognised value is an error.
//!   It exists because the macOS half cannot be smoke-tested on the machine
//!   this project is developed on (the plan says so and carries the live
//!   launchd check as an escalation), so the plist has to be reachable from a
//!   Linux test run or it is pinned nowhere at all.
//!
//! Three rows deliberately run **without** the dry-run hook, with `PATH`
//! pointing at a sandbox directory holding a fake `systemctl` that records its
//! argv. Containment is by `PATH`, not by the hook: the real `systemctl` is
//! not reachable from those children, and it is the only way to pin *which*
//! commands are run and in which order rather than only which are printed.
//!
//! # The generated files
//!
//! | OS | path | fixture |
//! |---|---|---|
//! | linux | `$XDG_CONFIG_HOME/systemd/user/mailypoppins.service` | `tests/fixtures/service/mailypoppins.service` |
//! | darwin | `$HOME/Library/LaunchAgents/dev.mailypoppins.daemon.plist` | `tests/fixtures/service/dev.mailypoppins.daemon.plist` |
//!
//! `$XDG_CONFIG_HOME` falls back to `$HOME/.config`, which is XDG's own rule
//! and systemd's. The fixtures are the byte-exact content with three
//! placeholders substituted: `{{MP}}` is the absolute path of the running
//! executable, `std::env::current_exe()`, and `{{DATA_DIR}}` and
//! `{{CONFIG_DIR}}` the data and config directories the installing `mp`
//! resolved, canonicalised, i.e. exactly the two strings
//! `mp daemon status --json` reports.
//!
//! The rows render `{{MP}}` canonicalised because the test binary is a real
//! file under `target/`, where the two spellings are the same string. They are
//! not the same string for a Homebrew `mp`, whose `bin/mp` is a symlink into a
//! version-stamped Cellar directory: baking the resolved path there would give
//! a unit that breaks at the next `brew upgrade`. Nothing here can tell the
//! two apart, so `current_exe()` is what the contract names and
//! `docs/release-process.md` carries the repair (`--force` after a move).
//!
//! Baking the two directories into the unit is the point of installing it: a
//! login-started daemon inherits the session manager's environment, not the
//! shell's, so a user whose `MAILYPOPPINS_DATA_DIR` or `XDG_DATA_HOME` is
//! exported from a shell rc file would otherwise get a daemon serving a
//! different tree than the one his `mp` talks to. Nothing else is in
//! `Environment=` / `EnvironmentVariables`: the binary is invoked by absolute
//! path, and the daemon spawns nothing that needs a `PATH`.
//!
//! All three are quoted where they land, because all three are paths a user
//! chose: the unit's `ExecStart` and both `Environment=` values are
//! double-quoted systemd values with `\` and `"` backslash-escaped, and the
//! plist's are `<string>` bodies with `&`, `<`, `>` and `"` as entities.
//! `src/daemon/service.rs`'s own unit tests render a path holding a space and
//! an `&` through both templates; the sandbox paths here hold neither, so the
//! fixture comparison is unaffected by the escaping and stays byte-exact.
//!
//! `TimeoutStopSec` and `ExitTimeOut` are the shutdown grace P6-U4 landed plus
//! a five second margin, and one row ties the literal in the fixture to
//! `DEFAULT_GRACE_SECS` so a changed grace fails here rather than truncating a
//! shutdown in production.
//!
//! # Stdout, pinned line for line
//!
//! In the style of `mp daemon stop`: a `✓`/`✗` first line, then indented
//! detail lines.
//!
//! ```text
//! ✓ wrote <path>                     # a fresh install
//! ✓ <path> is already installed      # an identical one; the enable still runs
//!   systemctl --user daemon-reload
//!   systemctl --user enable --now mailypoppins.service
//!   dry run: MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN is set, nothing was run
//!
//! ✓ removed <path>                   # uninstall
//!   systemctl --user disable --now mailypoppins.service
//!   systemctl --user daemon-reload
//!
//! ✓ no service installed             # uninstall with nothing there: a no-op
//!
//! ✓ service installed at <path>      # --check, exit 0
//! ✗ no service installed             # --check, exit 1
//!   install one: mp daemon install-service
//! ```
//!
//! The darwin half prints the same shapes with one command line:
//! `launchctl bootstrap gui/<uid> <plist>` on install,
//! `launchctl bootout gui/<uid>/dev.mailypoppins.daemon` on uninstall.
//!
//! A unit already on disk whose content differs is **refused**: exit 1, the
//! `✗` line on stderr naming the path and `--force`, and the file left
//! byte-identical. `--force` replaces it.
//!
//! # What this file does not pin, and who does
//!
//! - **A live `systemctl --user enable`**: P6-U6's smoke run on this host, not
//!   a test. A suite that enabled a real unit would start a daemon against the
//!   developer's real data directory.
//! - **launchd at all**: escalated by the plan. The plist is pinned as a
//!   fixture, linted by `plutil` when it is on `PATH`, and checked
//!   structurally otherwise.
//! - **`mp daemon status`**: it stays the five-field lifecycle report and its
//!   `--json` key set does not grow (`tests/daemon_lifecycle.rs` pins it
//!   exactly). Whether a unit is installed is `install-service --check`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

use mailypoppins::daemon::client::needs_daemon;
use mailypoppins::daemon::shutdown::DEFAULT_GRACE_SECS;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Skip the service manager, keep everything else.
const DRY_RUN_ENV: &str = "MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN";

/// `linux` or `darwin`; unset means this build's target OS.
const OS_ENV: &str = "MAILYPOPPINS_DAEMON_SERVICE_OS";

/// The systemd unit's name, which is also the name every `systemctl` line
/// carries.
const UNIT_NAME: &str = "mailypoppins.service";

/// The launchd label, which is also the plist's file stem and the last
/// component of the `bootout` target.
const LAUNCHD_LABEL: &str = "dev.mailypoppins.daemon";

/// How much longer than the shutdown grace the service manager waits before it
/// resorts to `SIGKILL`. Five seconds: long enough for the eight steps to
/// finish after the grace expires, short enough that a stuck daemon does not
/// hold up a logout.
const STOP_MARGIN_SECS: u64 = 5;

/// The dry-run line, verbatim.
const DRY_RUN_LINE: &str = "  dry run: MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN is set, nothing was run";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A private `HOME`, `XDG_CONFIG_HOME`, data directory, config directory and
/// `PATH`, so nothing a row does can reach the host's systemd or launchd.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "xdg-config", "config", "data", "bin"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn xdg_config_home(&self) -> PathBuf {
        self.root.path().join("xdg-config")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    /// The only directory on the children's `PATH`.
    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    /// Where the systemd user unit goes.
    fn unit_path(&self) -> PathBuf {
        self.xdg_config_home()
            .join("systemd")
            .join("user")
            .join(UNIT_NAME)
    }

    /// Where the launch agent goes.
    fn plist_path(&self) -> PathBuf {
        self.home()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"))
    }

    /// `mp` pointed at this sandbox, with the dry-run hook on and an empty
    /// `PATH`, which is the shape every row uses unless it says otherwise.
    fn mp(&self) -> Command {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.xdg_config_home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env("PATH", self.bin())
            .env(DRY_RUN_ENV, "1")
            .env(OS_ENV, "linux");
        cmd
    }

    /// The same, for the launchd half.
    fn mp_darwin(&self) -> Command {
        let mut cmd = self.mp();
        cmd.env(OS_ENV, "darwin");
        cmd
    }

    /// `mp daemon install-service <args>`.
    fn install(&self, args: &[&str]) -> Output {
        self.mp()
            .args(["daemon", "install-service"])
            .args(args)
            .output()
            .expect("run mp daemon install-service")
    }

    /// `mp daemon uninstall-service`.
    fn uninstall(&self) -> Output {
        self.mp()
            .args(["daemon", "uninstall-service"])
            .output()
            .expect("run mp daemon uninstall-service")
    }

    /// Put a fake `systemctl` on the sandbox `PATH` that appends its argv to
    /// `bin/systemctl.log` and exits `code`.
    ///
    /// Containment for the three rows that run with the dry-run hook off: the
    /// child's `PATH` is this directory and nothing else, so `systemctl` can
    /// only ever resolve to this script.
    fn fake_systemctl(&self, code: i32) -> PathBuf {
        let log = self.bin().join("systemctl.log");
        let script = self.bin().join("systemctl");
        fs::write(
            &script,
            format!("#!/bin/sh\necho \"$@\" >> {}\nexit {code}\n", log.display()),
        )
        .expect("write the fake systemctl");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod +x");
        log
    }

    /// What the fake `systemctl` was called with, one entry per invocation, in
    /// order. Empty when it was never run.
    fn systemctl_calls(&self) -> Vec<String> {
        match fs::read_to_string(self.bin().join("systemctl.log")) {
            Ok(raw) => raw.lines().map(str::to_string).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The rendered fixture for this sandbox: the committed file with `{{MP}}`,
    /// `{{DATA_DIR}}` and `{{CONFIG_DIR}}` substituted.
    fn expected(&self, fixture: &str) -> String {
        fs::create_dir_all(self.data_dir()).expect("data dir");
        fs::create_dir_all(self.config_dir()).expect("config dir");
        fixture_text(fixture)
            .replace("{{MP}}", &canonical(Path::new(MP)))
            .replace("{{DATA_DIR}}", &canonical(&self.data_dir()))
            .replace("{{CONFIG_DIR}}", &canonical(&self.config_dir()))
    }
}

/// A committed fixture under `tests/fixtures/service/`.
fn fixture_text(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("service")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// An absolute, symlink-resolved path as a string, which is what the daemon's
/// own `canonical` helper produces and therefore what the unit must carry.
fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn stdout_lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn stderr_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// The two `systemctl` lines an install prints, in order.
fn enable_lines() -> Vec<String> {
    vec![
        "  systemctl --user daemon-reload".to_string(),
        format!("  systemctl --user enable --now {UNIT_NAME}"),
    ]
}

/// The two an uninstall prints, in the other order: disable first, then
/// reload, because reloading before the unit file is gone re-reads it.
fn disable_lines() -> Vec<String> {
    vec![
        format!("  systemctl --user disable --now {UNIT_NAME}"),
        "  systemctl --user daemon-reload".to_string(),
    ]
}

fn uid() -> u32 {
    // SAFETY: `getuid` takes no argument, cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

/// The value of one `key=value` line of the systemd unit.
fn unit_value(unit: &str, key: &str) -> Option<String> {
    unit.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

/// Whether `name` resolves on the *test process's* `PATH`, for the optional
/// `plutil` lint.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(name);
                candidate.is_file()
                    && fs::metadata(&candidate)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

// ===========================================================================
// 1. The systemd user unit
// ===========================================================================

/// A fresh install writes the fixture, byte for byte, where `$XDG_CONFIG_HOME`
/// says.
#[test]
fn install_writes_the_systemd_user_unit_the_fixture_pins() {
    let sandbox = Sandbox::new();

    let out = sandbox.install(&[]);
    assert_eq!(
        code(&out),
        0,
        "a fresh install exits 0; stderr: {}",
        stderr_text(&out)
    );

    let unit = sandbox.unit_path();
    assert!(
        unit.exists(),
        "the unit is written to {}, which is `$XDG_CONFIG_HOME/systemd/user/{UNIT_NAME}`",
        unit.display()
    );
    assert_eq!(
        fs::read_to_string(&unit).expect("read the unit"),
        sandbox.expected(UNIT_NAME),
        "the unit is the committed fixture with the three paths substituted"
    );
    assert_eq!(
        fs::metadata(&unit)
            .expect("stat the unit")
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "the unit is world-readable like every other systemd unit: it holds no secret"
    );
}

/// The unit runs the **foreground** daemon, never the detached start.
///
/// A `Type=simple` unit whose `ExecStart` forked and returned would be
/// restarted forever by `Restart=on-failure`, and the daemon it left behind
/// would be one systemd does not own.
#[test]
fn the_unit_starts_the_foreground_run_and_never_the_detached_start() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    let unit = fs::read_to_string(sandbox.unit_path()).expect("read the unit");

    assert_eq!(
        unit_value(&unit, "ExecStart"),
        Some(format!("\"{}\" daemon run", canonical(Path::new(MP)))),
        "ExecStart is the absolute path of this executable, double-quoted so a \
         space in it is one argument, plus `daemon run`; unit:\n{unit}"
    );
    for forbidden in ["daemon start", "daemon restart", "--foreground-logs"] {
        assert!(
            !unit.contains(forbidden),
            "the unit does not mention `{forbidden}`; unit:\n{unit}"
        );
    }

    assert_eq!(unit_value(&unit, "Type"), Some("simple".to_string()));
    assert_eq!(unit_value(&unit, "Restart"), Some("on-failure".to_string()));
    assert_eq!(
        unit_value(&unit, "KillSignal"),
        Some("SIGTERM".to_string()),
        "SIGTERM is the signal the daemon's eight shutdown steps answer"
    );
    assert_eq!(
        unit_value(&unit, "WantedBy"),
        Some("default.target".to_string()),
        "a user unit wanted by default.target is what starts at login"
    );

    let environment: Vec<&str> = unit
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .collect();
    assert_eq!(
        environment,
        vec![
            format!(
                "\"MAILYPOPPINS_DATA_DIR={}\"",
                canonical(&sandbox.data_dir())
            ),
            format!(
                "\"MAILYPOPPINS_CONFIG_DIR={}\"",
                canonical(&sandbox.config_dir())
            ),
        ],
        "the unit carries the two directories the installing `mp` resolved, each one \
         double-quoted so a space in a path does not truncate the assignment, and nothing else"
    );
}

/// The service manager's stop timeout is the daemon's own grace plus a margin.
///
/// The tie is asserted rather than assumed: a grace raised in
/// `src/daemon/shutdown.rs` without the unit following it would have systemd
/// `SIGKILL` a daemon in the middle of the eight steps, which is exactly the
/// ungraceful stop P6-U4 exists to prevent.
#[test]
fn the_stop_timeout_is_the_shutdown_grace_plus_a_margin() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    let unit = fs::read_to_string(sandbox.unit_path()).expect("read the unit");

    let expected = DEFAULT_GRACE_SECS + STOP_MARGIN_SECS;
    assert_eq!(
        unit_value(&unit, "TimeoutStopSec"),
        Some(expected.to_string()),
        "TimeoutStopSec is DEFAULT_GRACE_SECS ({DEFAULT_GRACE_SECS}) + {STOP_MARGIN_SECS}"
    );

    let plist = sandbox.expected(&format!("{LAUNCHD_LABEL}.plist"));
    assert!(
        plist.contains(&format!("<integer>{expected}</integer>")),
        "and so is the launch agent's ExitTimeOut; plist:\n{plist}"
    );
}

/// The install says what it wrote and what it enabled, and nothing else.
#[test]
fn install_prints_the_path_it_wrote_and_the_enable_lines() {
    let sandbox = Sandbox::new();

    let out = sandbox.install(&[]);
    let mut expected = vec![format!("\u{2713} wrote {}", sandbox.unit_path().display())];
    expected.extend(enable_lines());
    expected.push(DRY_RUN_LINE.to_string());

    assert_eq!(
        stdout_lines(&out),
        expected,
        "stdout is the ✓ line, the commands, and the dry-run note; stderr: {}",
        stderr_text(&out)
    );
}

/// A second install over an identical unit rewrites nothing and still enables.
///
/// Idempotent in the sense that matters for a command a user re-runs after an
/// upgrade: the end state is the same, the file's mtime does not move, and the
/// `enable --now` runs again because a user who disabled the unit by hand
/// expects `install-service` to put it back.
#[test]
fn install_is_idempotent_on_an_identical_unit() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    let unit = sandbox.unit_path();
    let first = fs::metadata(&unit)
        .expect("stat")
        .modified()
        .expect("mtime");
    let content = fs::read_to_string(&unit).expect("read");

    let out = sandbox.install(&[]);
    assert_eq!(
        code(&out),
        0,
        "re-installing an identical unit exits 0; stderr: {}",
        stderr_text(&out)
    );

    let mut expected = vec![format!("\u{2713} {} is already installed", unit.display())];
    expected.extend(enable_lines());
    expected.push(DRY_RUN_LINE.to_string());
    assert_eq!(
        stdout_lines(&out),
        expected,
        "the second run says the unit is already there and still enables it"
    );

    assert_eq!(
        fs::read_to_string(&unit).expect("read"),
        content,
        "the file is unchanged"
    );
    assert_eq!(
        fs::metadata(&unit)
            .expect("stat")
            .modified()
            .expect("mtime"),
        first,
        "and was not rewritten with identical bytes: the mtime did not move"
    );
}

/// A unit whose content differs is refused, and `--force` replaces it.
///
/// A user may have edited the unit - an extra `Environment=`, a different
/// `Restart=` - and an upgrade that silently overwrote it would lose that
/// without saying so.
#[test]
fn install_refuses_to_overwrite_a_changed_unit_without_force() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    let unit = sandbox.unit_path();

    let edited = format!(
        "{}\nEnvironment=RUST_LOG=debug\n",
        fs::read_to_string(&unit).expect("read").trim_end()
    );
    fs::write(&unit, &edited).expect("hand-edit the unit");

    let out = sandbox.install(&[]);
    assert_eq!(
        code(&out),
        1,
        "refusing to overwrite is an error, not a success; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_text(&out);
    assert!(
        stderr.contains('\u{2717}'),
        "the refusal is a ✗ line; stderr: {stderr}"
    );
    assert!(
        stderr.contains(&unit.display().to_string()),
        "it names the file it would have replaced; stderr: {stderr}"
    );
    assert!(
        stderr.contains("differs"),
        "it says the file differs from what this version installs; stderr: {stderr}"
    );
    assert!(
        stderr.contains("--force"),
        "and names the flag that replaces it; stderr: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&unit).expect("read"),
        edited,
        "a refused install leaves the hand-edited unit byte-identical"
    );
}

/// `--force` is the other half of that rule.
#[test]
fn force_replaces_a_changed_unit() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    let unit = sandbox.unit_path();
    fs::write(&unit, "[Service]\nExecStart=/bin/false\n").expect("hand-edit the unit");

    let out = sandbox.install(&["--force"]);
    assert_eq!(
        code(&out),
        0,
        "--force exits 0; stderr: {}",
        stderr_text(&out)
    );
    assert_eq!(
        fs::read_to_string(&unit).expect("read"),
        sandbox.expected(UNIT_NAME),
        "--force restores the unit this version installs"
    );
    assert_eq!(
        stdout_lines(&out).first().map(String::as_str),
        Some(format!("\u{2713} wrote {}", unit.display()).as_str()),
        "and reports it as a write rather than as an already-installed no-op"
    );
}

/// `uninstall-service` removes the file and says what it disabled.
#[test]
fn uninstall_removes_the_unit_and_prints_the_disable_lines() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    let unit = sandbox.unit_path();

    let out = sandbox.uninstall();
    assert_eq!(
        code(&out),
        0,
        "uninstall exits 0; stderr: {}",
        stderr_text(&out)
    );

    let mut expected = vec![format!("\u{2713} removed {}", unit.display())];
    expected.extend(disable_lines());
    expected.push(DRY_RUN_LINE.to_string());
    assert_eq!(
        stdout_lines(&out),
        expected,
        "stdout names the file it removed and the commands it ran"
    );
    assert!(!unit.exists(), "the unit is gone from {}", unit.display());
}

/// An uninstall with nothing installed is a `✓` and a no-op.
///
/// Not an error: the user asked for a machine with no login service, and that
/// is what the machine has. Nothing is run either, so a user session without
/// the unit is not asked to disable a unit that was never there.
#[test]
fn uninstall_without_an_installed_unit_is_a_tick_and_a_no_op() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl(0);

    let out = sandbox
        .mp()
        .env_remove(DRY_RUN_ENV)
        .args(["daemon", "uninstall-service"])
        .output()
        .expect("run mp daemon uninstall-service");

    assert_eq!(
        code(&out),
        0,
        "nothing to remove is not a failure; stderr: {}",
        stderr_text(&out)
    );
    assert_eq!(
        stdout_lines(&out),
        vec!["\u{2713} no service installed".to_string()],
        "one line and nothing else"
    );
    assert!(
        sandbox.systemctl_calls().is_empty(),
        "and no systemctl was run: {:?}",
        sandbox.systemctl_calls()
    );
}

/// `--check` reports, and writes nothing.
#[test]
fn check_reports_installed_and_missing_without_writing_anything() {
    let sandbox = Sandbox::new();
    let unit = sandbox.unit_path();

    let out = sandbox.install(&["--check"]);
    assert_eq!(
        code(&out),
        1,
        "--check with nothing installed exits 1; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        stdout_lines(&out),
        vec![
            "\u{2717} no service installed".to_string(),
            "  install one: mp daemon install-service".to_string(),
        ],
        "and says how to install one"
    );
    assert!(
        !unit.exists(),
        "--check installs nothing: {} does not exist",
        unit.display()
    );

    assert_eq!(code(&sandbox.install(&[])), 0);
    let out = sandbox.install(&["--check"]);
    assert_eq!(
        code(&out),
        0,
        "--check against an installed unit exits 0; stderr: {}",
        stderr_text(&out)
    );
    assert_eq!(
        stdout_lines(&out),
        vec![format!("\u{2713} service installed at {}", unit.display())],
        "one line naming the file"
    );
    assert!(
        sandbox.systemctl_calls().is_empty(),
        "--check enables nothing: {:?}",
        sandbox.systemctl_calls()
    );
}

/// Without `$XDG_CONFIG_HOME` the unit lands under `$HOME/.config`.
#[test]
fn without_xdg_config_home_the_unit_lands_under_home_config() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .mp()
        .env_remove("XDG_CONFIG_HOME")
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        code(&out),
        0,
        "install exits 0 without XDG_CONFIG_HOME; stderr: {}",
        stderr_text(&out)
    );

    let fallback = sandbox
        .home()
        .join(".config")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME);
    assert!(
        fallback.exists(),
        "the XDG fallback is $HOME/.config, so the unit is at {}",
        fallback.display()
    );
    assert!(
        !sandbox.unit_path().exists(),
        "and not at the $XDG_CONFIG_HOME path this row unset"
    );
}

// ===========================================================================
// 2. What is actually run, with the service manager contained by PATH
// ===========================================================================

/// The dry-run hook executes nothing.
///
/// The guard on every other row in this file: a fake `systemctl` is the only
/// one reachable, and the command still must not call it.
#[test]
fn the_dry_run_hook_runs_no_service_manager_command() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl(0);

    assert_eq!(code(&sandbox.install(&[])), 0);
    assert!(
        sandbox.unit_path().exists(),
        "a dry run still writes the unit: only the enabling is skipped"
    );
    assert_eq!(code(&sandbox.uninstall()), 0);
    assert!(!sandbox.unit_path().exists(), "and still removes it");

    assert!(
        sandbox.systemctl_calls().is_empty(),
        "no systemctl ran under {DRY_RUN_ENV}=1: {:?}",
        sandbox.systemctl_calls()
    );
}

/// With the hook off, an install reloads and then enables, in that order.
#[test]
fn a_real_install_runs_daemon_reload_then_enable_now() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl(0);

    let out = sandbox
        .mp()
        .env_remove(DRY_RUN_ENV)
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        code(&out),
        0,
        "install exits 0 when systemctl succeeds; stderr: {}",
        stderr_text(&out)
    );

    assert_eq!(
        sandbox.systemctl_calls(),
        vec![
            "--user daemon-reload".to_string(),
            format!("--user enable --now {UNIT_NAME}"),
        ],
        "`systemctl` is resolved through PATH and called twice, reload first"
    );

    let mut expected = vec![format!("\u{2713} wrote {}", sandbox.unit_path().display())];
    expected.extend(enable_lines());
    assert_eq!(
        stdout_lines(&out),
        expected,
        "and the dry-run note is absent when the commands really ran"
    );
}

/// With the hook off, an uninstall disables and then reloads.
#[test]
fn a_real_uninstall_runs_disable_now_then_daemon_reload() {
    let sandbox = Sandbox::new();
    assert_eq!(code(&sandbox.install(&[])), 0);
    sandbox.fake_systemctl(0);

    let out = sandbox
        .mp()
        .env_remove(DRY_RUN_ENV)
        .args(["daemon", "uninstall-service"])
        .output()
        .expect("run mp daemon uninstall-service");
    assert_eq!(
        code(&out),
        0,
        "uninstall exits 0; stderr: {}",
        stderr_text(&out)
    );

    assert_eq!(
        sandbox.systemctl_calls(),
        vec![
            format!("--user disable --now {UNIT_NAME}"),
            "--user daemon-reload".to_string(),
        ],
        "disable while the unit file is still readable, then reload once it is gone"
    );
    assert!(!sandbox.unit_path().exists(), "the unit is gone");
}

/// A `systemctl` that fails is reported, and the unit stays on disk.
///
/// The file is what `install-service` owns; enabling is what it asked the
/// session manager for. Removing the unit because the enable failed would
/// throw away the half that worked.
#[test]
fn an_enable_that_fails_is_reported_and_leaves_the_unit_in_place() {
    let sandbox = Sandbox::new();
    sandbox.fake_systemctl(3);

    let out = sandbox
        .mp()
        .env_remove(DRY_RUN_ENV)
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");

    assert_eq!(
        code(&out),
        1,
        "a failed enable is a failed command; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_text(&out);
    assert!(
        stderr.contains('\u{2717}') && stderr.contains("systemctl"),
        "it says which command failed; stderr: {stderr}"
    );
    assert!(
        sandbox.unit_path().exists(),
        "and the unit it wrote is still at {}",
        sandbox.unit_path().display()
    );
}

/// No service manager on `PATH` at all: the unit is written, the commands are
/// printed to run by hand, and the command succeeds.
///
/// This is a container, a minimal image, or a machine whose session is not
/// systemd's. Refusing to write the file there would make the command useless
/// exactly where a user would copy the unit somewhere else himself.
#[test]
fn an_install_with_no_service_manager_on_path_still_writes_the_unit() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .mp()
        .env_remove(DRY_RUN_ENV)
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        code(&out),
        0,
        "a missing systemctl is not a failure of the write; stderr: {}",
        stderr_text(&out)
    );
    assert_eq!(
        fs::read_to_string(sandbox.unit_path()).expect("read the unit"),
        sandbox.expected(UNIT_NAME),
        "the unit is written all the same"
    );

    let mut expected = vec![format!("\u{2713} wrote {}", sandbox.unit_path().display())];
    expected.extend(enable_lines());
    expected
        .push("  systemctl is not on PATH; run the lines above to enable the service".to_string());
    assert_eq!(
        stdout_lines(&out),
        expected,
        "and the user is told to run the two lines himself"
    );
}

// ===========================================================================
// 3. The launchd user agent
// ===========================================================================

/// The darwin half writes the fixture into `~/Library/LaunchAgents`.
#[test]
fn darwin_writes_the_launch_agent_the_fixture_pins() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .mp_darwin()
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        code(&out),
        0,
        "the darwin half exits 0; stderr: {}",
        stderr_text(&out)
    );

    let plist = sandbox.plist_path();
    assert!(
        plist.exists(),
        "the launch agent is written to {}",
        plist.display()
    );
    assert_eq!(
        fs::read_to_string(&plist).expect("read the plist"),
        sandbox.expected(&format!("{LAUNCHD_LABEL}.plist")),
        "the plist is the committed fixture with the three paths substituted"
    );
    assert_eq!(
        fs::metadata(&plist).expect("stat").permissions().mode() & 0o777,
        0o644,
        "the plist is 0644, which is what launchd expects of an agent"
    );
    assert!(
        sandbox.data_dir().join("logs").is_dir(),
        "and the directory StandardOutPath names exists, or launchd refuses the job"
    );
}

/// The plist is well-formed and says what the contract says it says.
///
/// Structural rather than parsed: no plist crate is in `Cargo.lock` and this
/// unit adds no dependency for one file. `plutil` is the real check and runs
/// when the host has it, which this host does not.
#[test]
fn the_plist_is_well_formed_xml_and_plutil_lints_it() {
    let sandbox = Sandbox::new();
    assert_eq!(
        code(
            &sandbox
                .mp_darwin()
                .args(["daemon", "install-service"])
                .output()
                .expect("run mp daemon install-service")
        ),
        0
    );
    let path = sandbox.plist_path();
    let plist = fs::read_to_string(&path).expect("read the plist");

    assert!(
        plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
        "the XML declaration comes first; plist:\n{plist}"
    );
    assert!(
        plist.contains("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\""),
        "the Apple doctype is there; plist:\n{plist}"
    );
    assert!(
        plist.trim_end().ends_with("</plist>"),
        "and the document is closed; plist:\n{plist}"
    );
    assert_eq!(
        plist.matches("<dict>").count(),
        plist.matches("</dict>").count(),
        "every dict is closed"
    );
    assert_eq!(
        plist.matches("<array>").count(),
        plist.matches("</array>").count(),
        "every array is closed"
    );

    // The keys the contract fixes, in the order the fixture carries them.
    let mut cursor = 0usize;
    for key in [
        "Label",
        "ProgramArguments",
        "EnvironmentVariables",
        "RunAtLoad",
        "KeepAlive",
        "ExitTimeOut",
        "StandardOutPath",
        "StandardErrorPath",
    ] {
        let needle = format!("<key>{key}</key>");
        let at = plist[cursor..].find(&needle).unwrap_or_else(|| {
            panic!("the plist has a <key>{key}</key>, in order; plist:\n{plist}")
        });
        cursor += at + needle.len();
    }

    assert!(
        plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")),
        "the label is the bundle-style identifier the file is named after"
    );
    assert!(
        plist.contains(&format!(
            "<array>\n\t\t<string>{}</string>\n\t\t<string>daemon</string>\n\t\t<string>run</string>\n\t</array>",
            canonical(Path::new(MP))
        )),
        "ProgramArguments is [<mp>, daemon, run]: the foreground daemon, argv-split, never a shell string; plist:\n{plist}"
    );
    assert!(
        plist.contains("<key>RunAtLoad</key>\n\t<true/>"),
        "RunAtLoad is true, which is what makes it a login-start agent"
    );
    assert!(
        plist.contains("<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>"),
        "KeepAlive restarts a crash but respects `mp daemon stop`, which exits 0"
    );
    assert!(
        !plist.contains("daemon</string>\n\t\t<string>start"),
        "and it never runs the detached start"
    );

    if on_path("plutil") {
        let lint = Command::new("plutil")
            .args(["-lint", &path.display().to_string()])
            .output()
            .expect("run plutil -lint");
        assert!(
            lint.status.success(),
            "plutil -lint accepts the plist: {}{}",
            String::from_utf8_lossy(&lint.stdout),
            String::from_utf8_lossy(&lint.stderr)
        );
    } else {
        eprintln!(
            "skipping `plutil -lint`: not on PATH (this is the Linux host the plan escalates)"
        );
    }
}

/// The darwin half prints the `launchctl` line it would run.
#[test]
fn darwin_install_prints_the_bootstrap_line_it_would_run() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .mp_darwin()
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        stdout_lines(&out),
        vec![
            format!("\u{2713} wrote {}", sandbox.plist_path().display()),
            format!(
                "  launchctl bootstrap gui/{} {}",
                uid(),
                sandbox.plist_path().display()
            ),
            DRY_RUN_LINE.to_string(),
        ],
        "bootstrap into the user's own GUI domain, by uid; stderr: {}",
        stderr_text(&out)
    );
}

/// And the bootout line on the way out, which targets the label rather than
/// the file.
#[test]
fn darwin_uninstall_removes_the_plist_and_prints_the_bootout_line() {
    let sandbox = Sandbox::new();
    assert_eq!(
        code(
            &sandbox
                .mp_darwin()
                .args(["daemon", "install-service"])
                .output()
                .expect("install")
        ),
        0
    );

    let out = sandbox
        .mp_darwin()
        .args(["daemon", "uninstall-service"])
        .output()
        .expect("run mp daemon uninstall-service");
    assert_eq!(
        code(&out),
        0,
        "uninstall exits 0; stderr: {}",
        stderr_text(&out)
    );
    assert_eq!(
        stdout_lines(&out),
        vec![
            format!("\u{2713} removed {}", sandbox.plist_path().display()),
            format!("  launchctl bootout gui/{}/{LAUNCHD_LABEL}", uid()),
            DRY_RUN_LINE.to_string(),
        ],
        "bootout takes the service target, not the path"
    );
    assert!(
        !sandbox.plist_path().exists(),
        "and the plist is gone from {}",
        sandbox.plist_path().display()
    );
}

/// Each half writes its own file and never the other's.
#[test]
fn each_os_writes_only_its_own_half() {
    let linux = Sandbox::new();
    assert_eq!(code(&linux.install(&[])), 0);
    assert!(linux.unit_path().exists(), "linux writes the systemd unit");
    assert!(
        !linux.home().join("Library").exists(),
        "and creates no ~/Library at all"
    );

    let darwin = Sandbox::new();
    assert_eq!(
        code(
            &darwin
                .mp_darwin()
                .args(["daemon", "install-service"])
                .output()
                .expect("install")
        ),
        0
    );
    assert!(
        darwin.plist_path().exists(),
        "darwin writes the launch agent"
    );
    assert!(
        !darwin.xdg_config_home().join("systemd").exists(),
        "and writes nothing under $XDG_CONFIG_HOME/systemd"
    );
}

/// The override defaults to this build's target OS, and refuses anything it
/// does not know.
#[test]
fn the_os_override_defaults_to_the_host_and_refuses_an_unknown_value() {
    let sandbox = Sandbox::new();

    let out = sandbox
        .mp()
        .env_remove(OS_ENV)
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        code(&out),
        0,
        "unset means the target OS; stderr: {}",
        stderr_text(&out)
    );
    if cfg!(target_os = "macos") {
        assert!(
            sandbox.plist_path().exists(),
            "a macOS build with no override installs the launch agent"
        );
    } else {
        assert!(
            sandbox.unit_path().exists(),
            "a Linux build with no override installs the systemd unit"
        );
    }

    let other = Sandbox::new();
    let out = other
        .mp()
        .env(OS_ENV, "plan9")
        .args(["daemon", "install-service"])
        .output()
        .expect("run mp daemon install-service");
    assert_eq!(
        code(&out),
        1,
        "an unrecognised {OS_ENV} is an error rather than a silent default; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = stderr_text(&out);
    assert!(
        stderr.contains(OS_ENV) && stderr.contains("linux") && stderr.contains("darwin"),
        "and names the variable and the two values it takes; stderr: {stderr}"
    );
    assert!(
        !other.unit_path().exists() && !other.plist_path().exists(),
        "a refused run installs nothing"
    );
}

// ===========================================================================
// 4. Regression
// ===========================================================================

/// Neither command auto-starts a daemon.
///
/// This row passes today, by construction: `needs_daemon` answers `false` for
/// the whole `daemon` family. It is here because P6-U6 adds two names to that
/// family, and a command that installs a login service by first starting a
/// daemon would be absurd - the file it writes is the thing that starts one.
#[test]
fn neither_service_command_is_on_the_daemon_list() {
    for sub in ["install-service", "uninstall-service"] {
        assert!(
            !needs_daemon(Some("daemon"), Some(sub)),
            "`mp daemon {sub}` needs no daemon"
        );
    }
}
