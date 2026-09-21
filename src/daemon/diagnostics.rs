//! Health, the daemon's own log, and the support bundle (P6-U8, ticket #0125).
//!
//! Three questions a support case asks, answered from one place: what is this
//! daemon doing, what did it write down, and what may a user send a stranger.
//!
//! # The checks are the whole of `health`'s verdict
//!
//! Four fixed checks in a fixed order - `config_loaded`, `store_open`,
//! `socket_owner`, `log_writable` - then one `account:<name>` per configured
//! account in configuration order. A [`Status`] is `ok`, `warn` or `fail`, and
//! a detail is never empty, including on a passing check: the sentence is what
//! a support case is read with and it is also what the TUI renders into its
//! activity ring.
//!
//! `warn` and `fail` are two words because a second `mp` holding an account's
//! engine lock is a normal state of a developer's machine, while an account the
//! daemon cannot serve at all is a failure whatever caused it. A daemon that
//! called the first one a failure would cry wolf and the second would stop
//! being read.
//!
//! # Reads do not publish
//!
//! [`Diagnostics::refresh`] is the only thing that announces a flip;
//! [`Diagnostics::health`] and [`Diagnostics::not_ok`] evaluate and say
//! nothing. Three `diagnostic.health` calls in a row are one client asking
//! three times, not three things happening, and a bootstrap that announced the
//! very checks it is handing over in the same frame would be telling a client
//! twice.
//!
//! # The log the daemon writes
//!
//! `<data_dir>/logs/mailypoppins-<date>.log`, the simplelog `WriteLogger`
//! [`crate::config::init_logging`] installs. It is **not**
//! `<data_dir>/logs/daemon.log`, which is only where `mp daemon start` points a
//! detached daemon's stdio and which is empty for a daemon started in the
//! foreground. A line is
//! `2026-09-21 19:05:20.081 [INFO] [(thread) target: ]message`; [`parse_line`]
//! says which parts appear at which level and what happens to a line the format
//! does not explain.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, TimeZone};
use log::Level;
use serde_json::{json, Value};

use mp_protocol::events::KIND_DIAGNOSTIC_CHECK_CHANGED;

use super::config::{validate_document, ConfigStore};
use super::hold::HoldScheduler;
use super::operations::OperationRegistry;
use super::runtime::{socket_path, InstanceMeta};
use super::server::{RuntimeHealth, RuntimeTable};
use super::state::events::Event;
use super::state::CanonicalState;

/// The four checks every health report carries, whatever the configuration
/// holds, in report order.
pub const FIXED_CHECKS: [&str; 4] = [
    "config_loaded",
    "store_open",
    "socket_owner",
    "log_writable",
];

/// The five files a bundle holds, in the order `files` reports them.
pub const BUNDLE_FILES: [&str; 5] = [
    "config.toml",
    "daemon-status.json",
    "health.json",
    "log.txt",
    "version.txt",
];

/// What replaces a secret value in a redacted bundle.
pub const REDACTED: &str = "<redacted>";

/// How many lines `diagnostic.logs` answers when the caller names none.
pub const DEFAULT_LOG_LINES: usize = 200;

/// The most it will answer at all; above this is `-32602` naming the cap.
pub const MAX_LOG_LINES: usize = 5000;

/// How much of the daemon's log a support bundle carries.
const BUNDLE_LOG_LINES: usize = 2000;

/// How often the daemon re-evaluates its checks without being asked.
///
/// A safety net rather than the trigger: a configuration reload and an account
/// runtime that reported both refresh at once. What this catches is the socket
/// or the log file going away under a daemon nobody is talking to.
pub const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

// ---------------------------------------------------------------------------
// One check
// ---------------------------------------------------------------------------

/// A check's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing to do.
    Ok,
    /// Something a user would want to know about and can live with.
    Warn,
    /// Something the daemon cannot do its job through.
    Fail,
}

impl Status {
    /// The word this status travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

/// One check, as the report, the snapshot and the event all carry it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// `config_loaded`, `store_open`, `socket_owner`, `log_writable` or
    /// `account:<name>`.
    pub name: String,
    /// The verdict.
    pub status: Status,
    /// One sentence, never empty.
    pub detail: String,
}

impl Check {
    fn new(name: impl Into<String>, status: Status, detail: impl Into<String>) -> Check {
        Check {
            name: name.into(),
            status,
            detail: detail.into(),
        }
    }

    /// The three keys, which are the same three everywhere this travels.
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "status": self.status.as_str(), "detail": self.detail})
    }
}

/// Take a lock whose holder may have panicked; the map behind it is whole
/// either way.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// The assembly
// ---------------------------------------------------------------------------

/// Everything a health report is assembled from, and the ledger that remembers
/// what it last said.
///
/// Held by [`DaemonState`](super::server::DaemonState) and by the canonical
/// state, which projects [`Diagnostics::not_ok`] into every bootstrap snapshot.
/// The canonical state is held **weakly** back, for the reason the operation
/// registry's fan-out closure is: the two must not keep each other alive.
pub struct Diagnostics {
    meta: InstanceMeta,
    started: Instant,
    config: Arc<ConfigStore>,
    runtimes: Arc<RuntimeTable>,
    operations: Arc<OperationRegistry>,
    holds: Arc<HoldScheduler>,
    canonical: Weak<CanonicalState>,
    ledger: Mutex<BTreeMap<String, Status>>,
}

impl std::fmt::Debug for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Diagnostics")
            .field("instance", &self.meta.instance_id)
            .finish_non_exhaustive()
    }
}

impl Diagnostics {
    /// Assemble the reporter. `started` is now, because the caller builds this
    /// at the same moment it writes `daemon.json`.
    pub fn new(
        meta: InstanceMeta,
        config: Arc<ConfigStore>,
        runtimes: Arc<RuntimeTable>,
        operations: Arc<OperationRegistry>,
        holds: Arc<HoldScheduler>,
        canonical: &Arc<CanonicalState>,
    ) -> Diagnostics {
        Diagnostics {
            meta,
            started: Instant::now(),
            config,
            runtimes,
            operations,
            holds,
            canonical: Arc::downgrade(canonical),
            ledger: Mutex::new(BTreeMap::new()),
        }
    }

    /// The `result` of `daemon.status`.
    ///
    /// Lives here rather than on the state because it is assembled from the
    /// same three things a health report is, and `daemon-status.json` in a
    /// support bundle is this object verbatim. The CLI adds `"running": true`
    /// and prints the rest.
    pub fn status_result(&self) -> Value {
        json!({
            "instance_id": self.meta.instance_id,
            "app_version": self.meta.app_version,
            "protocol": {"min": self.meta.protocol_min, "max": self.meta.protocol_max},
            "pid": self.meta.pid,
            "started_at": self.meta.started_at,
            "data_dir": self.meta.data_dir.display().to_string(),
            "config_dir": self.meta.config_dir.display().to_string(),
            // The live configuration says which accounts there are; the runtime
            // table says how each one is doing. An account with no entry yet is
            // `opening`, which is what an account whose start is still in flight
            // is.
            "accounts": self
                .config
                .status_accounts()
                .iter()
                .map(|name| {
                    let state = self.runtimes.state_of(name).unwrap_or("opening");
                    json!({"name": name, "state": state})
                })
                .collect::<Vec<_>>(),
        })
    }

    /// The `result` of `diagnostic.health`, at the protocol version the asking
    /// connection negotiated.
    ///
    /// `version` and `protocol_version` rather than `app_version` and
    /// `protocol`: `daemon.status` is the pre-handshake lifecycle probe and
    /// answers the daemon's *range*, while health is a support document and
    /// what a support case needs is the single integer this connection settled
    /// on. Two different facts, two different names.
    pub fn health(&self, protocol: u32) -> Value {
        let accounts = self.config.accounts();
        let clients = self
            .canonical
            .upgrade()
            .map_or(0, |canonical| canonical.subscriber_count());
        json!({
            "instance_id": self.meta.instance_id,
            "version": self.meta.app_version,
            "protocol_version": protocol,
            "uptime_secs": self.started.elapsed().as_secs(),
            "pid": self.meta.pid,
            "socket": socket_path().display().to_string(),
            "log_path": log_file().display().to_string(),
            // The asking connection counts itself: a report that said `0` while
            // answering over a socket would be wrong on its face.
            "clients": clients,
            "accounts": accounts
                .iter()
                .map(|account| self.account_entry(&account.name))
                .collect::<Vec<_>>(),
            "holds": self.holds.listing(None).holds.len(),
            // An object with one key rather than a bare number, so a count of
            // failures or of durable operations joins it without a version bump.
            "operations": {"active": self.operations.live().len()},
            "store": self.store_entry(&accounts),
            "checks": self.checks().iter().map(Check::to_json).collect::<Vec<_>>(),
        })
    }

    /// The checks that are not `ok`, in report order, which is what
    /// `snapshot.diagnostics` carries.
    pub fn not_ok(&self) -> Vec<Value> {
        self.checks()
            .iter()
            .filter(|check| check.status != Status::Ok)
            .map(Check::to_json)
            .collect()
    }

    /// Evaluate every check and publish the ones whose status moved.
    ///
    /// Called when the daemon has a reason to believe something changed: an
    /// account runtime reported, a configuration reload happened or was
    /// refused, or the periodic sweep came round.
    pub fn refresh(&self) {
        let checks = self.checks();
        let flipped: Vec<Check> = {
            let mut ledger = lock(&self.ledger);
            let flipped = checks
                .iter()
                .filter(|check| {
                    ledger
                        .insert(check.name.clone(), check.status)
                        .unwrap_or(Status::Ok)
                        != check.status
                })
                .cloned()
                .collect();
            // A check that no longer exists is forgotten rather than remembered
            // as whatever it last was: an account removed by a reload must not
            // publish a recovery when a later one adds it back.
            ledger.retain(|name, _| checks.iter().any(|check| &check.name == name));
            flipped
        };
        let Some(canonical) = self.canonical.upgrade() else {
            return;
        };
        for check in flipped {
            log::info!(
                "[daemon] {} is {}: {}",
                check.name,
                check.status.as_str(),
                check.detail
            );
            canonical.publish(Event::Lifecycle {
                kind: KIND_DIAGNOSTIC_CHECK_CHANGED,
                payload: check.to_json(),
            });
        }
    }

    /// Every check, in report order. Pure: nothing is published and the ledger
    /// is not touched.
    pub fn checks(&self) -> Vec<Check> {
        let accounts = self.config.accounts();
        let mut checks = vec![
            self.config_loaded(),
            store_open(&accounts),
            socket_owner(),
            log_writable(),
        ];
        for account in accounts.iter() {
            checks.push(self.account_check(&account.name));
        }
        checks
    }

    /// Whether the file on disk would load *now*.
    ///
    /// Deliberately the file rather than the live snapshot: `config.reload` on a
    /// broken file is refused and the daemon keeps serving the configuration it
    /// has, which is right, and which is exactly the state this check exists to
    /// report.
    fn config_loaded(&self) -> Check {
        let path = self.config.path();
        let shown = path.display();
        if !path.exists() {
            return Check::new(
                "config_loaded",
                Status::Warn,
                format!("no config.toml at {shown}; the daemon is serving no accounts"),
            );
        }
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                return Check::new(
                    "config_loaded",
                    Status::Fail,
                    format!("cannot read {shown}: {e}"),
                )
            }
        };
        match validate_document(&text, path) {
            Ok(config) => Check::new(
                "config_loaded",
                Status::Ok,
                format!("{shown} loaded, {} account(s)", config.accounts.len()),
            ),
            Err(diagnostic) => Check::new(
                "config_loaded",
                Status::Fail,
                match diagnostic.line {
                    Some(line) => format!("{}, at line {line} of {shown}", diagnostic.message),
                    None => format!("{}, in {shown}", diagnostic.message),
                },
            ),
        }
    }

    /// One account's runtime, as the check that names it.
    fn account_check(&self, account: &str) -> Check {
        let name = format!("account:{account}");
        match self.runtimes.health_of(account) {
            RuntimeHealth::Ready => Check::new(name, Status::Ok, format!("{account} is serving")),
            RuntimeHealth::Opening => Check::new(
                name,
                Status::Warn,
                format!("{account} is still opening its store"),
            ),
            // A lock held elsewhere is a normal state of a developer's machine:
            // the daemon still serves this account's reads.
            RuntimeHealth::Blocked(reason) => Check::new(
                name,
                Status::Warn,
                format!("{account} is blocked: {reason}"),
            ),
            RuntimeHealth::Failed(reason) => Check::new(
                name,
                Status::Fail,
                format!("{account} has no runtime at all: {reason}"),
            ),
        }
    }

    /// One account's entry in a health report.
    fn account_entry(&self, account: &str) -> Value {
        let health = self.runtimes.health_of(account);
        let (finished_at, outcome) = self
            .canonical
            .upgrade()
            .and_then(|canonical| canonical.last_sync(account))
            .map_or((Value::Null, Value::Null), |(at, outcome)| {
                (json!(at.to_rfc3339()), json!(outcome))
            });
        json!({
            "name": account,
            "runtime": health.as_str(),
            // Derived from the readiness rather than from a task handle: the
            // daemon watches exactly the accounts whose runtime came up ready,
            // and a blocked runtime watches nothing because the engine holding
            // the lock is watching the same mailbox.
            "watcher": if matches!(health, RuntimeHealth::Ready) { "running" } else { "stopped" },
            "last_sync": {"finished_at": finished_at, "outcome": outcome},
        })
    }

    /// The store tree, as one entry rather than one per account: `path` is the
    /// tree that holds every account's store and blobs, and `size_bytes` is the
    /// summed size of the `store.sqlite3` files under it. It answers the only
    /// question a health report is asked about the store, which is whether the
    /// cache is large.
    fn store_entry(&self, accounts: &[crate::config::AccountConfig]) -> Value {
        let size: u64 = accounts
            .iter()
            .filter_map(|account| fs::metadata(crate::config::store_path(&account.name)).ok())
            .map(|meta| meta.len())
            .sum();
        json!({
            "path": accounts_dir().display().to_string(),
            "size_bytes": size,
        })
    }
}

/// `<data_dir>/accounts`, the tree every account's store lives under.
fn accounts_dir() -> PathBuf {
    crate::config::mailypoppins_data_dir().join("accounts")
}

/// The file the daemon is writing, which is the file `Action::OpenLogFile`
/// opens (`INT-02`, `OBS-05`).
///
/// [`crate::config::latest_log_file`] when there is one, and today's name when
/// the directory is empty, so the answer is always a path a client can act on.
pub fn log_file() -> PathBuf {
    crate::config::latest_log_file().unwrap_or_else(|| {
        crate::config::logs_dir().join(format!(
            "mailypoppins-{}.log",
            chrono::Utc::now().format("%Y-%m-%d")
        ))
    })
}

/// How many of the configured accounts have a store the daemon could open.
///
/// Every one is `ok`, some is `warn`, none is `fail`: the check is about the
/// set, and the per-account checks say which one it was.
fn store_open(accounts: &[crate::config::AccountConfig]) -> Check {
    let total = accounts.len();
    if total == 0 {
        return Check::new(
            "store_open",
            Status::Ok,
            "no accounts are configured, so there is no store to open",
        );
    }
    let open = accounts
        .iter()
        .filter(|account| crate::config::store_path(&account.name).exists())
        .count();
    let detail = format!(
        "{open} of {total} account store(s) are open under {}",
        accounts_dir().display()
    );
    let status = match open {
        _ if open == total => Status::Ok,
        0 => Status::Fail,
        _ => Status::Warn,
    };
    Check::new("store_open", status, detail)
}

/// Whether the socket at the layout's path is this process's, at mode 0600.
fn socket_owner() -> Check {
    let socket = socket_path();
    let shown = socket.display();
    let meta = match fs::symlink_metadata(&socket) {
        Ok(meta) => meta,
        Err(e) => {
            return Check::new(
                "socket_owner",
                Status::Fail,
                format!("{shown} cannot be read: {e}"),
            )
        }
    };
    if !meta.file_type().is_socket() {
        return Check::new(
            "socket_owner",
            Status::Fail,
            format!("{shown} is not a socket"),
        );
    }
    // SAFETY: `geteuid` takes no pointer and cannot fail.
    let us = unsafe { libc::geteuid() };
    if meta.uid() != us {
        return Check::new(
            "socket_owner",
            Status::Fail,
            format!("{shown} is owned by uid {}, not by uid {us}", meta.uid()),
        );
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Check::new(
            "socket_owner",
            Status::Warn,
            format!("{shown} is mode {mode:04o}, not 0600"),
        );
    }
    Check::new(
        "socket_owner",
        Status::Ok,
        format!("{shown} is this daemon's, mode 0600"),
    )
}

/// Whether the daemon can still append to the log it names.
fn log_writable() -> Check {
    let path = log_file();
    let shown = path.display();
    match fs::OpenOptions::new().append(true).create(true).open(&path) {
        Ok(_) => Check::new("log_writable", Status::Ok, format!("writing {shown}")),
        Err(e) => Check::new(
            "log_writable",
            Status::Fail,
            format!("cannot append to {shown}: {e}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

/// One validated `diagnostic.logs` call.
#[derive(Clone, Debug)]
pub struct LogQuery {
    /// How many matching lines to answer, at most [`MAX_LOG_LINES`].
    pub lines: usize,
    /// The *minimum* severity a line must carry, `None` for all of them.
    pub level: Option<Level>,
    /// The instant a line must be at or after, `None` for all of them.
    pub since: Option<DateTime<FixedOffset>>,
}

impl Default for LogQuery {
    fn default() -> LogQuery {
        LogQuery {
            lines: DEFAULT_LOG_LINES,
            level: None,
            since: None,
        }
    }
}

/// The `level` vocabulary, lowercase, which is what a parsed line reports and
/// what the parameter takes.
pub fn level_from_wire(value: &str) -> Option<Level> {
    match value {
        "trace" => Some(Level::Trace),
        "debug" => Some(Level::Debug),
        "info" => Some(Level::Info),
        "warn" => Some(Level::Warn),
        "error" => Some(Level::Error),
        _ => None,
    }
}

/// The five words `level` takes, for the refusal that names them.
pub const LOG_LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// One parsed line of the daemon's log.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogLine {
    stamp: Option<DateTime<FixedOffset>>,
    level: Option<Level>,
    target: String,
    message: String,
}

impl LogLine {
    fn to_json(&self) -> Value {
        json!({
            // RFC3339 with the daemon's local offset: the file's own stamps are
            // local time with no offset, and a bundle read on another machine
            // would otherwise lie about when something happened.
            "ts": self.stamp.map(|stamp| stamp.to_rfc3339()),
            "level": self.level.map(|level| level.as_str().to_ascii_lowercase()),
            "target": self.target,
            "message": self.message,
        })
    }

    /// Whether this line survives the query's two filters.
    fn matches(&self, query: &LogQuery) -> bool {
        // A minimum level admits the lines that carry no level at all: those
        // are what a crash looks like, and dropping them precisely when someone
        // filters for errors would be the wrong way round.
        if let (Some(wanted), Some(level)) = (query.level, self.level) {
            if level > wanted {
                return false;
            }
        }
        match (query.since, self.stamp) {
            // A line with no timestamp cannot be shown to be after an instant.
            (Some(_), None) => false,
            (Some(since), Some(stamp)) => stamp >= since,
            (None, _) => true,
        }
    }
}

/// The `result` of `diagnostic.logs`: the **tail** of the matching lines, in
/// file order, and whether older ones were dropped.
pub fn read_logs(path: &Path, query: &LogQuery) -> Result<Value> {
    let (lines, truncated) = tail(path, query)?;
    Ok(json!({
        "path": path.display().to_string(),
        "lines": lines,
        "truncated": truncated,
    }))
}

/// The last `query.lines` matching lines of `path`, and whether any matching
/// line was dropped to fit.
///
/// Streamed through a ring rather than read whole: a daily log of a daemon that
/// has been up for a week is not a thing to hold in memory to answer two
/// hundred lines out of.
fn tail(path: &Path, query: &LogQuery) -> Result<(Vec<Value>, bool)> {
    let file = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut kept: VecDeque<Value> = VecDeque::new();
    let mut truncated = false;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("reading {}", path.display()))?
            == 0
        {
            break;
        }
        // Lossy rather than fatal: a log truncated mid-character by a crash is
        // exactly the file somebody is asking to read.
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches(['\n', '\r']);
        if line.is_empty() {
            continue;
        }
        let parsed = parse_line(line);
        if !parsed.matches(query) {
            continue;
        }
        kept.push_back(parsed.to_json());
        if kept.len() > query.lines {
            kept.pop_front();
            truncated = true;
        }
    }
    Ok((kept.into(), truncated))
}

/// One line of the daemon's log, as the writer's format explains it.
///
/// The thread and the target are read only at `DEBUG` and below, which is where
/// the writer emits them: a message like `warning: the disk is full` carries a
/// token ending in a colon too, and a parser that took it for a target would
/// eat half the sentence.
fn parse_line(line: &str) -> LogLine {
    let unparsed = || LogLine {
        stamp: None,
        level: None,
        target: String::new(),
        message: line.to_string(),
    };
    let Some((stamp, rest)) = split_stamp(line) else {
        return unparsed();
    };
    let Some((level, rest)) = split_level(rest) else {
        return unparsed();
    };
    let rest = match level {
        Level::Debug | Level::Trace => strip_thread(rest),
        _ => rest,
    };
    let (target, message) = match level {
        Level::Debug | Level::Trace => split_target(rest),
        _ => ("", rest),
    };
    LogLine {
        stamp: Some(stamp),
        level: Some(level),
        target: target.to_string(),
        message: message.to_string(),
    }
}

/// `2026-09-21 19:05:20.081 `, as an instant at the daemon's current local
/// offset.
fn split_stamp(line: &str) -> Option<(DateTime<FixedOffset>, &str)> {
    let (head, rest) = line.split_at_checked(23)?;
    let naive = NaiveDateTime::parse_from_str(head, "%Y-%m-%d %H:%M:%S%.3f").ok()?;
    let offset = *Local::now().offset();
    let stamp = offset.from_local_datetime(&naive).single()?;
    Some((stamp, rest.strip_prefix(' ')?))
}

/// `[INFO] `.
fn split_level(rest: &str) -> Option<(Level, &str)> {
    let rest = rest.strip_prefix('[')?;
    let (name, rest) = rest.split_once("] ")?;
    let level = match name {
        "ERROR" => Level::Error,
        "WARN" => Level::Warn,
        "INFO" => Level::Info,
        "DEBUG" => Level::Debug,
        "TRACE" => Level::Trace,
        _ => return None,
    };
    Some((level, rest))
}

/// `(3) `, the thread id simplelog writes at `DEBUG` and below.
fn strip_thread(rest: &str) -> &str {
    match rest
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(") "))
    {
        Some((id, tail)) if !id.contains(' ') => tail,
        _ => rest,
    }
}

/// `mailypoppins::daemon::server: `, the module path, `""` when there is none.
fn split_target(rest: &str) -> (&str, &str) {
    match rest.split_once(": ") {
        Some((target, message)) if !target.is_empty() && !target.contains(' ') => (target, message),
        _ => ("", rest),
    }
}

// ---------------------------------------------------------------------------
// The support bundle
// ---------------------------------------------------------------------------

/// One validated `diagnostic.support_bundle` call.
#[derive(Clone, Debug)]
pub struct BundlePlan {
    /// Where the directory goes; absolute, checked by the caller.
    pub out: PathBuf,
    /// Whether secret values are struck from every file.
    pub redact: bool,
    /// The protocol version `health.json` reports, which is the asking
    /// connection's.
    pub protocol: u32,
}

/// A default bundle name under the data directory, distinct per call so two
/// bundles in one second are two directories.
pub fn default_bundle_path() -> PathBuf {
    crate::config::mailypoppins_data_dir().join(format!(
        "support-bundle-{}",
        Local::now().format("%Y%m%d-%H%M%S-%3f")
    ))
}

/// Write the five files and, unless told not to, strike every secret from all
/// of them.
///
/// Neither `secrets.enc` nor the token cache is reachable from here: the five
/// files are written from what the daemon holds, so there is no copy step a
/// credential could ride in on.
pub fn write_bundle(diagnostics: &Diagnostics, plan: &BundlePlan) -> Result<Value> {
    fs::create_dir_all(&plan.out)
        .with_context(|| format!("creating the bundle directory {}", plan.out.display()))?;

    let config_path = diagnostics.config.path().to_path_buf();
    let config_text = fs::read_to_string(&config_path)
        .unwrap_or_else(|e| format!("# {} could not be read: {e}\n", config_path.display()));
    write_file(&plan.out, "config.toml", &config_text)?;
    write_file(
        &plan.out,
        "daemon-status.json",
        &pretty(&diagnostics.status_result()),
    )?;
    write_file(
        &plan.out,
        "health.json",
        &pretty(&diagnostics.health(plan.protocol)),
    )?;
    write_file(&plan.out, "log.txt", &log_tail()?)?;
    write_file(
        &plan.out,
        "version.txt",
        &format!(
            "mailypoppins {}\nprotocol {}..{}\nplatform {}\n",
            env!("CARGO_PKG_VERSION"),
            mp_protocol::PROTOCOL_MIN,
            mp_protocol::PROTOCOL_MAX,
            std::env::consts::OS,
        ),
    )?;

    let redactions = if plan.redact {
        redact_tree(&plan.out, &secret_values(&config_text))?
    } else {
        0
    };
    Ok(json!({
        "path": plan.out.display().to_string(),
        "files": BUNDLE_FILES,
        "redactions": redactions,
    }))
}

/// The tail of the daemon's own log, whole lines, as they are on disk.
fn log_tail() -> Result<String> {
    let path = log_file();
    let (lines, _) = tail(
        &path,
        &LogQuery {
            lines: BUNDLE_LOG_LINES,
            ..LogQuery::default()
        },
    )
    .unwrap_or_else(|_| (Vec::new(), false));
    let mut text = format!("# the tail of {}\n", path.display());
    for line in lines {
        let stamp = line["ts"].as_str().unwrap_or("").to_string();
        let level = line["level"]
            .as_str()
            .map(str::to_uppercase)
            .unwrap_or_default();
        let target = line["target"].as_str().unwrap_or("");
        let message = line["message"].as_str().unwrap_or("");
        text.push_str(&render_log_line(&stamp, &level, target, message));
        text.push('\n');
    }
    Ok(text)
}

/// One log line as a client prints it: the stamp, the level, the target when
/// there is one, and the message.
///
/// Shared by `log.txt` and by `mp daemon logs`, so a bundle and a terminal
/// never disagree about what a line said.
pub fn render_log_line(stamp: &str, level: &str, target: &str, message: &str) -> String {
    let mut line = String::new();
    if !stamp.is_empty() {
        line.push_str(stamp);
        line.push(' ');
    }
    if !level.is_empty() {
        line.push('[');
        line.push_str(level);
        line.push_str("] ");
    }
    if !target.is_empty() {
        line.push_str(target);
        line.push_str(": ");
    }
    line.push_str(message);
    line
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()) + "\n"
}

fn write_file(dir: &Path, name: &str, text: &str) -> Result<()> {
    let path = dir.join(name);
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).ok();
    Ok(())
}

/// Every value a secret key named, longest first.
///
/// Longest first so a secret that contains another is struck before the one it
/// contains, which would otherwise leave the tail of it in the file.
///
/// A line scan rather than a TOML parse: a `config.toml` that does not load is
/// exactly the one a support bundle is collected for, and a parser that refused
/// it would leave its credentials in the file.
fn secret_values(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !is_secret_key(key.trim()) {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if value.is_empty() || found.iter().any(|seen| seen == value) {
            continue;
        }
        found.push(value.to_string());
    }
    found.sort_by_key(|value| std::cmp::Reverse(value.len()));
    found
}

/// Whether a configuration key names a credential.
///
/// Email addresses and OAuth2 client ids stay verbatim: a bundle without them
/// is useless and neither is a credential.
fn is_secret_key(key: &str) -> bool {
    let key = key.trim().trim_matches('"');
    matches!(
        key,
        "password" | "client_secret" | "access_token" | "refresh_token"
    ) || key.ends_with("_secret")
        || key.ends_with("_token")
}

/// Strike every secret from every file of the bundle, and answer how many
/// replacements that took.
///
/// Every file, not only `config.toml`: the same string may have been logged by
/// a library that did not know it was a secret.
fn redact_tree(dir: &Path, secrets: &[String]) -> Result<u64> {
    let mut replaced = 0u64;
    for name in BUNDLE_FILES {
        let path = dir.join(name);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut text = text;
        for secret in secrets {
            let hits = text.matches(secret.as_str()).count() as u64;
            if hits == 0 {
                continue;
            }
            replaced += hits;
            text = text.replace(secret.as_str(), REDACTED);
        }
        fs::write(&path, text).with_context(|| format!("redacting {}", path.display()))?;
    }
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_info_line_is_a_stamp_a_level_and_a_message() {
        let parsed = parse_line("2026-09-21 19:25:59.921 [INFO] Watching mailbox 'INBOX'");
        assert_eq!(parsed.level, Some(Level::Info));
        assert_eq!(parsed.target, "");
        assert_eq!(parsed.message, "Watching mailbox 'INBOX'");
        assert!(parsed.stamp.is_some());
        assert_eq!(parsed.to_json()["level"], json!("info"));
    }

    /// At `DEBUG` the writer adds the thread id and the module path, and both
    /// come off the message.
    #[test]
    fn a_debug_line_carries_a_thread_and_a_target() {
        let parsed =
            parse_line("2026-09-21 19:25:59.921 [DEBUG] (3) mailypoppins::daemon: a frame");
        assert_eq!(parsed.level, Some(Level::Debug));
        assert_eq!(parsed.target, "mailypoppins::daemon");
        assert_eq!(parsed.message, "a frame");
    }

    /// A sentence with a colon in it at `INFO` keeps the whole sentence: the
    /// writer emits no target at that level, so nothing may be taken for one.
    #[test]
    fn an_info_message_with_a_colon_keeps_its_whole_sentence() {
        let parsed = parse_line("2026-09-21 19:25:59.921 [INFO] warning: the disk is full");
        assert_eq!(parsed.target, "");
        assert_eq!(parsed.message, "warning: the disk is full");
    }

    /// A panic's first line explains itself to nobody, and is kept whole.
    #[test]
    fn a_line_the_format_does_not_explain_is_kept_whole() {
        let stray = "thread 'main' panicked at src/nowhere.rs:1:1:";
        let parsed = parse_line(stray);
        assert_eq!(
            parsed.to_json(),
            json!({"ts": null, "level": null, "target": "", "message": stray})
        );
    }

    /// A minimum level admits everything at least that severe, and the lines
    /// that carry no level at all.
    #[test]
    fn a_minimum_level_admits_the_severe_and_the_unexplained() {
        let query = LogQuery {
            level: Some(Level::Warn),
            ..LogQuery::default()
        };
        let line = |text: &str| parse_line(text).matches(&query);
        assert!(line("2026-09-21 19:25:59.921 [ERROR] boom"));
        assert!(line("2026-09-21 19:25:59.921 [WARN] careful"));
        assert!(!line("2026-09-21 19:25:59.921 [INFO] chatter"));
        assert!(line("a backtrace frame"));
    }

    /// `since` drops what has no timestamp: it cannot be shown to be after an
    /// instant.
    #[test]
    fn since_drops_a_line_with_no_timestamp() {
        let query = LogQuery {
            since: Some(
                DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z").expect("a fixed instant"),
            ),
            ..LogQuery::default()
        };
        assert!(parse_line("2026-09-21 19:25:59.921 [INFO] later").matches(&query));
        assert!(!parse_line("a backtrace frame").matches(&query));
    }

    /// The keys decide what is a secret, and an address or a client id is not
    /// one.
    #[test]
    fn the_secret_keys_are_the_four_names_and_the_two_suffixes() {
        for key in [
            "password",
            "client_secret",
            "access_token",
            "refresh_token",
            "app_secret",
            "id_token",
        ] {
            assert!(is_secret_key(key), "{key} names a credential");
        }
        for key in ["username", "client_id", "default_from", "host"] {
            assert!(!is_secret_key(key), "{key} does not");
        }
    }

    /// Every value a secret key named, longest first, each once.
    #[test]
    fn the_values_are_collected_longest_first() {
        let text = concat!(
            "username = \"a@example.com\"\n",
            "password = \"short\"\n",
            "client_secret = \"a-much-longer-secret\"\n",
            "password = \"short\"\n",
        );
        assert_eq!(
            secret_values(text),
            vec!["a-much-longer-secret".to_string(), "short".to_string()]
        );
    }

    /// A status is one of three words and a check always says why.
    #[test]
    fn a_check_renders_its_three_keys() {
        let check = Check::new("store_open", Status::Warn, "one of two");
        assert_eq!(
            check.to_json(),
            json!({"name": "store_open", "status": "warn", "detail": "one of two"})
        );
    }
}
