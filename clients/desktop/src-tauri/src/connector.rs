//! The GUI's `mp_client::session::Connector`: reach the daemon, starting one on
//! demand, and say why when that fails instead of exiting.
//!
//! This mirrors `src/daemon/client.rs::open_session` in the root crate, which a
//! client may not link:
//!
//! - One connect plus `initialize`, announcing [`ClientKind::Gui`] and the
//!   identity pair, under [`HANDSHAKE_TIMEOUT`].
//! - Nothing listening: start a daemon on demand, honouring
//!   `MAILYPOPPINS_DAEMON_AUTOSTART` (off switch) and
//!   `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS` (the budget), then retry the
//!   handshake on a widening gap until the budget is spent.
//! - The start is `mp daemon start`, run as a child process, rather than a
//!   second implementation of it: that command takes the `flock` start lock
//!   (two racing starters produce one daemon), sweeps a stale socket under the
//!   lock, spawns `mp daemon run` in its own session with its stdio in the
//!   daemon log, and exits once the daemon answers. A client cannot hold the
//!   lock the daemon's own `run` checks, so delegating is the only way to keep
//!   those guarantees.
//! - Failure is a typed [`ConnectError`], never an exit and never a fallback to
//!   the store.
//!
//! The `mp` binary is found by [`resolve_mp_binary`]: `$MP_DESKTOP_MP_BIN`,
//! then the sidecar next to this executable (`Contents/MacOS/mp` in a bundle),
//! then `mp` on `PATH`, then the two install locations a Finder-launched app
//! does not have on its `PATH` (`~/.cargo/bin/mp`, `/opt/homebrew/bin/mp`,
//! `/usr/local/bin/mp`).
//!
//! The version handshake: a daemon that completes `initialize` must also run the
//! same mailypoppins version as the `mp` the app would start one with, read off
//! that binary's `mp --version` (cached per binary and modification time). A
//! daemon left running by another install, or by a `cargo install` that was not
//! followed by `mp daemon restart`, is a [`ConnectFailure::VersionMismatch`],
//! which is the blocking restart screen; its Restart runs `mp daemon restart`
//! with that same binary, which replaces the daemon by a matching one. The CLI
//! and the TUI check only the protocol range, since each is the `mp` that would
//! start the daemon; the app ships its own and so checks the version too.
//!
//! `Connector` is two function pointers, so what they need (the paths, the last
//! handshake, the last reconnect failure) lives in statics here.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use mp_client::session::Connector;
use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity, InitializeResult};
use mp_protocol::ErrorCode;

use crate::error::{GuiError, ProtocolRange};
use crate::paths::Paths;

/// Budget for one connect plus `initialize`, the binary's `CONNECT_TIMEOUT`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default on-demand start budget. The binary's is 5 s; a GUI cold start
/// can afford more, and `mp daemon start`'s own readiness wait is 10 s.
const AUTOSTART_TIMEOUT: Duration = Duration::from_secs(12);

/// The root crate's names for the auto-start switches, honoured here too.
pub const AUTOSTART_ENV: &str = "MAILYPOPPINS_DAEMON_AUTOSTART";
pub const AUTOSTART_TIMEOUT_ENV: &str = "MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS";

/// Override for the `mp` binary the GUI starts the daemon with.
pub const MP_BIN_ENV: &str = "MP_DESKTOP_MP_BIN";

/// First and last gap between handshake attempts after a start.
const RETRY_MIN: Duration = Duration::from_millis(25);
const RETRY_MAX: Duration = Duration::from_millis(400);

/// A reconnect starts a daemon at most this often, so a daemon that keeps
/// dying (or that the user stopped on purpose) is not respawned every 2 s.
const REOPEN_AUTOSTART_EVERY: Duration = Duration::from_secs(30);

/// How long `mp daemon restart` may take.
const RESTART_TIMEOUT: Duration = Duration::from_secs(60);

/// Every method the GUI calls, required at the handshake so an older daemon
/// is a `capability_missing` there (the restart screen) rather than a
/// `-32601` in the middle of a read. Under test the fixture door panics on a
/// method missing here, so a call a test reaches cannot ship unlisted.
pub const REQUIRED_CAPABILITIES: &[&str] = &[
    "state.bootstrap",
    "account.list",
    "mailbox.list",
    "message.list",
    "message.get",
    "message.html",
    "message.search",
    "message.search_server",
    "message.materialise_html",
    "message.materialise_attachment",
    "message.release_handle",
    "message.fetch",
    "draft.list",
    "draft.create",
    "draft.create_from_message",
    "draft.reply",
    "draft.forward",
    "draft.path",
    "draft.validate",
    "draft.preview",
    "draft.approve",
    "draft.demote",
    "operation.status",
    "operation.cancel",
    "message.archive",
    "message.delete",
    "message.move",
    "message.set_flag",
    "message.set_read",
    "draft.discard",
    "send.cancel_hold",
    "send.hold_status",
    "send.draft",
    "send.approved",
    "send.outbox_list",
    "send.outbox_retry",
    "send.outbox_discard",
    "sync.quick",
    "sync.full",
    "calendar.events",
    "message.ics",
    "message.invite",
    "calendar.rsvp",
    "send.invite",
    "contact.search",
    "contact.rebuild",
    "config.get",
    "diagnostic.log_path",
    "config.reload",
    "config.set_password",
    "config.init",
    "config.add_account",
    "config.oauth2_login",
    "signature.list",
    "draft.attachments",
    "draft.attach",
    "draft.detach",
];

/// Why a connect failed, for the connection screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum ConnectFailure {
    /// No daemon, and none could be started.
    Unavailable,
    /// The daemon's protocol range or capabilities do not fit this GUI.
    VersionMismatch,
    /// A daemon for other data or config directories holds the socket.
    IdentityMismatch,
}

/// A connect that did not produce a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ConnectError {
    pub kind: ConnectFailure,
    pub why: String,
    pub socket: PathBuf,
    pub log: PathBuf,
    pub daemon_version: Option<String>,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (socket {}, daemon log {})",
            self.why,
            self.socket.display(),
            self.log.display()
        )
    }
}

impl From<ConnectError> for GuiError {
    fn from(e: ConnectError) -> GuiError {
        match e.kind {
            ConnectFailure::VersionMismatch => GuiError::VersionMismatch {
                message: e.why,
                daemon_version: e.daemon_version,
                client_protocol: ProtocolRange::ours(),
            },
            ConnectFailure::Unavailable | ConnectFailure::IdentityMismatch => {
                GuiError::DaemonUnavailable {
                    message: e.why,
                    socket: Some(e.socket.display().to_string()),
                    log: Some(e.log.display().to_string()),
                }
            }
        }
    }
}

/// What the last successful handshake reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct Hello {
    pub daemon_version: String,
    pub protocol: u32,
    pub instance_id: String,
}

impl From<&InitializeResult> for Hello {
    fn from(hello: &InitializeResult) -> Hello {
        Hello {
            daemon_version: hello.app_version.clone(),
            protocol: hello.protocol,
            instance_id: hello.instance_id.clone(),
        }
    }
}

static PATHS: OnceLock<Paths> = OnceLock::new();
static LAST_HELLO: Mutex<Option<Hello>> = Mutex::new(None);
static LAST_REOPEN_FAILURE: Mutex<Option<ConnectError>> = Mutex::new(None);
static LAST_AUTOSTART: Mutex<Option<Instant>> = Mutex::new(None);

/// Called when the kind of the latest reconnect failure changes, so the
/// session can push its status: a reconnect refused for a version mismatch
/// turns the banner into the restart screen.
type ReopenListener = Box<dyn Fn() + Send>;
static REOPEN_LISTENER: Mutex<Option<ReopenListener>> = Mutex::new(None);

fn paths() -> &'static Paths {
    PATHS.get_or_init(Paths::resolve)
}

/// The handshake the GUI's session last completed.
pub fn last_hello() -> Option<Hello> {
    LAST_HELLO.lock().ok().and_then(|g| g.clone())
}

fn record_hello(hello: &InitializeResult) {
    if let Ok(mut slot) = LAST_HELLO.lock() {
        *slot = Some(Hello::from(hello));
    }
}

/// Why the session's latest reconnect attempt failed, if it did.
pub fn last_reopen_failure() -> Option<ConnectError> {
    LAST_REOPEN_FAILURE.lock().ok().and_then(|g| g.clone())
}

pub(crate) fn record_reopen_failure(failure: Option<ConnectError>) {
    let changed = match LAST_REOPEN_FAILURE.lock() {
        Ok(mut slot) => {
            let changed = slot.as_ref().map(|f| (f.kind, f.daemon_version.as_deref()))
                != failure
                    .as_ref()
                    .map(|f| (f.kind, f.daemon_version.as_deref()));
            *slot = failure;
            changed
        }
        Err(_) => false,
    };
    // Outside the slot's lock: the listener reads it back through `status`.
    if changed {
        if let Ok(listener) = REOPEN_LISTENER.lock() {
            if let Some(notify) = listener.as_ref() {
                notify();
            }
        }
    }
}

/// Register what runs when the kind or the daemon version of the reconnect failure changes,
/// replacing an earlier listener.
pub fn on_reopen_failure_change(listener: ReopenListener) {
    if let Ok(mut slot) = REOPEN_LISTENER.lock() {
        *slot = Some(listener);
    }
}

fn error(kind: ConnectFailure, why: impl Into<String>) -> ConnectError {
    let paths = paths();
    ConnectError {
        kind,
        why: why.into(),
        socket: paths.socket.clone(),
        log: paths.daemon_log.clone(),
        daemon_version: None,
    }
}

// ---------------------------------------------------------------------------
// The handshake
// ---------------------------------------------------------------------------

/// One connect plus `initialize` as the GUI, under [`HANDSHAKE_TIMEOUT`].
async fn handshake() -> Result<(Connection, InitializeResult), ClientError> {
    let paths = paths();
    let attempt = async {
        let mut connection = Connection::connect(&paths.socket).await?;
        let hello = connection
            .initialize(
                ClientInfo {
                    kind: ClientKind::Gui,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: paths.data_dir.clone(),
                    config_dir: paths.config_dir.clone(),
                },
                REQUIRED_CAPABILITIES,
                &[],
            )
            .await?;
        Ok::<_, ClientError>((connection, hello))
    };
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, attempt).await {
        Ok(result) => result,
        Err(_) => Err(ClientError::Timeout {
            method: "initialize".to_string(),
            after: HANDSHAKE_TIMEOUT,
        }),
    }
}

/// A handshake failure as the screen shows it; `None` for "nothing is
/// listening", which is the one failure answered by starting a daemon.
fn classify(e: &ClientError) -> Option<ConnectError> {
    match e {
        ClientError::NotRunning => None,
        ClientError::Rpc(rpc) => {
            let kind = match ErrorCode::from_code(rpc.code) {
                Some(ErrorCode::ProtocolIncompatible) | Some(ErrorCode::CapabilityMissing) => {
                    ConnectFailure::VersionMismatch
                }
                Some(ErrorCode::IdentityMismatch) => ConnectFailure::IdentityMismatch,
                _ => ConnectFailure::Unavailable,
            };
            let mut why = format!(
                "the daemon refused the handshake: {} ({})",
                rpc.message, rpc.code
            );
            if let Some(data) = &rpc.data {
                why.push_str(&format!(" {data}"));
            }
            let mut failure = error(kind, why);
            failure.daemon_version = rpc
                .data
                .as_ref()
                .and_then(|d| {
                    d.pointer("/daemon/version")
                        .or_else(|| d.get("daemon_version"))
                })
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(failure)
        }
        ClientError::Timeout { .. } => Some(error(
            ConnectFailure::Unavailable,
            format!(
                "a daemon is listening but did not complete the handshake within {}s",
                HANDSHAKE_TIMEOUT.as_secs()
            ),
        )),
        other => Some(error(ConnectFailure::Unavailable, other.to_string())),
    }
}

/// Whether on-demand starting is on (the binary's rule: anything but an
/// explicit off).
pub fn autostart_enabled() -> bool {
    autostart_enabled_from(std::env::var(AUTOSTART_ENV).ok().as_deref())
}

fn autostart_enabled_from(value: Option<&str>) -> bool {
    match value {
        Some(v) => !matches!(v.trim(), "" | "0" | "false" | "no"),
        None => true,
    }
}

fn autostart_budget() -> Duration {
    std::env::var(AUTOSTART_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(AUTOSTART_TIMEOUT)
}

// ---------------------------------------------------------------------------
// The version handshake
// ---------------------------------------------------------------------------

/// The `mp` the app starts and restarts the daemon with, and its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedMp {
    pub path: PathBuf,
    pub version: String,
}

/// How long `mp --version` may take.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The last probe, keyed by the binary's path, size and modification time,
/// so a `cargo install` or an app update is read again and nothing else is.
type ProbeKey = (PathBuf, u64, Option<std::time::SystemTime>);
static LAST_PROBE: Mutex<Option<(ProbeKey, String)>> = Mutex::new(None);

/// The version in `mp --version`'s first line, `mailypoppins 0.10.0`.
pub fn parse_version(output: &str) -> Option<String> {
    let token = output.lines().next()?.split_whitespace().last()?;
    token
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| token.to_string())
}

/// Run `<path> --version` under [`VERSION_PROBE_TIMEOUT`].
async fn probe_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    parse_version(&out)
}

/// The `mp` [`resolve_mp_binary`] finds and the version it reports; `None`
/// when there is none or it does not say, which skips the check rather than
/// refusing every daemon.
pub async fn expected_mp() -> Option<ExpectedMp> {
    let path = resolve_mp_binary(&BinarySearch::from_process()).ok()?;
    let meta = path.metadata().ok()?;
    let key: ProbeKey = (path.clone(), meta.len(), meta.modified().ok());
    if let Ok(last) = LAST_PROBE.lock() {
        if let Some((seen, version)) = last.as_ref() {
            if *seen == key {
                return Some(ExpectedMp {
                    path,
                    version: version.clone(),
                });
            }
        }
    }
    let version = probe_version(&path).await?;
    if let Ok(mut last) = LAST_PROBE.lock() {
        *last = Some((key, version.clone()));
    }
    Some(ExpectedMp { path, version })
}

/// A daemon whose version is not the expected one, as the restart screen
/// shows it.
pub fn version_mismatch(daemon_version: &str, expected: &ExpectedMp) -> Option<ConnectError> {
    if daemon_version == expected.version {
        return None;
    }
    let mut failure = error(
        ConnectFailure::VersionMismatch,
        format!(
            "the running daemon is mailypoppins {daemon_version}, but this app starts {} \
             (mailypoppins {}); restart the daemon to run the matching version",
            expected.path.display(),
            expected.version
        ),
    );
    failure.daemon_version = Some(daemon_version.to_string());
    Some(failure)
}

/// Hand back a handshake whose daemon runs the expected version, or refuse it.
async fn check_version(
    open: (Connection, InitializeResult),
) -> Result<(Connection, InitializeResult), ConnectError> {
    let Some(expected) = expected_mp().await else {
        tracing::warn!(
            "[connect] no `mp --version` to compare the daemon's {} with; skipping the version check",
            open.1.app_version
        );
        return Ok(open);
    };
    match version_mismatch(&open.1.app_version, &expected) {
        None => Ok(open),
        Some(failure) => {
            tracing::warn!("[connect] {}", failure.why);
            Err(failure)
        }
    }
}

/// Connect, starting a daemon on demand when `autostart` allows it, and
/// refuse a daemon whose version is not the expected `mp`'s.
pub async fn connect_or_start(
    autostart: bool,
) -> Result<(Connection, InitializeResult), ConnectError> {
    match handshake().await {
        Ok(open) => return check_version(open).await,
        Err(e) => {
            if let Some(failure) = classify(&e) {
                return Err(failure);
            }
        }
    }
    if !autostart_enabled() {
        return Err(error(
            ConnectFailure::Unavailable,
            format!("none is listening and {AUTOSTART_ENV} turned on-demand starting off"),
        ));
    }
    if !autostart {
        return Err(error(ConnectFailure::Unavailable, "no daemon is listening"));
    }

    let budget = autostart_budget();
    let deadline = Instant::now() + budget;
    start_daemon(budget).await?;

    let mut gap = RETRY_MIN;
    loop {
        match handshake().await {
            // Started with the expected binary, unless another starter won.
            Ok(open) => return check_version(open).await,
            Err(e) => {
                if let Some(failure) = classify(&e) {
                    if failure.kind != ConnectFailure::Unavailable {
                        return Err(failure);
                    }
                }
                if Instant::now() >= deadline {
                    return Err(error(
                        ConnectFailure::Unavailable,
                        format!(
                            "a daemon was started but did not answer within {} ms: {e}",
                            budget.as_millis()
                        ),
                    ));
                }
                tokio::time::sleep(gap).await;
                gap = (gap * 2).min(RETRY_MAX);
            }
        }
    }
}

/// How long the session's first connect retries. Under `mp_client`'s
/// `CONNECT_CEILING` (30 s), so `Session::connect` always hears back from
/// [`open_session`] rather than giving up on a thread that goes on retrying.
pub const OPEN_DEADLINE: Duration = Duration::from_secs(25);

/// Set when the latest [`open_session`] ran out of time and handed back a
/// closed connection; read once by [`take_open_gave_up`].
static OPEN_GAVE_UP: AtomicBool = AtomicBool::new(false);

/// Whether the session's first connect gave up, clearing the mark. The
/// session thread then serves a dead connection, and the caller drops the
/// `Session`, which ends that thread.
pub fn take_open_gave_up() -> bool {
    OPEN_GAVE_UP.swap(false, Ordering::SeqCst)
}

/// The session's first connect. Only reached after [`connect_or_start`]
/// succeeded on the same machine a moment earlier, so it retries plain
/// handshakes, for [`OPEN_DEADLINE`].
///
/// `Connector::open` cannot fail, so past the deadline it answers a
/// connection whose peer is already gone and marks [`take_open_gave_up`].
/// The session thread then sees the connection close and ends once the
/// `Session` is dropped, instead of retrying every 2 s for ever behind a
/// `Session::connect` that stopped waiting.
async fn open_session() -> Connection {
    open_within(OPEN_DEADLINE, handshake).await
}

async fn open_within<F, Fut>(budget: Duration, mut attempt: F) -> Connection
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(Connection, InitializeResult), ClientError>>,
{
    OPEN_GAVE_UP.store(false, Ordering::SeqCst);
    let deadline = Instant::now() + budget;
    let mut gap = RETRY_MIN;
    loop {
        // Each attempt under what is left, so a handshake that hangs for its
        // own 10 s cannot carry the whole open past the ceiling.
        let left = deadline.saturating_duration_since(Instant::now());
        let tried = match tokio::time::timeout(left, attempt()).await {
            Ok(result) => result,
            Err(_) => Err(ClientError::Timeout {
                method: "initialize".to_string(),
                after: left,
            }),
        };
        match tried {
            Ok((connection, hello)) => {
                record_hello(&hello);
                return connection;
            }
            Err(e) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    tracing::warn!(
                        "[connect] the session handshake failed for {} s, giving up: {e}",
                        budget.as_secs()
                    );
                    OPEN_GAVE_UP.store(true, Ordering::SeqCst);
                    return match closed_connection().await {
                        Some(connection) => connection,
                        None => {
                            // Nothing to hand back: park rather than retry.
                            tracing::error!(
                                "[connect] no closed connection to end the session thread with"
                            );
                            std::future::pending().await
                        }
                    };
                }
                tracing::warn!("[connect] session handshake failed, retrying: {e}");
                tokio::time::sleep(gap.min(left)).await;
                gap = (gap * 2).min(Duration::from_secs(2));
            }
        }
    }
}

/// A connection whose peer has already hung up: a listener bound on a
/// throwaway socket for the length of one connect.
async fn closed_connection() -> Option<Connection> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "mp-desktop-closed-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).ok()?;
    let _ = listener.set_nonblocking(true);
    let connection = Connection::connect(&path).await.ok();
    // Accepting and dropping closes the peer; dropping a listener with the
    // connect still in its backlog resets it. Either way the read ends.
    drop(listener.accept());
    drop(listener);
    let _ = std::fs::remove_file(&path);
    connection
}

/// A reconnect: the same sequence, starting a daemon at most once every
/// [`REOPEN_AUTOSTART_EVERY`].
async fn reopen() -> Option<(Connection, String)> {
    let autostart = match LAST_AUTOSTART.lock() {
        Ok(mut last) => {
            let due = last.is_none_or(|at| at.elapsed() >= REOPEN_AUTOSTART_EVERY);
            if due {
                *last = Some(Instant::now());
            }
            due
        }
        Err(_) => false,
    };
    match connect_or_start(autostart).await {
        Ok((connection, hello)) => {
            record_hello(&hello);
            record_reopen_failure(None);
            Some((connection, hello.instance_id))
        }
        Err(e) => {
            tracing::info!("[connect] no daemon to reconnect to: {e}");
            record_reopen_failure(Some(e));
            None
        }
    }
}

/// The connector the session thread uses.
pub fn connector() -> Connector {
    Connector {
        open: || Box::pin(open_session()),
        reopen: || Box::pin(reopen()),
    }
}

// ---------------------------------------------------------------------------
// Starting and restarting the daemon
// ---------------------------------------------------------------------------

/// Where `resolve_mp_binary` looks, in order, for the report.
#[derive(Clone, Debug, Default)]
pub struct BinarySearch {
    pub env_override: Option<OsString>,
    pub exe_dir: Option<PathBuf>,
    pub path_var: Option<OsString>,
    pub home: Option<PathBuf>,
}

impl BinarySearch {
    pub fn from_process() -> BinarySearch {
        BinarySearch {
            env_override: std::env::var_os(MP_BIN_ENV).filter(|v| !v.is_empty()),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            path_var: std::env::var_os("PATH"),
            home: std::env::var_os("HOME").map(PathBuf::from),
        }
    }

    /// Every candidate, in search order.
    pub fn candidates(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(explicit) = &self.env_override {
            // An explicit override is the only candidate: silently using
            // another binary would hide a typo in it.
            out.push(PathBuf::from(explicit));
            return out;
        }
        if let Some(dir) = &self.exe_dir {
            out.push(dir.join("mp"));
        }
        if let Some(path) = &self.path_var {
            out.extend(std::env::split_paths(path).map(|dir| dir.join("mp")));
        }
        if let Some(home) = &self.home {
            out.push(home.join(".cargo/bin/mp"));
        }
        out.push(PathBuf::from("/opt/homebrew/bin/mp"));
        out.push(PathBuf::from("/usr/local/bin/mp"));
        out
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The first executable candidate, or the list that was tried.
pub fn resolve_mp_binary(search: &BinarySearch) -> Result<PathBuf, Vec<PathBuf>> {
    let candidates = search.candidates();
    candidates
        .iter()
        .find(|p| is_executable(p))
        .cloned()
        .ok_or(candidates)
}

fn mp_binary() -> Result<PathBuf, ConnectError> {
    resolve_mp_binary(&BinarySearch::from_process()).map_err(|tried| {
        let shown: Vec<String> = tried.iter().take(6).map(|p| p.display().to_string()).collect();
        error(
            ConnectFailure::Unavailable,
            format!(
                "no `mp` executable was found to start the daemon with (set {MP_BIN_ENV}); tried {}",
                shown.join(", ")
            ),
        )
    })
}

/// Run `mp <args>` with its stdio appended to the daemon log.
fn spawn_mp(args: &[&str]) -> Result<Child, ConnectError> {
    let exe = mp_binary()?;
    let paths = paths();
    let _ = mp_core::config::create_private_dir_all(&paths.logs_dir);
    let mut command = Command::new(&exe);
    command.args(args).stdin(Stdio::null());
    match File::options()
        .create(true)
        .append(true)
        .open(&paths.daemon_log)
    {
        Ok(out) => {
            let err = out
                .try_clone()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null());
            command.stdout(Stdio::from(out)).stderr(err);
        }
        Err(_) => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    tracing::info!("[connect] running {} {}", exe.display(), args.join(" "));
    command.spawn().map_err(|e| {
        error(
            ConnectFailure::Unavailable,
            format!("could not run {} {}: {e}", exe.display(), args.join(" ")),
        )
    })
}

/// What waiting for a child came to.
enum Waited {
    Exited(std::process::ExitStatus),
    TimedOut,
}

/// `mp daemon start`, waited for without blocking the runtime.
async fn start_daemon(budget: Duration) -> Result<(), ConnectError> {
    let secs = budget.as_secs().max(1).to_string();
    let mut child = spawn_mp(&["daemon", "start", "--timeout-secs", &secs])?;
    let deadline = Instant::now() + budget + Duration::from_secs(1);
    let waited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Waited::Exited(status),
            Ok(None) if Instant::now() >= deadline => break Waited::TimedOut,
            Ok(None) => tokio::time::sleep(Duration::from_millis(25)).await,
            Err(e) => {
                return Err(error(
                    ConnectFailure::Unavailable,
                    format!("polling `mp daemon start` failed: {e}"),
                ))
            }
        }
    };
    finish_child(&mut child, waited, "mp daemon start")
}

fn finish_child(child: &mut Child, waited: Waited, what: &str) -> Result<(), ConnectError> {
    match waited {
        Waited::Exited(status) if status.success() => Ok(()),
        Waited::Exited(status) => Err(error(
            ConnectFailure::Unavailable,
            format!("`{what}` failed ({status}); its output is in the daemon log"),
        )),
        Waited::TimedOut => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error(
                ConnectFailure::Unavailable,
                format!("`{what}` did not finish in time"),
            ))
        }
    }
}

/// `mp daemon restart`: stop the running daemon and start the `mp` binary's
/// own. Blocking; the caller runs it off the UI thread. The explicit user
/// confirmation is the frontend's.
pub fn restart_daemon_blocking() -> Result<(), ConnectError> {
    let mut child = spawn_mp(&["daemon", "restart"])?;
    let deadline = Instant::now() + RESTART_TIMEOUT;
    let waited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Waited::Exited(status),
            Ok(None) if Instant::now() >= deadline => break Waited::TimedOut,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                return Err(error(
                    ConnectFailure::Unavailable,
                    format!("polling `mp daemon restart` failed: {e}"),
                ))
            }
        }
    };
    // A reconnect after a deliberate restart may start a daemon at once.
    if let Ok(mut last) = LAST_AUTOSTART.lock() {
        *last = None;
    }
    finish_child(&mut child, waited, "mp daemon restart")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch_dir;
    use std::os::unix::fs::PermissionsExt;

    fn make_exe(path: &Path) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("dir");
        }
        std::fs::write(path, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn the_env_override_is_the_only_candidate() {
        let search = BinarySearch {
            env_override: Some("/nowhere/mp".into()),
            exe_dir: Some("/app/Contents/MacOS".into()),
            path_var: Some("/usr/bin".into()),
            home: Some("/home/me".into()),
        };
        assert_eq!(search.candidates(), vec![PathBuf::from("/nowhere/mp")]);
        assert_eq!(
            resolve_mp_binary(&search),
            Err(vec![PathBuf::from("/nowhere/mp")])
        );
    }

    #[test]
    fn the_sidecar_wins_over_path() {
        let root = scratch_dir("sidecar");
        let bundle = root.join("Contents/MacOS");
        let on_path = root.join("bin");
        make_exe(&bundle.join("mp"));
        make_exe(&on_path.join("mp"));
        let search = BinarySearch {
            env_override: None,
            exe_dir: Some(bundle.clone()),
            path_var: Some(on_path.clone().into_os_string()),
            home: None,
        };
        assert_eq!(resolve_mp_binary(&search), Ok(bundle.join("mp")));
    }

    #[test]
    fn path_is_searched_in_order_and_non_executables_are_skipped() {
        let root = scratch_dir("path");
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).expect("dir");
        std::fs::write(first.join("mp"), "not executable").expect("write");
        make_exe(&second.join("mp"));
        let path_var = std::env::join_paths([&first, &second]).expect("join");
        let search = BinarySearch {
            env_override: None,
            exe_dir: Some(root.join("empty")),
            path_var: Some(path_var),
            home: None,
        };
        assert_eq!(resolve_mp_binary(&search), Ok(second.join("mp")));
    }

    #[test]
    fn cargo_bin_is_the_fallback_after_path() {
        let root = scratch_dir("cargo");
        make_exe(&root.join(".cargo/bin/mp"));
        let search = BinarySearch {
            env_override: None,
            exe_dir: None,
            path_var: Some(root.join("empty").into_os_string()),
            home: Some(root.clone()),
        };
        let found = resolve_mp_binary(&search);
        // A Homebrew or /usr/local `mp` on this machine sorts after it.
        assert_eq!(found, Ok(root.join(".cargo/bin/mp")));
    }

    #[test]
    fn the_version_is_the_last_word_of_the_first_line() {
        assert_eq!(
            parse_version("mailypoppins 0.10.0\n"),
            Some("0.10.0".into())
        );
        assert_eq!(parse_version("mp 1.2.3-rc.1"), Some("1.2.3-rc.1".into()));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("usage: mp [OPTIONS]"), None);
    }

    #[test]
    fn a_daemon_of_another_version_is_a_version_mismatch_naming_both() {
        let expected = ExpectedMp {
            path: "/Applications/mailypoppins.app/Contents/MacOS/mp".into(),
            version: "0.11.0".into(),
        };
        assert!(version_mismatch("0.11.0", &expected).is_none());
        let failure = version_mismatch("0.10.0", &expected).expect("a mismatch");
        assert_eq!(failure.kind, ConnectFailure::VersionMismatch);
        assert_eq!(failure.daemon_version.as_deref(), Some("0.10.0"));
        assert!(failure.why.contains("0.10.0"), "{}", failure.why);
        assert!(failure.why.contains("0.11.0"), "{}", failure.why);
        assert!(failure.why.contains("Contents/MacOS/mp"), "{}", failure.why);
        assert!(matches!(
            GuiError::from(failure),
            GuiError::VersionMismatch { daemon_version: Some(v), .. } if v == "0.10.0"
        ));
    }

    /// The expected version is the resolved binary's own `--version`, read
    /// again once the binary is replaced.
    #[test]
    fn the_expected_version_is_probed_from_the_binary_and_reread_on_replacement() {
        let _lock = crate::test_support::env_lock();
        let bin = scratch_dir("probe").join("mp");
        let script = |version: &str| {
            std::fs::write(&bin, format!("#!/bin/sh\necho mailypoppins {version}\n"))
                .expect("write");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let saved = std::env::var_os(MP_BIN_ENV);
        std::env::set_var(MP_BIN_ENV, &bin);
        script("0.10.0");
        let first = runtime.block_on(expected_mp());
        // A longer version string changes the size, so the cache misses even
        // within the modification time's resolution.
        script("0.10.10");
        let second = runtime.block_on(expected_mp());
        std::fs::write(&bin, "#!/bin/sh\nexit 3\n").expect("write");
        let failing = runtime.block_on(expected_mp());
        match saved {
            Some(v) => std::env::set_var(MP_BIN_ENV, v),
            None => std::env::remove_var(MP_BIN_ENV),
        }
        assert_eq!(
            first,
            Some(ExpectedMp {
                path: bin.clone(),
                version: "0.10.0".into()
            })
        );
        assert_eq!(second.map(|e| e.version), Some("0.10.10".into()));
        assert_eq!(failing, None, "a failing --version skips the check");
    }

    #[test]
    fn the_autostart_switch_mirrors_the_binary() {
        assert!(autostart_enabled_from(None));
        assert!(autostart_enabled_from(Some("1")));
        for off in ["", "0", "false", "no", " 0 "] {
            assert!(!autostart_enabled_from(Some(off)), "{off:?}");
        }
    }

    #[test]
    fn a_capability_refusal_is_a_version_mismatch() {
        let e = ClientError::Rpc(mp_protocol::RpcError {
            code: ErrorCode::CapabilityMissing.code(),
            message: "capability_missing".into(),
            data: None,
        });
        let failure = classify(&e).expect("a failure");
        assert_eq!(failure.kind, ConnectFailure::VersionMismatch);
        assert!(matches!(
            GuiError::from(failure),
            GuiError::VersionMismatch { .. }
        ));
        assert!(classify(&ClientError::NotRunning).is_none());
    }

    /// The compose, send and attachment methods are required too, so a
    /// daemon older than M3 is refused at the handshake, and M4's agenda
    /// reads; each name once.
    #[test]
    fn the_required_capabilities_cover_compose_send_and_attachments() {
        for method in [
            "draft.create",
            "draft.create_from_message",
            "draft.reply",
            "draft.forward",
            "draft.path",
            "draft.validate",
            "draft.preview",
            "draft.approve",
            "draft.demote",
            "send.draft",
            "send.approved",
            "send.outbox_list",
            "send.outbox_retry",
            "send.outbox_discard",
            "message.fetch",
            "message.materialise_attachment",
            "calendar.events",
            "message.ics",
            "message.invite",
            "calendar.rsvp",
            "send.invite",
            "contact.search",
            "contact.rebuild",
            "config.get",
            "diagnostic.log_path",
            "config.reload",
            "config.set_password",
            "config.init",
            "config.add_account",
            "config.oauth2_login",
        ] {
            assert!(REQUIRED_CAPABILITIES.contains(&method), "{method}");
        }
        let mut names = REQUIRED_CAPABILITIES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), REQUIRED_CAPABILITIES.len());
    }

    #[test]
    fn the_session_open_gives_up_at_its_deadline_with_a_closed_connection() {
        let socket = scratch_dir("open").join("nobody.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let budget = Duration::from_millis(200);
        let mut attempts = 0;
        let started = Instant::now();
        let mut connection = runtime.block_on(open_within(budget, || {
            attempts += 1;
            let socket = socket.clone();
            async move {
                Connection::connect(&socket)
                    .await
                    .and_then(|_| Err(ClientError::NotRunning))
            }
        }));
        let took = started.elapsed();
        assert!(took >= budget, "gave up after {took:?}");
        assert!(took < Duration::from_secs(5), "gave up after {took:?}");
        assert!(attempts > 1, "{attempts} attempts");
        assert!(take_open_gave_up());
        assert!(!take_open_gave_up(), "the mark is read once");
        // The session thread's `serve` sees it close at once and ends.
        assert!(runtime.block_on(connection.next_notification()).is_none());
    }

    #[test]
    fn a_session_on_the_closed_connection_ends_its_thread_when_dropped() {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let session = mp_client::session::Session::connect(Connector {
                open: || Box::pin(async { closed_connection().await.expect("closed") }),
                reopen: || Box::pin(async { None }),
            })
            .expect("the open answers at once");
            // `Drop` joins the session thread.
            drop(session);
            let _ = done.send(());
        });
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("the session thread ended");
    }
}
