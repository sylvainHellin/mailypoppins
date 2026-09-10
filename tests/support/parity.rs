//! The parity harness: run one command twice and prove the bytes are the same.
//!
//! Phase 4 of `.agents/workflow/native-gui-daemon/plan.md` moves command after
//! command from "answered in this process" to "answered by the daemon". The
//! gate on every one of those slices is behaviour parity, and the only
//! definition of parity that does not drift is the pre-daemon binary's own
//! output. So this module gives a slice three things:
//!
//! - [`DaemonFixture`] - a live `mp daemon run` against a temporary root, and
//!   an `mp` client pointed at the same root.
//! - [`oracle`] - the binary built from the `pre-daemon` tag (`f8af44b`), run
//!   against **the same** root, so a path that appears in the output appears
//!   identically on both sides.
//! - [`assert_byte_identical`] - stdout, stderr and exit code, with a readable
//!   failure when they differ.
//!
//! # Why one root for both binaries
//!
//! Most of what `mp` prints is fixture-relative: a config path, a store that
//! does not exist yet, a warning naming the file it could not find. Running the
//! two binaries against two roots would make every one of those a false
//! difference, and papering over it with a path rewrite would mean the harness
//! decided what counts as the same. One root, and the comparison is literal.
//!
//! Nothing in the harness runs the two binaries concurrently, so the shared
//! root is not a race: [`DaemonFixture::mp`] and [`oracle`] each return after
//! their child has exited.
//!
//! # The oracle binary, and where it comes from
//!
//! Resolution order, once per process ([`oracle_bin`]):
//!
//! 1. `$MP_ORACLE_BIN`, if it names an existing file. CI and bisects set this.
//! 2. `~/.cache/mp-oracle/pre-daemon/mp`, if it is already there.
//! 3. A build of the `pre-daemon` tag, into that path.
//!
//! Step 3 costs about 75 s on a warm cargo registry and happens at most once
//! per machine, under an exclusive `flock` on `~/.cache/mp-oracle/build.lock`,
//! so a parallel test suite builds it once and the rest wait. Every external
//! step runs under `timeout(1)`: a git or cargo invocation that hangs must fail
//! the test rather than the session. The procedure is written up in
//! `docs/baselines/pre-daemon/README.md` and `docs/daemon-operations.md`.
//!
//! The cache is deliberately outside the repository. It holds a second checkout
//! and a release target directory, roughly 1.5 GB, and a `git clean` in the
//! work tree must not throw that away.
//!
//! # Helpers a later slice will reuse
//!
//! - [`sandbox_env`] - the environment builder. `HOME`, `MAILYPOPPINS_DATA_DIR`
//!   and `MAILYPOPPINS_CONFIG_DIR` all point at the root, and every daemon
//!   environment hook ([`DAEMON_ENV_HOOKS`]) is explicitly removed, so an
//!   exported variable in a developer's shell cannot change an outcome.
//! - [`mp_command`] / [`oracle_command`] - a sandboxed [`Command`] for either
//!   binary, for a slice that needs stdin, a working directory or a hook the
//!   two convenience wrappers do not take.
//! - [`socket_path`] - `<root>/runtime/daemon.sock`, the layout plan section
//!   3.0 fixes.
//! - [`MP`] - the just-built `mp` under test.
//!
//! # Running without a daemon (P4-U2)
//!
//! Three helpers cover the other half of the migration, the one where no
//! daemon is listening:
//!
//! - [`mp_no_daemon`] runs the client with `MAILYPOPPINS_DAEMON_AUTOSTART=0`,
//!   so a command that would have started one exits 4 instead. That is what a
//!   no-daemon-list assertion needs: "it ran, and nothing appeared".
//! - [`mp_autostart`] runs it with auto-start on and the bound set through
//!   `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS`, so a test waits milliseconds
//!   rather than the five-second default.
//! - [`stop_daemon`] ends whatever the auto-start left behind. It has to be a
//!   command rather than a kill, because a daemon this process did not spawn
//!   is nobody's child: `mp daemon run` detaches into its own session.
//! - [`SandboxRoot`] is that same stop, tied to a temporary tree's lifetime,
//!   for a suite that never asked for a daemon and got one anyway.
//!
//! # Suites that auto-start a daemon without meaning to
//!
//! A suite written before Phase 4 runs `mp` against a temp tree and drops the
//! tree when the test ends. Since P4-U2 that same `mp` starts a daemon on
//! demand, and the daemon outlives the test, the tree and the test binary: a
//! full `cargo test --workspace` left eleven of them behind, each holding a
//! deleted directory open. [`SandboxRoot`] owns the [`TempDir`] instead and
//! stops that daemon on the way out, so the suites keep their assertions and
//! stop leaking processes.
//!
//! [`TempDir`]: tempfile::TempDir
//!
//! [`DaemonFixture::start_in`] and [`DaemonFixture::mp_in`] give the daemon
//! and the client different working directories, which is how a test proves
//! the daemon's own cwd carries no meaning: start it from `/`, run the client
//! from a temp directory, and a relative path still resolves under the temp
//! directory.
//!
//! [`DaemonFixture::mp_routed`] sets `MAILYPOPPINS_DAEMON_REQUIRE=1`, which
//! makes a command that answered in process fail loudly instead of passing a
//! parity comparison it never earned. Until a slice migrates a command, every
//! command fails under it; that is the point of having it before the slices.
//!
//! # Process hygiene
//!
//! [`DaemonFixture`] kills and reaps its daemon on [`DaemonFixture::stop`] and
//! again in `Drop`, so a panicking assertion leaks nothing. `stop` waits for
//! the process to be gone rather than for the kill to return.

#![allow(dead_code)]

use std::fs;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The binary under test: the `mp` cargo just built for this test run.
pub const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Where [`oracle`] looks first.
pub const ORACLE_BIN_ENV: &str = "MP_ORACLE_BIN";

/// The tag the oracle is built from. `git rev-parse pre-daemon` is `f8af44b`.
pub const ORACLE_TAG: &str = "pre-daemon";

/// Upper bound on a readiness wait or a shutdown wait. A ceiling, never a
/// sleep: every wait below polls and returns as soon as it can.
pub const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
pub const TICK: Duration = Duration::from_millis(25);

/// Seconds allowed for one `git` step of the oracle build.
const GIT_TIMEOUT_SECS: &str = "180";

/// Seconds allowed for the oracle's `cargo build --release`. A cold registry
/// build of this tree is about 75 s; the ceiling is generous because exceeding
/// it means something is wedged, not slow.
const CARGO_TIMEOUT_SECS: &str = "1800";

/// Seconds a test waits for another process to finish building the oracle.
const LOCK_TIMEOUT_SECS: u64 = 2100;

/// Every daemon environment hook `docs/daemon-operations.md` documents, removed
/// from any child this module spawns.
pub const DAEMON_ENV_HOOKS: [&str; 13] = [
    "MAILYPOPPINS_DAEMON_AUTOSTART",
    "MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS",
    "MAILYPOPPINS_DAEMON_REQUIRE",
    "MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES",
    "MAILYPOPPINS_DAEMON_FAIL_START",
    "MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS",
    "MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST",
    "MAILYPOPPINS_DAEMON_FAKE_OPERATIONS",
    "MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME",
    "MAILYPOPPINS_DAEMON_WATCH_POLL_MS",
    "MAILYPOPPINS_DAEMON_WATCH_DEBOUNCE_MS",
    "MAILYPOPPINS_DAEMON_HANDLE_TTL_MS",
    "MAILYPOPPINS_DAEMON_START_LOCK_HELD",
];

// ---------------------------------------------------------------------------
// The sandbox
// ---------------------------------------------------------------------------

/// The daemon's socket under a root used as the data directory.
pub fn socket_path(root: &Path) -> PathBuf {
    root.join("runtime").join("daemon.sock")
}

/// Point `cmd` at `root` as `HOME`, data root and config root, and clear every
/// daemon hook.
pub fn sandbox_env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("MAILYPOPPINS_DATA_DIR", root)
        .env("MAILYPOPPINS_CONFIG_DIR", root);
    for hook in DAEMON_ENV_HOOKS {
        cmd.env_remove(hook);
    }
    cmd
}

/// A sandboxed invocation of the binary under test, with its stdio piped.
pub fn mp_command(root: &Path) -> Command {
    let mut cmd = Command::new(MP);
    sandbox_env(&mut cmd, root);
    cmd.stdin(Stdio::null());
    cmd
}

/// A sandboxed invocation of the pre-daemon oracle, with its stdio piped.
pub fn oracle_command(root: &Path) -> Command {
    let mut cmd = Command::new(oracle_bin());
    sandbox_env(&mut cmd, root);
    cmd.stdin(Stdio::null());
    cmd
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A running `mp daemon run` and the client that talks to the same root.
///
/// `start` returns only once the socket accepts a connection, so a test never
/// races the daemon's bind. The daemon dies with the fixture, whether the test
/// called [`DaemonFixture::stop`] or panicked before it could.
pub struct DaemonFixture {
    root: PathBuf,
    child: Option<Child>,
    pid: u32,
}

impl DaemonFixture {
    /// Boot a daemon against `tmp`, used as both the data and the config root.
    pub fn start(tmp: &Path) -> Self {
        Self::start_in(tmp, None)
    }

    /// Boot a daemon against `tmp` from a chosen working directory.
    ///
    /// `cwd` exists for one assertion: a daemon started from `/` must answer
    /// exactly as one started from the test's temp directory, because the
    /// client resolves every path before it crosses the socket. A daemon whose
    /// cwd mattered would produce a different answer here, which is the
    /// failure P4-U2's absolutisation rule prevents.
    pub fn start_in(tmp: &Path, cwd: Option<&Path>) -> Self {
        fs::create_dir_all(tmp).unwrap_or_else(|e| panic!("create {}: {e}", tmp.display()));
        let mut cmd = mp_command(tmp);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let child = cmd
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn `{MP} daemon run`: {e}"));
        let pid = child.id();
        let fixture = DaemonFixture {
            root: tmp.to_path_buf(),
            child: Some(child),
            pid,
        };
        fixture.wait_ready();
        fixture
    }

    /// The root this fixture owns, which is also what [`oracle`] must be given.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The daemon's pid, so a test can assert it is gone.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Run the client against the same root and collect its output.
    pub fn mp(&self, args: &[&str]) -> Output {
        mp_command(&self.root)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run `mp {}`: {e}", args.join(" ")))
    }

    /// Run the client from `cwd`, against the same root.
    ///
    /// The client is the process with a meaningful working directory, so this
    /// is where a relative `-o` or a relative `attachments:` entry is anchored.
    pub fn mp_in(&self, cwd: &Path, args: &[&str]) -> Output {
        mp_command(&self.root)
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run `mp {}` in {}: {e}", args.join(" "), cwd.display()))
    }

    /// Run the client with [`REQUIRE_ENV`] set, so a command that answered
    /// from its own process fails instead of passing for the daemon's work.
    ///
    /// A slice migrating a command switches its parity assertion from [`mp`]
    /// to this the moment the command routes, and the assertion stops being a
    /// tautology.
    ///
    /// [`mp`]: DaemonFixture::mp
    pub fn mp_routed(&self, args: &[&str]) -> Output {
        mp_command(&self.root)
            .env(REQUIRE_ENV, "1")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("run routed `mp {}`: {e}", args.join(" ")))
    }

    /// Terminate the daemon and wait until it is really gone.
    pub fn stop(mut self) {
        self.terminate();
        let start = Instant::now();
        while process_is_alive(self.pid) {
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon (pid {}) was still alive {DEADLINE:?} after being killed",
                self.pid
            );
            std::thread::sleep(TICK);
        }
    }

    fn wait_ready(&self) {
        let socket = socket_path(&self.root);
        let start = Instant::now();
        loop {
            if UnixStream::connect(&socket).is_ok() {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                socket.display()
            );
            std::thread::sleep(TICK);
        }
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        self.terminate();
    }
}

// ---------------------------------------------------------------------------
// Running without a daemon
// ---------------------------------------------------------------------------

/// `MAILYPOPPINS_DAEMON_AUTOSTART`: `0` turns on-demand starting off.
pub const AUTOSTART_ENV: &str = "MAILYPOPPINS_DAEMON_AUTOSTART";

/// `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS`: the whole auto-start budget.
pub const AUTOSTART_TIMEOUT_ENV: &str = "MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS";

/// `MAILYPOPPINS_DAEMON_REQUIRE`: fail a command that did not route.
pub const REQUIRE_ENV: &str = "MAILYPOPPINS_DAEMON_REQUIRE";

/// The exit code of a client that could not reach a daemon (plan section 3.0).
pub const EXIT_UNAVAILABLE: i32 = 4;

/// Run `mp` against `root` with no daemon listening and auto-start off.
///
/// The assertion this enables is "the command ran, and no daemon appeared":
/// with auto-start on, a command that needs one would start it and the test
/// could no longer tell a no-daemon-list command from a migrated one.
pub fn mp_no_daemon(args: &[&str], root: &Path) -> Output {
    fs::create_dir_all(root).unwrap_or_else(|e| panic!("create {}: {e}", root.display()));
    mp_command(root)
        .env(AUTOSTART_ENV, "0")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run `mp {}` without a daemon: {e}", args.join(" ")))
}

/// Run `mp` against `root` with auto-start on and the whole budget set to
/// `budget_ms`, from `cwd` when one is given.
///
/// The budget is explicit because the default is five seconds and a test that
/// expects a failure would otherwise pay all of it; a test that expects a
/// success needs it long enough for a real daemon to bind, which is well under
/// a second on this tree.
pub fn mp_autostart(args: &[&str], root: &Path, budget_ms: u64, cwd: Option<&Path>) -> Output {
    fs::create_dir_all(root).unwrap_or_else(|e| panic!("create {}: {e}", root.display()));
    let mut cmd = mp_command(root);
    cmd.env(AUTOSTART_TIMEOUT_ENV, budget_ms.to_string());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.args(args)
        .output()
        .unwrap_or_else(|e| panic!("run `mp {}` with auto-start: {e}", args.join(" ")))
}

/// Stop whatever daemon is running against `root`, and wait until the socket
/// is gone.
///
/// A daemon an auto-start produced is not this process's child: `mp daemon
/// run` detaches into its own session, which is what keeps it alive after the
/// client that wanted it exits. So it is ended the way a user would end it,
/// through `mp daemon stop`, rather than with a signal to a pid nobody owns.
pub fn stop_daemon(root: &Path) -> Output {
    let out = mp_command(root)
        .env(AUTOSTART_ENV, "0")
        .args(["daemon", "stop"])
        .output()
        .unwrap_or_else(|e| panic!("run `mp daemon stop` against {}: {e}", root.display()));
    let socket = socket_path(root);
    let start = Instant::now();
    while socket.exists() {
        assert!(
            start.elapsed() < DEADLINE,
            "`mp daemon stop` returned but {} is still there after {DEADLINE:?}",
            socket.display()
        );
        std::thread::sleep(TICK);
    }
    out
}

/// A temporary tree that stops whatever daemon ran against it.
///
/// The data directory is kept separately from the tree root because the legacy
/// suites use a split layout (`<root>/data`, `<root>/config`) while the parity
/// suites point everything at one root, and the socket lives under the *data*
/// directory either way.
///
/// [`Drop`] is best-effort by construction: it spawns `mp daemon stop` only
/// when a socket is there to stop, and it never asserts, because a panic while
/// another panic unwinds aborts the process and would hide the assertion the
/// test was actually about.
pub struct SandboxRoot {
    data: PathBuf,
    tmp: tempfile::TempDir,
}

impl SandboxRoot {
    /// A tree whose data directory is somewhere under it.
    pub fn new(tmp: tempfile::TempDir, data: impl Into<PathBuf>) -> SandboxRoot {
        SandboxRoot {
            data: data.into(),
            tmp,
        }
    }

    /// A tree that is its own data directory, the one-root layout
    /// [`sandbox_env`] sets up.
    pub fn single(tmp: tempfile::TempDir) -> SandboxRoot {
        let data = tmp.path().to_path_buf();
        SandboxRoot::new(tmp, data)
    }

    /// A fresh one-root tree.
    pub fn fresh() -> SandboxRoot {
        SandboxRoot::single(tempfile::TempDir::new().expect("a temporary sandbox root"))
    }

    /// The tree root, which is what `TempDir::path` returned before the guard
    /// was introduced.
    pub fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// The directory the daemon's runtime lives under.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }
}

impl Drop for SandboxRoot {
    fn drop(&mut self) {
        if !socket_path(&self.data).exists() {
            return;
        }
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.tmp.path())
            .env("MAILYPOPPINS_DATA_DIR", &self.data)
            .env(AUTOSTART_ENV, "0")
            .args(["daemon", "stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = cmd.status();
    }
}

/// Whether anything currently accepts a connection on `root`'s socket.
pub fn daemon_is_listening(root: &Path) -> bool {
    UnixStream::connect(socket_path(root)).is_ok()
}

/// Every `mp daemon run` this host can see, as `pgrep -af` reports it: one
/// line per process, pid first.
///
/// The bracket in the pattern is the usual trick that keeps `pgrep` from
/// matching its own command line.
pub fn daemon_pgrep_lines() -> Vec<String> {
    let out = Command::new("pgrep")
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
/// because it is neither: a test binary runs on several threads, so a sibling
/// test's fixture is legitimately in the list while this one checks, and so is
/// a daemon the developer started by hand. Neither is a leak the caller
/// caused, and a set difference taken across two `pgrep` calls races the
/// siblings instead.
pub fn pgrep_line_for(pid: u32) -> Option<String> {
    daemon_pgrep_lines().into_iter().find(|line| {
        line.split_whitespace()
            .next()
            .and_then(|first| first.parse::<u32>().ok())
            == Some(pid)
    })
}

/// The pid in `<root>/runtime/daemon.pid`, if a daemon wrote one.
///
/// Diagnostic only, exactly as the runtime layout says: it answers "which
/// process did the auto-start produce", which is what a leak assertion needs,
/// and never "is a daemon running", which is the socket's question.
pub fn daemon_pid_file(root: &Path) -> Option<u32> {
    fs::read_to_string(root.join("runtime").join("daemon.pid"))
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
}

/// Whether `pid` still names a live or unreaped process.
///
/// The daemon is a direct child and `stop` reaps it, so this is `false` rather
/// than "true forever as a zombie" once the wait has run.
pub fn process_is_alive(pid: u32) -> bool {
    // Safety: signal 0 performs the permission and existence check and delivers
    // nothing.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

// ---------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------

/// Run the pre-daemon binary against `tmp` and collect its output.
pub fn oracle(args: &[&str], tmp: &Path) -> Output {
    fs::create_dir_all(tmp).unwrap_or_else(|e| panic!("create {}: {e}", tmp.display()));
    oracle_command(tmp)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run oracle `mp {}`: {e}", args.join(" ")))
}

/// The oracle binary, resolved once per process and built if it is missing.
pub fn oracle_bin() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(resolve_oracle_bin).as_path()
}

/// `~/.cache/mp-oracle`, the build cache root.
pub fn oracle_cache_dir() -> PathBuf {
    dirs::home_dir()
        .expect("a home directory, which is where the oracle cache lives")
        .join(".cache")
        .join("mp-oracle")
}

fn resolve_oracle_bin() -> PathBuf {
    if let Some(from_env) = std::env::var_os(ORACLE_BIN_ENV) {
        let path = PathBuf::from(from_env);
        assert!(
            path.is_file(),
            "{ORACLE_BIN_ENV} is set to {}, which is not a file. Unset it to build the \
             `{ORACLE_TAG}` tag instead.",
            path.display()
        );
        return path;
    }

    let cache = oracle_cache_dir();
    let cached = cache.join(ORACLE_TAG).join("mp");
    if cached.is_file() {
        return cached;
    }
    build_oracle(&cache, &cached)
}

fn build_oracle(cache: &Path, dest: &Path) -> PathBuf {
    fs::create_dir_all(cache).unwrap_or_else(|e| panic!("create {}: {e}", cache.display()));
    let _lock = BuildLock::acquire(&cache.join("build.lock"));

    // Another process may have built it while we waited for the lock.
    if dest.is_file() {
        return dest.to_path_buf();
    }

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = cache.join("src");
    let target = cache.join("target");
    materialise_oracle_source(repo, &src);

    let mut cargo = Command::new("timeout");
    cargo
        .arg(CARGO_TIMEOUT_SECS)
        .args(["cargo", "build", "--release", "--offline", "--locked"])
        .current_dir(&src)
        .env("CARGO_TARGET_DIR", &target);
    run_step("cargo build --release (oracle)", &mut cargo);

    let built = target.join("release").join("mp");
    assert!(
        built.is_file(),
        "the oracle build finished but produced no binary at {}.\n\
         Build it by hand with the procedure in docs/baselines/pre-daemon/README.md, or point \
         {ORACLE_BIN_ENV} at a `{ORACLE_TAG}` build.",
        built.display()
    );

    let parent = dest.parent().expect("the oracle path has a parent");
    fs::create_dir_all(parent).unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
    fs::copy(&built, dest)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", built.display(), dest.display()));
    assert!(
        dest.is_file(),
        "neither {ORACLE_BIN_ENV} nor a build of `{ORACLE_TAG}` produced an oracle at {}",
        dest.display()
    );
    dest.to_path_buf()
}

/// Put the `pre-daemon` tree at `src`, by worktree if it is not there yet and
/// by `git archive` if something already is.
fn materialise_oracle_source(repo: &Path, src: &Path) {
    if src.join("Cargo.toml").is_file() {
        return;
    }
    if !src.exists() {
        let mut worktree = Command::new("timeout");
        worktree
            .arg(GIT_TIMEOUT_SECS)
            .args(["git", "worktree", "add", "--detach"])
            .arg(src)
            .arg(ORACLE_TAG)
            .current_dir(repo);
        if worktree
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }
        // A worktree for this tag may already be registered elsewhere, which is
        // not a reason to fail: an export of the tag is just as good an oracle
        // source, since nothing here ever commits from it.
    }
    fs::create_dir_all(src).unwrap_or_else(|e| panic!("create {}: {e}", src.display()));
    let mut archive = Command::new("sh");
    archive
        .arg("-c")
        .arg(format!(
            "timeout {GIT_TIMEOUT_SECS} git archive {ORACLE_TAG} | tar -x -C {}",
            shell_quote(src)
        ))
        .current_dir(repo);
    run_step("git archive pre-daemon | tar -x", &mut archive);
    assert!(
        src.join("Cargo.toml").is_file(),
        "exporting `{ORACLE_TAG}` into {} produced no Cargo.toml",
        src.display()
    );
}

fn run_step(what: &str, cmd: &mut Command) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("the oracle build could not run `{what}`: {e}"));
    assert!(
        out.status.success(),
        "the oracle build step `{what}` failed with {}.\n\
         Reproduce it with the procedure in docs/baselines/pre-daemon/README.md, or point \
         {ORACLE_BIN_ENV} at a `{ORACLE_TAG}` build.\n\
         --- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// An exclusive advisory lock on the oracle cache, so parallel tests build once.
struct BuildLock(fs::File);

impl BuildLock {
    fn acquire(path: &Path) -> Self {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .unwrap_or_else(|e| panic!("open the oracle build lock {}: {e}", path.display()));
        let start = Instant::now();
        loop {
            // Safety: `flock` on a descriptor this process owns; released by
            // the kernel when the file is closed, however this process dies.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return BuildLock(file);
            }
            assert!(
                start.elapsed() < Duration::from_secs(LOCK_TIMEOUT_SECS),
                "another process has held the oracle build lock {} for over {LOCK_TIMEOUT_SECS}s",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

// ---------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------

/// Assert that two runs produced the same stdout, the same stderr and the same
/// exit code.
///
/// The convention in this suite is `assert_byte_identical(&daemon, &oracle)`,
/// so "left" in a failure is the build under test and "right" is the
/// pre-daemon behaviour it must reproduce.
///
/// A mismatch names the stream, prints both values in full and, when both are
/// valid UTF-8, a unified-diff-style excerpt around the first differing line.
/// Binary output falls back to a length and a first-differing-byte report,
/// because a lossy rendering of bytes that differ is exactly the case where a
/// lossy rendering lies.
pub fn assert_byte_identical(left: &Output, right: &Output) {
    let mut failures = Vec::new();
    if left.status.code() != right.status.code() {
        failures.push(format!(
            "exit code differs:\n  left  = {}\n  right = {}",
            describe_status(left),
            describe_status(right)
        ));
    }
    if left.stdout != right.stdout {
        failures.push(format!(
            "stdout differs:\n{}",
            stream_report(&left.stdout, &right.stdout)
        ));
    }
    if left.stderr != right.stderr {
        failures.push(format!(
            "stderr differs:\n{}",
            stream_report(&left.stderr, &right.stderr)
        ));
    }
    if failures.is_empty() {
        return;
    }
    panic!(
        "outputs are not byte-identical ({} of 3 differ)\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

fn describe_status(out: &Output) -> String {
    match out.status.code() {
        Some(code) => code.to_string(),
        None => format!("{} (killed by a signal)", out.status),
    }
}

fn stream_report(left: &[u8], right: &[u8]) -> String {
    match (std::str::from_utf8(left), std::str::from_utf8(right)) {
        (Ok(left), Ok(right)) => format!(
            "  left  ({} bytes):\n{}\n  right ({} bytes):\n{}\n  diff (-left +right):\n{}",
            left.len(),
            indent(left),
            right.len(),
            indent(right),
            unified_excerpt(left, right)
        ),
        _ => {
            let at = left
                .iter()
                .zip(right.iter())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| left.len().min(right.len()));
            format!(
                "  not UTF-8 on at least one side; left is {} bytes, right is {} bytes, \
                 first difference at byte {at}",
                left.len(),
                right.len()
            )
        }
    }
}

fn indent(text: &str) -> String {
    if text.is_empty() {
        return "    <empty>".to_string();
    }
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A unified-diff-style excerpt: three lines of context before the first
/// difference, then up to twenty lines of it.
fn unified_excerpt(left: &str, right: &str) -> String {
    const CONTEXT: usize = 3;
    const WINDOW: usize = 20;

    let left: Vec<&str> = left.lines().collect();
    let right: Vec<&str> = right.lines().collect();
    let first = (0..left.len().max(right.len()))
        .find(|&i| left.get(i) != right.get(i))
        .unwrap_or(0);
    let from = first.saturating_sub(CONTEXT);
    let to = (first + WINDOW).min(left.len().max(right.len()));

    let mut out = vec![format!("    @@ line {} @@", first + 1)];
    for i in from..to {
        match (left.get(i), right.get(i)) {
            (Some(l), Some(r)) if l == r => out.push(format!("     {l}")),
            (l, r) => {
                if let Some(l) = l {
                    out.push(format!("    -{l}"));
                }
                if let Some(r) = r {
                    out.push(format!("    +{r}"));
                }
            }
        }
    }
    if to < left.len().max(right.len()) {
        out.push("    @@ … @@".to_string());
    }
    out.join("\n")
}
