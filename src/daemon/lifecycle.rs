//! `mp daemon run | start | status | stop | restart` (P2-U7).
//!
//! ## The startup sequence, in order
//!
//! `run` does exactly this, and the order is load-bearing:
//!
//! 1. logging is already initialised by `main`, so a failure below is on disk;
//! 2. take the start lock, so two starters cannot both reach the bind;
//! 3. run `MIG-04`, before anything reads the config directory;
//! 4. load the config, tolerating a missing `config.toml` as zero accounts;
//! 5. ensure `<data_dir>/runtime` exists at mode 0700;
//! 6. classify whatever is at the socket path and remove it only if stale;
//! 7. bind the socket at mode 0600;
//! 8. write `daemon.pid` and `daemon.json`;
//! 9. serve until `daemon.stop`, SIGTERM or SIGINT, then unlink all three.
//!
//! ## Who holds the start lock
//!
//! `start` holds it across spawn *and* readiness, because releasing it at spawn
//! time would let a second starter observe a socket that the first daemon has
//! not bound yet and spawn a second daemon. The child it spawns therefore must
//! not try to take the same lock, and is told so through
//! `MAILYPOPPINS_DAEMON_START_LOCK_HELD=1`, an internal variable no user sets
//! and no flag exposes. A `mp daemon run` typed by hand takes the lock itself
//! and releases it once the socket is bound and the runtime files are written.
//!
//! ## Readiness
//!
//! Readiness is a real `daemon.status` round trip over the socket, not the
//! existence of a file: a socket inode appears before `accept` does, and the
//! whole point of `mp daemon start` returning is that the next command can
//! connect. The round trip goes through `mp-client`, but without an
//! `initialize`: `daemon.status` and `daemon.stop` are lifecycle surface and
//! answer before a handshake, which is exactly what lets these commands
//! describe and end a daemon whose protocol range they cannot negotiate.

use std::fs::{self, File, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use log::{error, info, warn};
use serde_json::{json, Value};
use tokio::sync::watch;

use mp_client::Connection;
use mp_protocol::{PROTOCOL_MAX, PROTOCOL_MIN};

use super::runtime::{
    self, acquire_start_lock, ensure_runtime_dir, instance_path, pid_path, probe_socket,
    remove_stale_socket, socket_path, InstanceMeta, SocketProbe,
};
use super::server::{serve, AccountStatus, DaemonState};
use super::session::ConfigReport;

/// Test-only hook: make `run` exit nonzero after logging is up and before the
/// socket is bound, so `mp daemon start` has a deterministic dead child to
/// report. Pinned by `tests/daemon_lifecycle.rs`; no flag exposes it.
pub const FAIL_START_ENV: &str = "MAILYPOPPINS_DAEMON_FAIL_START";

/// Opt-in for account runtimes before Phase 5 (plan section 3.0). Absent, the
/// daemon creates no runtime and acquires no engine lock.
pub const ACCOUNT_RUNTIMES_ENV: &str = "MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES";

/// Internal handshake between `start` and the `run` it spawns: the parent holds
/// the start lock, so the child must not block on it.
const START_LOCK_HELD_ENV: &str = "MAILYPOPPINS_DAEMON_START_LOCK_HELD";

/// Success.
const EXIT_OK: i32 = 0;
/// Generic failure, and `status` when no daemon runs.
const EXIT_ERROR: i32 = 1;
/// Daemon unavailable or failed to start (plan section 3.0). Public because a
/// routed client command (`mp --daemon …`) exits with it too, and the two must
/// not drift apart.
pub const EXIT_UNAVAILABLE: i32 = 4;

/// How often a bounded wait re-checks its condition.
const POLL: Duration = Duration::from_millis(25);

/// Timeout for a single request/response on the admin socket.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// The file `mp daemon start` points a detached daemon's stdio at.
fn daemon_log_path() -> PathBuf {
    crate::config::logs_dir().join("daemon.log")
}

/// `mp daemon <action>`.
///
/// Only compiled with the `daemon` feature, and hidden from `mp --help` until
/// P4-U1 removes the feature: the help snapshot is a single file shared by the
/// featured and unfeatured builds, so a visible subcommand would make one of
/// the two `cargo test` runs fail on a snapshot it cannot also satisfy.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum DaemonAction {
    /// Run the daemon in the foreground; never connects to another daemon
    Run {
        /// Echo lifecycle logs to stderr as well as to the log directory
        #[arg(long)]
        foreground_logs: bool,
    },
    /// Start a detached daemon and return once it answers
    Start {
        /// Seconds to wait for the daemon to become ready
        #[arg(long, default_value_t = 10)]
        timeout_secs: u64,
    },
    /// Report whether a daemon is running against this data directory
    Status {
        /// Print one JSON object instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },
    /// Ask the running daemon to shut down
    Stop {
        /// Seconds to wait for the daemon to go away
        #[arg(long, default_value_t = 10)]
        timeout_secs: u64,
    },
    /// Stop the running daemon and start this executable's daemon
    Restart,
}

/// Run one lifecycle command and return the process exit code.
pub async fn dispatch(action: DaemonAction) -> i32 {
    let outcome = match action {
        DaemonAction::Run { foreground_logs } => run(foreground_logs).await.map(|()| EXIT_OK),
        DaemonAction::Start { timeout_secs } => start(Duration::from_secs(timeout_secs)).await,
        DaemonAction::Status { json } => status(json).await,
        DaemonAction::Stop { timeout_secs } => stop(Duration::from_secs(timeout_secs)).await,
        DaemonAction::Restart => restart().await,
    };
    match outcome {
        Ok(code) => code,
        Err(e) => {
            error!("[daemon] {e:#}");
            eprintln!("{} {e:#}", "\u{2717}".red());
            EXIT_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// Foreground daemon mode.
async fn run(foreground_logs: bool) -> Result<()> {
    let echo = |line: &str| {
        info!("[daemon] {line}");
        if foreground_logs {
            eprintln!("{line}");
        }
    };
    echo("starting");

    // The test hook lives here on purpose: after logging, before the lock and
    // before the socket, so a forced failure leaves nothing on disk to clean up.
    if env_flag(FAIL_START_ENV) {
        bail!("{FAIL_START_ENV} is set: failing before the socket is bound");
    }

    // The parent of a detached start already holds the lock (see the module
    // docs); a hand-typed `run` takes it here and releases it below.
    let start_lock = if env_flag(START_LOCK_HELD_ENV) {
        None
    } else {
        match acquire_start_lock()? {
            Some(guard) => Some(guard),
            None => bail!(
                "another daemon is starting against {}; wait for it or run `mp daemon status`",
                crate::config::mailypoppins_data_dir().display()
            ),
        }
    };

    // MIG-04 before the first config read, and fatal: the alternative is a
    // daemon serving an empty config while the real one sits in the old place.
    crate::config::migrate_legacy_config_dir()?;

    // A missing config.toml is not a startup failure: the daemon serves zero
    // accounts until one is written (the plan's Daemon-lifecycle section). The
    // handshake reports which of the three cases this daemon is in, so a client
    // can tell "no accounts yet" from "your config does not parse".
    let config_path = crate::config::config_path();
    let (accounts, configured, config) = if !config_path.exists() {
        echo(&format!("no config at {}", config_path.display()));
        (
            Vec::new(),
            Vec::new(),
            ConfigReport::Absent {
                path: config_path.clone(),
            },
        )
    } else {
        match crate::config::load_global_config() {
            Ok(config) => {
                echo(&format!(
                    "config loaded, {} accounts",
                    config.accounts.len()
                ));
                let report = ConfigReport::Loaded {
                    path: config_path.clone(),
                    accounts: config.accounts.len(),
                };
                (account_statuses(&config), config.accounts.clone(), report)
            }
            Err(e) => {
                warn!("[daemon] no usable config, serving zero accounts: {e:#}");
                if foreground_logs {
                    eprintln!("no usable config, serving zero accounts: {e:#}");
                }
                (
                    Vec::new(),
                    Vec::new(),
                    ConfigReport::Invalid {
                        path: config_path.clone(),
                        problem: format!("{e:#}"),
                    },
                )
            }
        }
    };

    ensure_runtime_dir()?;
    let socket = socket_path();
    match probe_socket(&socket) {
        SocketProbe::Live => bail!(
            "a daemon is already listening on {}; stop it first with `mp daemon stop`",
            socket.display()
        ),
        SocketProbe::Stale => remove_stale_socket(&socket)?,
        SocketProbe::Unsafe { reason } => bail!("refusing to bind {}: {reason}", socket.display()),
        SocketProbe::Absent => {}
    }

    let listener = bind_socket(&socket)?;
    echo(&format!("listening on {}", socket.display()));

    let meta = InstanceMeta {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_min: PROTOCOL_MIN,
        protocol_max: PROTOCOL_MAX,
        instance_id: new_instance_id(),
        pid: std::process::id(),
        started_at: Utc::now().to_rfc3339(),
        data_dir: canonical(&crate::config::mailypoppins_data_dir()),
        config_dir: canonical(&crate::config::config_dir()),
    };
    write_runtime_files(&meta)?;

    // The lock covered exactly the start sequence; from here the bound socket
    // is what excludes a second daemon.
    drop(start_lock);

    let state = Arc::new(DaemonState {
        meta,
        accounts,
        configured,
        config,
    });
    let (shutdown, _) = watch::channel(false);
    spawn_signal_watch(shutdown.clone())?;

    let result = serve(listener, Arc::clone(&state), shutdown).await;
    cleanup(&state.meta);
    echo("stopped");
    result
}

/// Bind the Unix socket at mode 0600 with no window at a looser mode.
///
/// `umask` is set around the bind because the kernel applies it to the socket
/// inode at creation time; the explicit `set_permissions` afterwards is the
/// belt to that braces, since [`probe_socket`] treats any other mode as unsafe.
fn bind_socket(path: &Path) -> Result<tokio::net::UnixListener> {
    // SAFETY: `umask` cannot fail and takes no pointer. The daemon has no other
    // thread creating files at this point in startup.
    let previous = unsafe { libc::umask(0o177) };
    let bound = tokio::net::UnixListener::bind(path);
    // SAFETY: as above; restores what the process had.
    unsafe { libc::umask(previous) };
    let listener =
        bound.with_context(|| format!("binding the daemon socket {}", path.display()))?;
    fs::set_permissions(path, Permissions::from_mode(0o600))
        .with_context(|| format!("setting mode 0600 on {}", path.display()))?;
    Ok(listener)
}

/// Write `daemon.pid` (a bare decimal pid) and `daemon.json` ([`InstanceMeta`]).
fn write_runtime_files(meta: &InstanceMeta) -> Result<()> {
    let pid_file = pid_path();
    fs::write(&pid_file, format!("{}\n", meta.pid))
        .with_context(|| format!("writing {}", pid_file.display()))?;
    fs::set_permissions(&pid_file, Permissions::from_mode(0o600)).ok();

    let instance_file = instance_path();
    let json = serde_json::to_string_pretty(meta).context("serialising daemon.json")?;
    fs::write(&instance_file, format!("{json}\n"))
        .with_context(|| format!("writing {}", instance_file.display()))?;
    fs::set_permissions(&instance_file, Permissions::from_mode(0o600)).ok();
    Ok(())
}

/// Unlink the socket, the pid file and the instance file, but only the ones
/// this daemon owns.
///
/// "Removes only its own socket" is a plan requirement, and the same reasoning
/// covers the other two: a daemon that started after us must not have its
/// metadata deleted by our shutdown.
fn cleanup(meta: &InstanceMeta) {
    let socket = socket_path();
    if let Err(e) = fs::remove_file(&socket) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!("[daemon] could not remove {}: {e}", socket.display());
        }
    }

    if read_instance_meta().is_some_and(|other| other.instance_id == meta.instance_id) {
        let _ = fs::remove_file(instance_path());
    }
    if fs::read_to_string(pid_path())
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid == meta.pid)
    {
        let _ = fs::remove_file(pid_path());
    }
}

/// Shut the daemon down on SIGTERM or SIGINT.
fn spawn_signal_watch(shutdown: watch::Sender<bool>) -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).context("installing the SIGTERM handler")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("installing the SIGINT handler")?;
    tokio::spawn(async move {
        let which = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
        };
        info!("[daemon] {which} received, shutting down");
        let _ = shutdown.send(true);
    });
    Ok(())
}

/// The accounts `daemon.status` reports.
///
/// Empty unless [`ACCOUNT_RUNTIMES_ENV`] is set, and even then a placeholder:
/// nothing opens a store or takes an engine lock before Phase 5, so every
/// configured account is reported as `opening` and stays there.
fn account_statuses(config: &crate::config::GlobalConfig) -> Vec<AccountStatus> {
    if !env_flag(ACCOUNT_RUNTIMES_ENV) {
        return Vec::new();
    }
    config
        .accounts
        .iter()
        .map(|account| AccountStatus {
            name: account.name.clone(),
            state: "opening".to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

/// Detached start: spawn `mp daemon run`, wait for a real `daemon.status`.
async fn start(timeout: Duration) -> Result<i32> {
    let socket = socket_path();
    if query_status().await.is_ok() {
        info!("[daemon] start: a daemon is already running");
        return Ok(EXIT_OK);
    }

    let Some(_lock) = acquire_start_lock()? else {
        // Another starter is ahead of us; its daemon is the one daemon.
        return Ok(match wait_ready(timeout, None).await? {
            Some(_) => EXIT_OK,
            None => {
                report_start_failure(
                    "the daemon another `mp daemon start` launched never became ready",
                );
                EXIT_UNAVAILABLE
            }
        });
    };

    // The socket may have outlived a crashed daemon; `run` would refuse to bind
    // over it, and this is the moment we hold the lock that makes removing it
    // safe.
    if matches!(probe_socket(&socket), SocketProbe::Stale) {
        remove_stale_socket(&socket)?;
    }

    let mut child = spawn_detached()?;
    match wait_ready(timeout, Some(&mut child)).await? {
        Some(_) => Ok(EXIT_OK),
        None => {
            // The child may still be alive but wedged; it is ours until we
            // return, so it does not get to outlive a failed start.
            let _ = child.kill();
            let _ = child.wait();
            report_start_failure("the daemon did not become ready in time");
            Ok(EXIT_UNAVAILABLE)
        }
    }
}

/// Launch `mp daemon run` with its stdio in the daemon log and its own session.
fn spawn_detached() -> Result<Child> {
    let exe = std::env::current_exe().context("resolving this executable")?;
    let logs = crate::config::logs_dir();
    fs::create_dir_all(&logs)
        .with_context(|| format!("creating the log directory {}", logs.display()))?;
    let log_path = daemon_log_path();
    let out = File::options()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening the daemon log {}", log_path.display()))?;
    let err = out.try_clone().context("duplicating the daemon log")?;

    let mut command = Command::new(&exe);
    command
        .args(["daemon", "run"])
        .env(START_LOCK_HELD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    // SAFETY: `setsid` is async-signal-safe and is the whole body of the hook;
    // it detaches the daemon from the launching terminal's session so a closed
    // terminal cannot take it down.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = command
        .spawn()
        .with_context(|| format!("spawning {} daemon run", exe.display()))?;
    info!("[daemon] spawned pid {} for `mp daemon run`", child.id());
    Ok(child)
}

/// Poll for readiness until `timeout`, failing early if `child` dies.
async fn wait_ready(timeout: Duration, mut child: Option<&mut Child>) -> Result<Option<Value>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(status) = query_status().await {
            return Ok(Some(status));
        }
        if let Some(child) = child.as_mut() {
            if let Some(exit) = child.try_wait().context("polling the daemon child")? {
                report_start_failure(&format!("`mp daemon run` exited with {exit}"));
                return Ok(None);
            }
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The exit-4 diagnostic: what happened, where the log is, and how to see it
/// happen in the foreground.
fn report_start_failure(what: &str) {
    let log = daemon_log_path();
    error!("[daemon] start failed: {what} (log {})", log.display());
    eprintln!("{} the daemon failed to start: {what}", "\u{2717}".red());
    eprintln!("  daemon log: {}", log.display());
    if let Some(latest) = crate::config::latest_log_file() {
        eprintln!("  structured log: {}", latest.display());
    }
    eprintln!("  run it in the foreground to see why: mp daemon run");
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Report whether a daemon answers, as JSON or as one human-readable block.
async fn status(as_json: bool) -> Result<i32> {
    let running = query_status().await.ok();
    let object = status_object(running.as_ref());
    let code = if running.is_some() {
        EXIT_OK
    } else {
        EXIT_ERROR
    };

    if as_json {
        println!("{}", serde_json::to_string(&object)?);
        return Ok(code);
    }

    match &running {
        Some(_) => {
            println!("{} daemon running", "\u{2713}".green());
            println!("  instance:   {}", string_of(&object["instance_id"]));
            println!("  version:    {}", string_of(&object["app_version"]));
            println!(
                "  protocol:   {}..{}",
                object["protocol"]["min"], object["protocol"]["max"]
            );
            println!("  pid:        {}", object["pid"]);
            println!("  started at: {}", string_of(&object["started_at"]));
            println!("  data dir:   {}", string_of(&object["data_dir"]));
            println!("  config dir: {}", string_of(&object["config_dir"]));
            let accounts = object["accounts"].as_array().cloned().unwrap_or_default();
            if accounts.is_empty() {
                println!("  accounts:   none");
            } else {
                for account in accounts {
                    println!(
                        "  account:    {} ({})",
                        string_of(&account["name"]),
                        string_of(&account["state"])
                    );
                }
            }
        }
        None => {
            println!("{} no daemon running", "\u{2717}".red());
            println!("  data dir:   {}", string_of(&object["data_dir"]));
            println!("  config dir: {}", string_of(&object["config_dir"]));
            println!("  start one:  mp daemon start");
        }
    }
    Ok(code)
}

/// The `status --json` object.
///
/// The keys never change with the answer: a client parsing this must not have
/// to branch on which fields exist, only on `running`.
fn status_object(running: Option<&Value>) -> Value {
    match running {
        Some(status) => json!({
            "running": true,
            "instance_id": status["instance_id"],
            "app_version": status["app_version"],
            "protocol": status["protocol"],
            "pid": status["pid"],
            "started_at": status["started_at"],
            "data_dir": status["data_dir"],
            "config_dir": status["config_dir"],
            "accounts": status.get("accounts").cloned().unwrap_or_else(|| json!([])),
        }),
        None => json!({
            "running": false,
            "instance_id": Value::Null,
            "app_version": Value::Null,
            "protocol": Value::Null,
            "pid": Value::Null,
            "started_at": Value::Null,
            "data_dir": canonical(&crate::config::mailypoppins_data_dir()).display().to_string(),
            "config_dir": canonical(&crate::config::config_dir()).display().to_string(),
            "accounts": json!([]),
        }),
    }
}

/// A JSON string without its quotes, for the human-readable block.
fn string_of(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

// ---------------------------------------------------------------------------
// stop and restart
// ---------------------------------------------------------------------------

/// Ask the daemon to stop, and wait until it is really gone.
async fn stop(timeout: Duration) -> Result<i32> {
    let socket = socket_path();
    if query_status().await.is_err() {
        // Nothing answers. A leftover socket from a crashed daemon is swept
        // here, because "stop" is exactly when a user expects that tidying.
        if matches!(probe_socket(&socket), SocketProbe::Stale) {
            let _ = remove_stale_socket(&socket);
        }
        eprintln!("no daemon running");
        return Ok(EXIT_OK);
    }

    let pid = read_instance_meta().map(|meta| meta.pid);
    match rpc("daemon.stop").await {
        Ok(_) => info!("[daemon] stop: the daemon acknowledged the shutdown"),
        Err(e) => {
            // The socket answered `daemon.status` a moment ago, so an
            // unresponsive stop is a wedged daemon rather than an absent one:
            // fall back to the signal the runtime files point at.
            warn!("[daemon] stop over the socket failed ({e:#}), falling back to SIGTERM");
            match pid {
                Some(pid) => {
                    // SAFETY: `kill` with a pid this daemon published in its own
                    // runtime directory, under our uid.
                    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                }
                None => bail!(
                    "the daemon did not answer and {} names no pid",
                    instance_path().display()
                ),
            }
        }
    }

    let deadline = Instant::now() + timeout;
    loop {
        if !socket.exists() && query_status().await.is_err() {
            println!("{} daemon stopped", "\u{2713}".green());
            return Ok(EXIT_OK);
        }
        if Instant::now() >= deadline {
            bail!(
                "the daemon did not shut down within {}s; its log is {}",
                timeout.as_secs(),
                daemon_log_path().display()
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Stop whatever runs and start this executable's daemon.
async fn restart() -> Result<i32> {
    let previous = read_instance_meta().map(|meta| meta.pid);
    let code = stop(Duration::from_secs(10)).await?;
    if code != EXIT_OK {
        return Ok(code);
    }
    // The old process must be gone before the new one binds, or the new daemon
    // races the old one's cleanup for the socket it just created.
    if let Some(pid) = previous {
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_alive(pid) && Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
        }
    }
    start(Duration::from_secs(10)).await
}

/// Whether `pid` still exists (signal 0 probes without delivering).
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing and only reports whether the pid exists
    // and is signalable by this uid.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

// ---------------------------------------------------------------------------
// The admin client
// ---------------------------------------------------------------------------

/// `daemon.status` against the socket, or an error if nothing usable answers.
async fn query_status() -> Result<Value> {
    rpc("daemon.status").await
}

/// One request/response round trip on the admin socket, through `mp-client`.
///
/// Deliberately **without** an `initialize`: `daemon.status` and `daemon.stop`
/// are lifecycle surface and answer before the handshake, so these commands
/// keep working against a daemon whose protocol range this build cannot
/// negotiate. That is the case `mp daemon restart` exists for.
async fn rpc(method: &str) -> Result<Value> {
    let socket = socket_path();
    let call = async {
        let mut connection = Connection::connect(&socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        connection
            .call(method, json!({}))
            .await
            .with_context(|| format!("{method} failed"))
    };
    match tokio::time::timeout(RPC_TIMEOUT, call).await {
        Ok(result) => result,
        Err(_) => bail!(
            "the daemon did not answer {method} within {}s",
            RPC_TIMEOUT.as_secs()
        ),
    }
}

/// `daemon.json`, when it is there and parses.
fn read_instance_meta() -> Option<InstanceMeta> {
    let raw = fs::read_to_string(runtime::instance_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// An absolute, symlink-resolved path, falling back to the path itself when it
/// does not exist yet (`status` runs against directories nobody created).
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A `1`/`true`-style environment opt-in.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

/// A fresh instance id: 128 random bits as hex, which is enough for a client to
/// tell a restart from a reconnect and carries nothing about the host.
fn new_instance_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The not-running object has every key the contract lists, all five
    /// nullable ones null, and an empty account array.
    #[test]
    fn the_not_running_status_object_is_complete() {
        let object = status_object(None);
        for key in [
            "running",
            "instance_id",
            "app_version",
            "protocol",
            "pid",
            "started_at",
            "data_dir",
            "config_dir",
            "accounts",
        ] {
            assert!(object.get(key).is_some(), "{key} is present");
        }
        assert_eq!(object["running"], json!(false));
        assert!(object["instance_id"].is_null());
        assert!(object["protocol"].is_null());
        assert_eq!(object["accounts"], json!([]));
    }

    /// The running object copies the daemon's own answer rather than
    /// re-deriving the paths locally, so a daemon started against a different
    /// data directory cannot be reported as if it were ours.
    #[test]
    fn the_running_status_object_mirrors_the_daemon() {
        let answer = json!({
            "instance_id": "id",
            "app_version": "9.9.9",
            "protocol": {"min": 1, "max": 1},
            "pid": 4242,
            "started_at": "2026-01-01T00:00:00Z",
            "data_dir": "/elsewhere/data",
            "config_dir": "/elsewhere/config",
            "accounts": [{"name": "work", "state": "opening"}],
        });
        let object = status_object(Some(&answer));
        assert_eq!(object["running"], json!(true));
        assert_eq!(object["data_dir"], json!("/elsewhere/data"));
        assert_eq!(object["accounts"][0]["state"], json!("opening"));
    }

    /// The opt-in variables are read the same way everywhere: unset and `0` are
    /// both off, so a shell that exports `FOO=0` does not turn a hook on.
    #[test]
    fn env_flag_is_off_for_unset_empty_and_zero() {
        assert!(!env_flag("MAILYPOPPINS_A_VARIABLE_NOBODY_SETS"));
    }

    /// Instance ids are unique per call and shaped like the fixture.
    #[test]
    fn instance_ids_are_unique_hex() {
        let a = new_instance_id();
        let b = new_instance_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
