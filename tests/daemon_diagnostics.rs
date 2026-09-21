//! Health, logs and the support bundle, over a socket (P6-U7, ticket #0125).
//!
//! The plan's sentence, verbatim: *"`diagnostic.health`, `diagnostic.logs`,
//! `diagnostic.support_bundle` (redacted). Feeds the TUI activity overlay and
//! the future GUI."* The parity matrix carries the same subject three times:
//! `LIF-07` (health and support diagnostics), and `INT-02` / `OBS-05`, which
//! both name `diagnostic.log_path` as the surface the log file is reached
//! through.
//!
//! Almost every row here is a real `mp daemon run` over a real socket: a
//! health report is a statement about a live process - its uptime, its
//! connections, its runtimes, the socket it owns and the log it is writing -
//! and none of that can be produced in process. The TUI half of the unit is
//! `src/tui/diagnostics_tests.rs`, which is where a `&mut App` is.
//!
//! # The contract this file pins
//!
//! One Rust name, `DIAGNOSTIC_METHOD_SPECS`, which exists and grows from one
//! entry to five. Everything else is on the wire and on stdout, so **this file
//! compiles against the tree as committed** and every contract row fails at
//! runtime naming what is missing. The implementer (P6-U8) does not edit it;
//! they make it pass.
//!
//! ```text
//! diagnostic.health {}    Query      -> {
//!     instance_id, version, protocol_version, uptime_secs, pid,
//!     socket, log_path, clients,
//!     accounts: [{name, runtime, last_sync: {finished_at, outcome}, watcher}],
//!     holds, operations: {active}, store: {path, size_bytes},
//!     checks: [{name, status, detail}]
//! }
//!     runtime  is `opening` | `ready` | `blocked`, `daemon.status`'s vocabulary.
//!     watcher  is `running` | `stopped`.
//!     last_sync is {null, null} until a tick lands, then {rfc3339, "ok"|"failed"}.
//!     checks are, in this order: config_loaded, store_open, socket_owner,
//!            log_writable, then `account:<name>` in configuration order.
//!     status   is `ok` | `warn` | `fail`; detail is never empty.
//!
//! diagnostic.log_path {}  Query      -> {path}
//!     The file the daemon is writing, `<data_dir>/logs/mailypoppins-<date>.log`,
//!     which is the file `Action::OpenLogFile` opens (INT-02, OBS-05).
//!
//! diagnostic.logs {lines?, level?, since?}  Query
//!                                -> {path, lines: [{ts, level, target, message}], truncated}
//!     lines defaults to 200 and above 5000 is -32602; level is one of
//!     trace|debug|info|warn|error and is a *minimum*; since is RFC3339.
//!     ts is RFC3339 with the daemon's local offset, level is lowercase,
//!     target is "" for a line that carries none, and a line that does not
//!     parse at all is kept as {null, null, "", <the whole line>}.
//!
//! diagnostic.support_bundle {out?, redact?}  Operation, Durable
//!                                -> {operation_id}
//!     settles {path, files: [...], redactions}
//!     A directory, not an archive: this tree links no tar and no gzip, and a
//!     new dependency is not this unit's to add. Five files and nothing else:
//!     config.toml, daemon-status.json, health.json, log.txt, version.txt.
//!     redact defaults to true and replaces every secret value with
//!     `<redacted>`, everywhere in the bundle.
//!
//! state.event {kind: "diagnostic.check_changed",
//!              payload: {name, status, detail}}   -> every bootstrapped client
//!     Published when a check's `status` changes, never for a detail that moved
//!     under an unchanged status.
//!
//! state.bootstrap's snapshot.diagnostics: the checks that are not `ok`, in
//!     `checks` order, each item the `checks` item verbatim.
//!
//! mp daemon health [--json]
//! mp daemon logs [--lines N] [--level L] [--json]
//! mp daemon support-bundle [OUT] [--no-redact]
//! ```
//!
//! # Why the bundle is a directory
//!
//! `Cargo.lock` holds neither `tar` nor `flate2`, and section 2.4 of the plan
//! says no to a new download that a unit can do without. A directory of five
//! files is what `tar czf` would have been handed anyway, and a user who wants
//! one archive runs the `tar` his machine already has. The contract names the
//! five entries and their order, so adding the archive later is a wrapper
//! rather than a reshuffle.
//!
//! # Why redaction is by value and not only by key
//!
//! A key-matching pass over `config.toml` catches `password`, `client_secret`
//! and every `*_token`, and it catches nothing in `log.txt`, where the same
//! string may have been logged by a library that did not know it was a secret.
//! So the rule is both: the keys decide *what is a secret*, and every value so
//! identified is then struck from every text file in the bundle. The row that
//! proves it seeds the sandbox configuration with five marker strings and
//! greps the finished bundle for each of them; `--no-redact` is the control
//! that keeps the row from passing vacuously over an empty bundle.
//!
//! # What this file deliberately does not pin
//!
//! - **The TUI's reading of the event.** `src/tui/diagnostics_tests.rs` owns
//!   the activity-overlay half: which `StatusLevel` a flipped check lands at
//!   and that no golden frame moves.
//! - **`diagnostic.store_gc`.** `tests/daemon_admin_slice.rs` owns the sweep;
//!   the only thing said about it here is that it stays in the family and
//!   keeps its place in the spec array.
//! - **The daemon's log *content*.** What the daemon logs is
//!   `docs/daemon-operations.md`'s business. This file pins how a line is
//!   parsed, not which lines exist, and every row picks its lines by level or
//!   by a substring the daemon has always written.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::methods::diagnostic::DIAGNOSTIC_METHOD_SPECS;
use mailypoppins::engine_lock::EngineLock;
use mailypoppins::store::Store;

use support::parity::{mp_no_daemon, socket_path, DaemonFixture};

// ---------------------------------------------------------------------------
// The contract's constants
// ---------------------------------------------------------------------------

/// The five methods of the family, in the order their spec array declares
/// them, which is method-name order like every other family.
const DIAGNOSTIC_METHODS: [&str; 5] = [
    "diagnostic.health",
    "diagnostic.log_path",
    "diagnostic.logs",
    "diagnostic.store_gc",
    "diagnostic.support_bundle",
];

/// The four checks every health report carries, in report order, whatever the
/// configuration holds.
const FIXED_CHECKS: [&str; 4] = [
    "config_loaded",
    "store_open",
    "socket_owner",
    "log_writable",
];

/// The keys of a health report, sorted.
const HEALTH_KEYS: [&str; 13] = [
    "accounts",
    "checks",
    "clients",
    "holds",
    "instance_id",
    "log_path",
    "operations",
    "pid",
    "protocol_version",
    "socket",
    "store",
    "uptime_secs",
    "version",
];

/// The keys of one account's entry in a health report, sorted.
const HEALTH_ACCOUNT_KEYS: [&str; 4] = ["last_sync", "name", "runtime", "watcher"];

/// The keys of one check, sorted. The same three the event carries and the
/// same three `snapshot.diagnostics` carries.
const CHECK_KEYS: [&str; 3] = ["detail", "name", "status"];

/// The keys of one parsed log line, sorted.
const LOG_LINE_KEYS: [&str; 4] = ["level", "message", "target", "ts"];

/// The five files a bundle holds, sorted, which is also the order `files`
/// reports them in.
const BUNDLE_FILES: [&str; 5] = [
    "config.toml",
    "daemon-status.json",
    "health.json",
    "log.txt",
    "version.txt",
];

/// How many lines `diagnostic.logs` answers when the caller names none.
const DEFAULT_LOG_LINES: usize = 200;

/// The most it will answer at all; above this is `-32602`.
const MAX_LOG_LINES: usize = 5000;

/// The kind a flipped check travels as.
const KIND_CHECK_CHANGED: &str = "diagnostic.check_changed";

/// The word that replaces a secret in a redacted bundle.
const REDACTED: &str = "<redacted>";

/// JSON-RPC's own "invalid params".
const INVALID_PARAMS: i32 = -32602;

/// The daemon's "your `config.toml` does not load".
const CONFIG_INVALID: i32 = -32007;

/// The two accounts every row is about.
const ALPHA: &str = "alpha";
const BETA: &str = "beta";

/// The five secrets seeded into the sandbox configuration.
///
/// Literals with a marker prefix, so a grep of the finished bundle is
/// unambiguous and a failure names the exact field that survived.
const MARKERS: [&str; 5] = [
    "P6U7-MARKER-smtp-password",
    "P6U7-MARKER-imap-password",
    "P6U7-MARKER-client-secret",
    "P6U7-MARKER-access-token",
    "P6U7-MARKER-refresh-token",
];

/// The sandbox configuration: two accounts with no host anywhere, so no
/// runtime reaches for a network, and five secrets in the five key shapes the
/// redaction rule names.
const CONFIG: &str = r#"[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.smtp]
username = "alpha@example.com"
password = "P6U7-MARKER-smtp-password"

[accounts.imap]
username = "alpha@example.com"
password = "P6U7-MARKER-imap-password"

[accounts.oauth2]
client_id = "00000000-0000-0000-0000-000000000000"
tenant_id = "common"
client_secret = "P6U7-MARKER-client-secret"
access_token = "P6U7-MARKER-access-token"
refresh_token = "P6U7-MARKER-refresh-token"

[[accounts]]
name = "beta"
default_from = "beta@example.com"
"#;

/// How long a row waits for an operation to settle or an event to arrive.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root and a daemon serving it.
///
/// The field order is the drop order: the daemon dies before the directory it
/// was reading is removed.
struct Diag {
    daemon: Option<DaemonFixture>,
    tmp: TempDir,
}

impl Diag {
    /// Seed the root, then start a daemon over it. In that order: the daemon
    /// loads `config.toml` once, at startup.
    fn start() -> Diag {
        Diag::start_with(&[])
    }

    /// The same, with hooks put back on top of the sandbox.
    fn start_with(env: &[(&str, &str)]) -> Diag {
        let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
        seed(tmp.path());
        let daemon = DaemonFixture::start_with(tmp.path(), None, env);
        wait_settled(tmp.path());
        Diag {
            daemon: Some(daemon),
            tmp,
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// Run the client against the same root.
    fn mp(&self, args: &[&str]) -> Output {
        self.daemon
            .as_ref()
            .expect("the daemon is live for as long as the fixture is")
            .mp(args)
    }

    /// End the daemon and wait until it is gone.
    fn stop(mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
}

/// Write the configuration and one store per account under `root`.
fn seed(root: &Path) {
    fs::create_dir_all(root).unwrap_or_else(|e| panic!("create {}: {e}", root.display()));
    fs::write(root.join("config.toml"), CONFIG).expect("write config.toml");
    for account in [ALPHA, BETA] {
        let dir = account_dir(root, account);
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        drop(Store::open(dir.join("store.sqlite3")).expect("open the seeded store"));
    }
}

/// `<root>/accounts/<account>`.
fn account_dir(root: &Path, account: &str) -> PathBuf {
    root.join("accounts").join(account)
}

/// The engine lock of `account` under `root`, which a test holds to make a
/// runtime come up blocked.
fn engine_lock_path(root: &Path, account: &str) -> PathBuf {
    account_dir(root, account).join("store.lock")
}

/// `<root>/logs/mailypoppins-<today>.log`, the file the daemon writes.
///
/// Derived from the directory rather than from a date this test computes: the
/// daemon opens whatever `crate::config::latest_log_file` would find, and a
/// row that guessed the date would break at midnight.
fn log_file(root: &Path) -> PathBuf {
    let mut candidates: Vec<PathBuf> = fs::read_dir(root.join("logs"))
        .unwrap_or_else(|e| panic!("read {}/logs: {e}", root.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("mailypoppins-") && name.ends_with(".log"))
        })
        .collect();
    candidates.sort();
    candidates
        .pop()
        .unwrap_or_else(|| panic!("the daemon writes a log file under {}/logs", root.display()))
}

/// Put one of the configuration's secrets into the daemon's own log file.
///
/// Every marker lives in `config.toml` and in no other file of a bundle, so a
/// redaction pass that ran over that one file and skipped the other four
/// would satisfy every assertion about the markers. This is what makes the
/// redaction row a statement about `log.txt` as well: the same string arrives
/// there the way a real one would, logged by something that did not know it
/// was a secret.
///
/// The line is written in the format the daemon's own writer produces, at a
/// level that carries no target, so the log reader parses it and renders it
/// back into `log.txt` with the marker intact.
fn log_a_secret(root: &Path, marker: &str) -> String {
    let path = log_file(root);
    let message = format!("a careless library logged {marker}");
    let mut text = fs::read_to_string(&path).expect("read the daemon log");
    text.push_str(&format!("2026-09-21 19:25:59.921 [WARN] {message}\n"));
    fs::write(&path, text).expect("append a line carrying a secret");
    message
}

/// Wait until every account's runtime has reported.
///
/// A runtime is started asynchronously, so a row that asserts `ready` or
/// `blocked` would otherwise race the start that decides it and read
/// `opening`. `daemon.status` is the settled-state oracle every other suite
/// polls, and it answers ahead of the handshake gate.
fn wait_settled(root: &Path) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let status = block_on(async {
            let mut conn = Connection::connect(&socket_path(root))
                .await
                .expect("connecting to a live daemon socket succeeds");
            conn.call("daemon.status", json!({}))
                .await
                .expect("daemon.status answers ahead of the handshake")
        });
        let opening = status["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts.iter().any(|a| a["state"] == json!("opening")));
        if !opening {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "an account was still opening after {DEADLINE:?}: {status}"
        );
        std::thread::sleep(TICK);
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

/// A handshaken connection, which is what every domain method needs.
async fn client(root: &Path) -> Connection {
    let mut conn = Connection::connect(&socket_path(root))
        .await
        .expect("connecting to a live daemon socket succeeds");
    conn.initialize(
        ClientInfo {
            kind: ClientKind::Tui,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        Identity {
            data_dir: root.to_path_buf(),
            config_dir: root.to_path_buf(),
        },
        &[],
        &[],
    )
    .await
    .expect("a compatible handshake succeeds");
    conn
}

/// The same, bootstrapped, so it receives events.
async fn subscribed(root: &Path) -> Connection {
    let mut conn = client(root).await;
    conn.call("state.bootstrap", json!({}))
        .await
        .expect("a bootstrap registers this connection for events");
    conn
}

/// One call that must be answered.
async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    match tokio::time::timeout(DEADLINE, conn.call(method, params)).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => panic!("{method} was refused: {e:?}"),
        Err(_) => panic!("{method} did not answer within {DEADLINE:?}"),
    }
}

/// One call that must be refused, and the typed error it was refused with.
async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = tokio::time::timeout(DEADLINE, conn.call(method, params))
        .await
        .unwrap_or_else(|_| panic!("{method} did not answer within {DEADLINE:?}"))
        .expect_err("this call must be refused");
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{method}: expected a typed RPC error, got {other:?}"),
    }
}

/// Start an operation and answer its terminal `operation.status` object.
async fn run_operation(conn: &mut Connection, method: &str, params: Value) -> Value {
    let started = call(conn, method, params).await;
    let id = started["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{method} answers with an operation_id: {started}"))
        .to_string();
    assert_eq!(
        started.as_object().map(serde_json::Map::len),
        Some(1),
        "{method} answers with the id and nothing else: {started}"
    );
    let start = Instant::now();
    loop {
        let status = call(conn, "operation.status", json!({"operation_id": &id})).await;
        let state = status["state"]
            .as_str()
            .unwrap_or_else(|| panic!("operation.status always carries a state: {status}"))
            .to_string();
        if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
            assert_eq!(state, "succeeded", "{method} settled as {state}: {status}");
            return status["result"].clone();
        }
        assert!(
            start.elapsed() < DEADLINE,
            "{method} was still {state} after {DEADLINE:?}"
        );
        tokio::time::sleep(TICK).await;
    }
}

/// Read notifications until one carries `kind`, and answer its payload.
async fn await_kind(conn: &mut Connection, kind: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "no {kind} event arrived within {DEADLINE:?}"
        );
        let notification = tokio::time::timeout(DEADLINE, conn.next_notification())
            .await
            .unwrap_or_else(|_| panic!("no {kind} arrived within {DEADLINE:?}"))
            .unwrap_or_else(|| panic!("the daemon closed the connection before any {kind}"));
        if notification.params["kind"] == kind {
            return notification.params["payload"].clone();
        }
    }
}

/// An object's keys, sorted.
fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("an object, and this is {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// The `checks` array of a health report, as values.
fn checks(health: &Value) -> Vec<Value> {
    health["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("a health report carries a checks array: {health}"))
        .clone()
}

/// One check by name, or a failure naming the ones that are there.
fn check(health: &Value, name: &str) -> Value {
    let all = checks(health);
    all.iter()
        .find(|check| check["name"] == json!(name))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "no check named {name}; the report carries {:?}",
                all.iter()
                    .map(|check| check["name"].clone())
                    .collect::<Vec<_>>()
            )
        })
}

/// The names of a health report's checks, in report order.
fn check_names(health: &Value) -> Vec<String> {
    checks(health)
        .iter()
        .map(|check| {
            check["name"]
                .as_str()
                .unwrap_or_else(|| panic!("a check carries a name: {check}"))
                .to_string()
        })
        .collect()
}

/// A `u64` field, or a failure naming it.
fn number(value: &Value, field: &str) -> u64 {
    value[field]
        .as_u64()
        .unwrap_or_else(|| panic!("{field} is a number: {value}"))
}

/// A string field, or a failure naming it.
fn text(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("{field} is a string: {value}"))
        .to_string()
}

fn stdout_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_lines(out: &Output) -> Vec<String> {
    stdout_text(out).lines().map(str::to_string).collect()
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// Every regular file under `dir`, as paths relative to it, sorted.
fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, found);
        } else {
            found.push(
                path.strip_prefix(root)
                    .expect("a path under the root it was walked from")
                    .display()
                    .to_string(),
            );
        }
    }
}

/// Every text file of a bundle, concatenated, which is what a grep for a
/// secret has to be run against.
fn bundle_text(dir: &Path) -> String {
    tree(dir)
        .iter()
        .map(|name| fs::read_to_string(dir.join(name)).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bundle directory one settled `diagnostic.support_bundle` wrote.
fn bundle_dir(result: &Value) -> PathBuf {
    let path = PathBuf::from(text(result, "path"));
    assert!(
        path.is_dir(),
        "a support bundle is a directory, and {} is not one",
        path.display()
    );
    path
}

// ===========================================================================
// 1. The family
// ===========================================================================

/// Five declarations, and the four facts each one fixes: the wire name, the
/// kind, the first protocol version, and what a disconnect does to it.
#[test]
fn the_diagnostic_family_declares_five_methods() {
    let names: Vec<&str> = DIAGNOSTIC_METHOD_SPECS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        names, DIAGNOSTIC_METHODS,
        "the family serves exactly these five"
    );

    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the array is in method-name order");

    for spec in DIAGNOSTIC_METHOD_SPECS {
        assert_eq!(spec.since, 1, "{} is served from protocol 1", spec.name);
    }
}

/// The three reads are queries, the bundle is a durable operation.
///
/// Durable and not `PerConnection`: a bundle is asked for by a client that is
/// about to send it somewhere and may well close its window while the daemon
/// is still copying, and a half-written bundle directory is worse than none.
#[test]
fn the_reads_are_queries_and_the_bundle_is_a_durable_operation() {
    let kind_of = |name: &str| {
        DIAGNOSTIC_METHOD_SPECS
            .iter()
            .find(|spec| spec.name == name)
            .unwrap_or_else(|| panic!("{name} is declared by the family"))
    };

    for name in [
        "diagnostic.health",
        "diagnostic.log_path",
        "diagnostic.logs",
    ] {
        assert_eq!(
            kind_of(name).kind,
            MethodKind::Query,
            "{name} reads and changes nothing"
        );
    }
    let bundle = kind_of("diagnostic.support_bundle");
    assert_eq!(bundle.kind, MethodKind::Operation);
    assert_eq!(
        bundle.cancel_scope,
        CancelScope::Durable,
        "a bundle half written to disk is worse than one nobody asked for"
    );
}

/// A connection is offered the four new names, so nothing is served
/// unadvertised.
#[test]
fn the_handshake_advertises_the_four_new_names() {
    let diag = Diag::start();
    let offered = block_on(async {
        let mut conn = Connection::connect(&socket_path(diag.root()))
            .await
            .expect("connect");
        let result = conn
            .initialize(
                ClientInfo {
                    kind: ClientKind::Tui,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: diag.root().to_path_buf(),
                    config_dir: diag.root().to_path_buf(),
                },
                &[],
                &[],
            )
            .await
            .expect("a compatible handshake succeeds");
        result.capabilities
    });

    for name in DIAGNOSTIC_METHODS {
        assert!(
            offered.iter().any(|capability| capability == name),
            "{name} is served, so it is advertised; the offer was {offered:?}"
        );
    }
    diag.stop();
}

// ===========================================================================
// 2. diagnostic.health
// ===========================================================================

/// The report's shape: thirteen keys, and every one of them a fact about this
/// process.
#[test]
fn health_reports_the_daemon_this_client_is_talking_to() {
    let diag = Diag::start();
    let (health, status) = block_on(async {
        let mut conn = client(diag.root()).await;
        let health = call(&mut conn, "diagnostic.health", json!({})).await;
        let status = call(&mut conn, "daemon.status", json!({})).await;
        (health, status)
    });

    assert_eq!(keys(&health), HEALTH_KEYS, "the report's keys are fixed");
    assert_eq!(
        health["instance_id"], status["instance_id"],
        "health describes the daemon `daemon.status` describes"
    );
    assert_eq!(health["pid"], status["pid"], "and the same process");
    assert_eq!(
        text(&health, "version"),
        env!("CARGO_PKG_VERSION"),
        "the version is this build's"
    );
    assert_eq!(
        number(&health, "protocol_version"),
        1,
        "the protocol version is the integer this connection negotiated, not a range"
    );
    assert_eq!(
        text(&health, "socket"),
        socket_path(diag.root()).display().to_string(),
        "the socket is the one this client connected to"
    );
    assert_eq!(
        text(&health, "log_path"),
        log_file(diag.root()).display().to_string(),
        "and the log is the file the daemon is writing"
    );
    assert!(
        number(&health, "uptime_secs") < 120,
        "a daemon started by this row is seconds old: {health}"
    );
    diag.stop();
}

/// The counters: one client per live connection, zero holds, zero operations,
/// and a store size the seeded tree actually carries.
#[test]
fn health_counts_the_connections_the_holds_and_the_operations() {
    let diag = Diag::start();
    let (alone, together) = block_on(async {
        let mut first = client(diag.root()).await;
        let alone = call(&mut first, "diagnostic.health", json!({})).await;
        let mut second = client(diag.root()).await;
        let together = call(&mut second, "diagnostic.health", json!({})).await;
        (alone, together)
    });

    assert_eq!(
        number(&alone, "clients"),
        1,
        "the asking connection counts itself: {alone}"
    );
    assert_eq!(
        number(&together, "clients"),
        2,
        "and a second connection is the second client: {together}"
    );
    assert_eq!(number(&alone, "holds"), 0, "nothing is holding");
    assert_eq!(
        number(&alone["operations"], "active"),
        0,
        "and nothing is running"
    );
    assert_eq!(
        keys(&alone["operations"]),
        vec!["active".to_string()],
        "an object, so a count of failures joins it without a version bump"
    );

    assert_eq!(
        keys(&alone["store"]),
        vec!["path".to_string(), "size_bytes".to_string()]
    );
    assert_eq!(
        text(&alone["store"], "path"),
        diag.root().join("accounts").display().to_string(),
        "the store path is the tree that holds every account's store"
    );
    assert!(
        number(&alone["store"], "size_bytes") > 0,
        "two seeded stores are more than zero bytes: {alone}"
    );
    diag.stop();
}

/// One entry per configured account, in configuration order, each carrying
/// its runtime, its watcher and how its last sync went.
#[test]
fn health_reports_one_entry_per_account() {
    let diag = Diag::start();
    let health = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "diagnostic.health", json!({})).await
    });

    let accounts = health["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("accounts is an array: {health}"))
        .clone();
    assert_eq!(
        accounts
            .iter()
            .map(|account| text(account, "name"))
            .collect::<Vec<_>>(),
        vec![ALPHA.to_string(), BETA.to_string()],
        "in configuration order, which is the order every other list uses"
    );

    for account in &accounts {
        assert_eq!(keys(account), HEALTH_ACCOUNT_KEYS);
        assert_eq!(
            text(account, "runtime"),
            "ready",
            "a seeded account whose lock nobody holds is serving: {account}"
        );
        assert_eq!(
            text(account, "watcher"),
            "running",
            "and a serving runtime is being watched: {account}"
        );
        assert_eq!(
            keys(&account["last_sync"]),
            vec!["finished_at".to_string(), "outcome".to_string()]
        );
        assert_eq!(
            account["last_sync"],
            json!({"finished_at": null, "outcome": null}),
            "nothing has synced, and the keys are there anyway: {account}"
        );
    }
    diag.stop();
}

/// The four fixed checks come first, in their fixed order, then one per
/// account, and every one of them carries a sentence.
#[test]
fn health_carries_the_four_fixed_checks_and_one_per_account() {
    let diag = Diag::start();
    let health = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "diagnostic.health", json!({})).await
    });

    let expected: Vec<String> = FIXED_CHECKS
        .iter()
        .map(|name| name.to_string())
        .chain([format!("account:{ALPHA}"), format!("account:{BETA}")])
        .collect();
    assert_eq!(
        check_names(&health),
        expected,
        "the fixed checks in their order, then the accounts in theirs"
    );

    for check in checks(&health) {
        assert_eq!(keys(&check), CHECK_KEYS, "one shape for every check");
        assert_eq!(
            text(&check, "status"),
            "ok",
            "a seeded root passes every check: {check}"
        );
        assert!(
            !text(&check, "detail").is_empty(),
            "a check always says why, including when it passes: {check}"
        );
    }
    diag.stop();
}

/// A `status` is one of three words, and the socket check is about *this*
/// socket.
#[test]
fn every_check_status_is_one_of_three_words() {
    let diag = Diag::start();
    let health = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "diagnostic.health", json!({})).await
    });

    for check in checks(&health) {
        let status = text(&check, "status");
        assert!(
            matches!(status.as_str(), "ok" | "warn" | "fail"),
            "a check status is ok, warn or fail, and this one is {status:?}: {check}"
        );
    }
    assert!(
        text(&check(&health, "socket_owner"), "detail")
            .contains(&socket_path(diag.root()).display().to_string()),
        "the socket check names the socket it looked at"
    );
    assert!(
        text(&check(&health, "log_writable"), "detail")
            .contains(&log_file(diag.root()).display().to_string()),
        "and the log check names the file it wrote to"
    );
    diag.stop();
}

/// An account whose engine lock is held elsewhere is a warning, not a failure:
/// the daemon still serves its reads, and a second `mp` holding the lock is a
/// normal state of this machine.
#[test]
fn an_account_blocked_by_another_engine_is_a_warning() {
    let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
    seed(tmp.path());
    let held = EngineLock::try_acquire_at(&engine_lock_path(tmp.path(), BETA), BETA)
        .expect("the engine lock is readable")
        .expect("and free before the daemon starts");
    let daemon = DaemonFixture::start_with(tmp.path(), None, &[]);
    wait_settled(tmp.path());

    let health = block_on(async {
        let mut conn = client(tmp.path()).await;
        call(&mut conn, "diagnostic.health", json!({})).await
    });

    let beta = check(&health, &format!("account:{BETA}"));
    assert_eq!(
        text(&beta, "status"),
        "warn",
        "a lock held elsewhere is a warning: {beta}"
    );
    assert!(
        !text(&beta, "detail").is_empty(),
        "and it says whose lock it is: {beta}"
    );
    assert_eq!(
        text(&check(&health, &format!("account:{ALPHA}")), "status"),
        "ok",
        "while the account nobody locked is unaffected"
    );

    let accounts = health["accounts"].as_array().expect("accounts").clone();
    let beta_entry = accounts
        .iter()
        .find(|account| account["name"] == json!(BETA))
        .expect("beta is listed");
    assert_eq!(
        text(beta_entry, "runtime"),
        "blocked",
        "and the entry agrees with the check: {beta_entry}"
    );
    assert_eq!(
        text(beta_entry, "watcher"),
        "stopped",
        "a blocked runtime watches nothing: {beta_entry}"
    );

    daemon.stop();
    drop(held);
}

/// An account with no store at all never came up, and that is a failure.
#[test]
fn an_account_whose_runtime_never_came_up_is_a_failure() {
    let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
    seed(tmp.path());
    fs::write(
        tmp.path().join("config.toml"),
        format!("{CONFIG}\n[[accounts]]\nname = \"gamma\"\ndefault_from = \"gamma@example.com\"\n"),
    )
    .expect("write config.toml");
    let daemon = DaemonFixture::start_with(tmp.path(), None, &[]);
    wait_settled(tmp.path());

    let health = block_on(async {
        let mut conn = client(tmp.path()).await;
        call(&mut conn, "diagnostic.health", json!({})).await
    });

    let gamma = check(&health, "account:gamma");
    assert_eq!(
        text(&gamma, "status"),
        "fail",
        "an account with no local store has no runtime at all: {gamma}"
    );
    assert_eq!(
        text(&check(&health, "store_open"), "status"),
        "warn",
        "one store of three that would not open is a warning about the set"
    );
    daemon.stop();
}

/// `diagnostic.health` takes no parameters, and says so rather than ignoring
/// one.
#[test]
fn health_and_log_path_take_no_parameters() {
    let diag = Diag::start();
    block_on(async {
        let mut conn = client(diag.root()).await;
        for method in ["diagnostic.health", "diagnostic.log_path"] {
            let error = call_err(&mut conn, method, json!({"account": ALPHA})).await;
            assert_eq!(
                error.code, INVALID_PARAMS,
                "{method} takes nothing, and a caller that sent something is told so: {error:?}"
            );
        }
    });
    diag.stop();
}

// ===========================================================================
// 3. diagnostic.log_path
// ===========================================================================

/// The surface `INT-02` and `OBS-05` name: one key, the file the daemon
/// writes, which is the file the TUI's `sf` opens.
#[test]
fn log_path_answers_the_file_the_daemon_is_writing() {
    let diag = Diag::start();
    let (answer, health) = block_on(async {
        let mut conn = client(diag.root()).await;
        let answer = call(&mut conn, "diagnostic.log_path", json!({})).await;
        let health = call(&mut conn, "diagnostic.health", json!({})).await;
        (answer, health)
    });

    assert_eq!(keys(&answer), vec!["path".to_string()], "one key");
    let path = PathBuf::from(text(&answer, "path"));
    assert_eq!(
        path,
        log_file(diag.root()),
        "which is `<data_dir>/logs/mailypoppins-<date>.log`"
    );
    assert!(path.is_absolute(), "absolute, because a GUI opens it");
    assert!(path.exists(), "and it is there before anybody asks");
    assert_eq!(
        answer["path"], health["log_path"],
        "the two surfaces name one file"
    );
    diag.stop();
}

// ===========================================================================
// 4. diagnostic.logs
// ===========================================================================

/// The answer's shape, and the parse of one line.
#[test]
fn logs_answers_parsed_lines_of_the_daemons_own_log() {
    let diag = Diag::start();
    let answer = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "diagnostic.logs", json!({})).await
    });

    assert_eq!(
        keys(&answer),
        vec![
            "lines".to_string(),
            "path".to_string(),
            "truncated".to_string()
        ]
    );
    assert_eq!(
        text(&answer, "path"),
        log_file(diag.root()).display().to_string(),
        "the answer names the file it read"
    );
    assert!(
        answer["truncated"].is_boolean(),
        "truncated is a boolean, always: {answer}"
    );

    let lines = answer["lines"]
        .as_array()
        .expect("lines is an array")
        .clone();
    assert!(
        !lines.is_empty(),
        "a daemon that answered this call has already logged its own startup"
    );
    assert!(
        lines.len() <= DEFAULT_LOG_LINES,
        "the default cap is {DEFAULT_LOG_LINES} and this answer carried {}",
        lines.len()
    );

    for line in &lines {
        assert_eq!(keys(line), LOG_LINE_KEYS, "one shape per line");
    }
    let listening = lines
        .iter()
        .find(|line| text(line, "message").contains("listening on"))
        .unwrap_or_else(|| panic!("the daemon logs its bind: {lines:?}"))
        .clone();
    assert_eq!(
        text(&listening, "level"),
        "info",
        "the level is lowercase, the vocabulary the `level` parameter takes"
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(&text(&listening, "ts")).is_ok(),
        "a timestamp is RFC3339 with an offset, so a bundle read elsewhere does \
         not lie about when something happened: {listening}"
    );
    diag.stop();
}

/// The lines are the *last* ones, in file order, and `lines` bounds them.
#[test]
fn logs_answers_the_tail_in_file_order() {
    let diag = Diag::start();
    let (few, many) = block_on(async {
        let mut conn = client(diag.root()).await;
        let few = call(&mut conn, "diagnostic.logs", json!({"lines": 3})).await;
        let many = call(&mut conn, "diagnostic.logs", json!({"lines": 500})).await;
        (few, many)
    });

    let few_lines = few["lines"].as_array().expect("lines").clone();
    let many_lines = many["lines"].as_array().expect("lines").clone();
    assert_eq!(few_lines.len(), 3, "`lines` is a bound and it is honoured");
    assert!(
        many_lines.len() >= few_lines.len(),
        "a larger bound answers at least as much"
    );
    assert_eq!(
        &many_lines[many_lines.len() - 3..],
        &few_lines[..],
        "the three are the last three, not the first three"
    );
    assert_eq!(
        few["truncated"],
        json!(true),
        "older matching lines were dropped, and the caller is told"
    );
    diag.stop();
}

/// `level` is a minimum, and an unknown one is refused rather than ignored.
#[test]
fn logs_filters_by_a_minimum_level() {
    let diag = Diag::start();
    block_on(async {
        let mut conn = client(diag.root()).await;
        let debug = call(&mut conn, "diagnostic.logs", json!({"lines": 500})).await;
        let warnings = call(
            &mut conn,
            "diagnostic.logs",
            json!({"lines": 500, "level": "warn"}),
        )
        .await;

        let all = debug["lines"].as_array().expect("lines").len();
        let warned = warnings["lines"].as_array().expect("lines").clone();
        assert!(
            warned.len() < all,
            "a startup logs more than warnings: {warned:?}"
        );
        for line in &warned {
            let level = text(line, "level");
            assert!(
                matches!(level.as_str(), "warn" | "error") || line["level"].is_null(),
                "a minimum of warn admits warn, error and the lines that carry no \
                 level at all, and this one is {level:?}: {line}"
            );
        }

        let error = call_err(&mut conn, "diagnostic.logs", json!({"level": "chatty"})).await;
        assert_eq!(
            error.code, INVALID_PARAMS,
            "an unknown level names the five it takes: {error:?}"
        );
    });
    diag.stop();
}

/// `since` drops what happened before it, on the instant rather than on the
/// string.
#[test]
fn logs_filters_by_since() {
    let diag = Diag::start();
    block_on(async {
        let mut conn = client(diag.root()).await;
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let empty = call(
            &mut conn,
            "diagnostic.logs",
            json!({"since": future.to_rfc3339()}),
        )
        .await;
        assert_eq!(
            empty["lines"],
            json!([]),
            "nothing was logged an hour from now: {empty}"
        );

        let past = chrono::Utc::now() - chrono::Duration::hours(24);
        let yesterday = call(
            &mut conn,
            "diagnostic.logs",
            json!({"since": past.to_rfc3339()}),
        )
        .await;
        assert!(
            !yesterday["lines"].as_array().expect("lines").is_empty(),
            "and this daemon started inside the last day: {yesterday}"
        );

        let error = call_err(&mut conn, "diagnostic.logs", json!({"since": "yesterday"})).await;
        assert_eq!(
            error.code, INVALID_PARAMS,
            "`since` is RFC3339 and a word is refused: {error:?}"
        );
    });
    diag.stop();
}

/// The cap is a refusal, not a silent clamp: a caller asking for a million
/// lines has misunderstood something, and answering five thousand of them
/// without a word hides it.
#[test]
fn logs_refuses_a_bound_above_the_cap() {
    let diag = Diag::start();
    block_on(async {
        let mut conn = client(diag.root()).await;
        let at_cap = call(
            &mut conn,
            "diagnostic.logs",
            json!({"lines": MAX_LOG_LINES}),
        )
        .await;
        assert!(
            at_cap["lines"].is_array(),
            "the cap itself is allowed: {at_cap}"
        );

        let error = call_err(
            &mut conn,
            "diagnostic.logs",
            json!({"lines": MAX_LOG_LINES + 1}),
        )
        .await;
        assert_eq!(error.code, INVALID_PARAMS);
        assert!(
            error.message.contains(&MAX_LOG_LINES.to_string()),
            "the refusal names the cap: {error:?}"
        );

        let unknown = call_err(&mut conn, "diagnostic.logs", json!({"tail": 10})).await;
        assert_eq!(
            unknown.code, INVALID_PARAMS,
            "and a parameter the method does not take is refused: {unknown:?}"
        );
    });
    diag.stop();
}

/// A line the format does not explain is kept, because that is what a panic
/// looks like.
#[test]
fn a_line_that_does_not_parse_is_kept_whole() {
    let diag = Diag::start();
    let path = log_file(diag.root());
    let stray = "thread 'main' panicked at src/nowhere.rs:1:1:";
    let mut text_on_disk = fs::read_to_string(&path).expect("read the daemon log");
    text_on_disk.push_str(stray);
    text_on_disk.push('\n');
    fs::write(&path, text_on_disk).expect("append a stray line");

    let answer = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "diagnostic.logs", json!({"lines": 500})).await
    });

    let line = answer["lines"]
        .as_array()
        .expect("lines")
        .iter()
        .find(|line| text(line, "message") == stray)
        .unwrap_or_else(|| panic!("the stray line survived: {answer}"))
        .clone();
    assert_eq!(
        line,
        json!({"ts": null, "level": null, "target": "", "message": stray}),
        "an unparsable line is the whole line, with nulls where the format was not found"
    );
    diag.stop();
}

// ===========================================================================
// 5. diagnostic.support_bundle
// ===========================================================================

/// Five files, named and ordered, and nothing else in the directory.
#[test]
fn a_bundle_holds_five_files_and_nothing_else() {
    let diag = Diag::start();
    let result = block_on(async {
        let mut conn = client(diag.root()).await;
        run_operation(&mut conn, "diagnostic.support_bundle", json!({})).await
    });

    assert_eq!(
        keys(&result),
        vec![
            "files".to_string(),
            "path".to_string(),
            "redactions".to_string()
        ]
    );
    let dir = bundle_dir(&result);
    assert!(
        dir.starts_with(diag.root()),
        "a bundle nobody placed lands under the data directory: {}",
        dir.display()
    );
    assert_eq!(
        result["files"],
        json!(BUNDLE_FILES),
        "the five names, in this order"
    );
    assert_eq!(
        tree(&dir),
        BUNDLE_FILES.map(str::to_string).to_vec(),
        "and the directory holds exactly them"
    );

    let health: Value = serde_json::from_str(
        &fs::read_to_string(dir.join("health.json")).expect("read health.json"),
    )
    .expect("health.json is JSON");
    assert_eq!(
        keys(&health),
        HEALTH_KEYS,
        "health.json is the `diagnostic.health` answer"
    );
    let status: Value = serde_json::from_str(
        &fs::read_to_string(dir.join("daemon-status.json")).expect("read daemon-status.json"),
    )
    .expect("daemon-status.json is JSON");
    assert_eq!(
        status["instance_id"], health["instance_id"],
        "both describe this daemon"
    );
    assert!(
        fs::read_to_string(dir.join("version.txt"))
            .expect("read version.txt")
            .contains(env!("CARGO_PKG_VERSION")),
        "version.txt carries what `mp --version` prints"
    );
    assert!(
        !fs::read_to_string(dir.join("log.txt"))
            .expect("read log.txt")
            .is_empty(),
        "and log.txt carries the tail of the daemon's log"
    );
    diag.stop();
}

/// No secret this daemon knows about appears anywhere in a redacted bundle.
///
/// The row the whole feature stands on: a support bundle is a file a user
/// mails to a stranger.
#[test]
fn a_redacted_bundle_carries_no_secret() {
    let diag = Diag::start();
    // One marker into the log as well, so the row is about all five files and
    // not only about the one every marker is seeded into.
    let logged = log_a_secret(diag.root(), MARKERS[0]);
    let result = block_on(async {
        let mut conn = client(diag.root()).await;
        run_operation(&mut conn, "diagnostic.support_bundle", json!({})).await
    });

    let dir = bundle_dir(&result);
    let text_of_bundle = bundle_text(&dir);
    for marker in MARKERS {
        assert!(
            !text_of_bundle.contains(marker),
            "{marker} survived into the bundle at {}",
            dir.display()
        );
    }
    assert!(
        text_of_bundle.contains(REDACTED),
        "and it says where the values went: {REDACTED}"
    );

    let log = fs::read_to_string(dir.join("log.txt")).expect("read log.txt");
    assert!(
        log.contains(&logged.replace(MARKERS[0], REDACTED)),
        "the logged line reached log.txt with its secret struck and the rest of \
         the sentence intact; log.txt:\n{log}"
    );
    assert_eq!(
        number(&result, "redactions"),
        MARKERS.len() as u64 + 1,
        "one replacement per secret the configuration carried, plus the one in the log: {result}"
    );

    let config = fs::read_to_string(dir.join("config.toml")).expect("read config.toml");
    assert!(
        config.contains("alpha@example.com"),
        "an address is not a secret and a bundle without them is useless"
    );
    assert!(
        config.contains("00000000-0000-0000-0000-000000000000"),
        "and neither is an OAuth2 client id"
    );
    assert!(
        config.contains(&format!("password = \"{REDACTED}\"")),
        "the key stays so a reader sees that a password was set: {config}"
    );
    diag.stop();
}

/// The control: without redaction the markers are there, so the row above is
/// not passing over an empty bundle.
#[test]
fn an_unredacted_bundle_is_the_control() {
    let diag = Diag::start();
    let logged = log_a_secret(diag.root(), MARKERS[0]);
    let result = block_on(async {
        let mut conn = client(diag.root()).await;
        run_operation(
            &mut conn,
            "diagnostic.support_bundle",
            json!({"redact": false}),
        )
        .await
    });

    let dir = bundle_dir(&result);
    let text_of_bundle = bundle_text(&dir);
    for marker in MARKERS {
        assert!(
            text_of_bundle.contains(marker),
            "{marker} is in the configuration this bundle copied verbatim"
        );
    }
    let log = fs::read_to_string(dir.join("log.txt")).expect("read log.txt");
    assert!(
        log.contains(&logged),
        "and the logged line reached log.txt whole, which is what the row above \
         strikes: log.txt:\n{log}"
    );
    assert_eq!(
        number(&result, "redactions"),
        0,
        "and nothing was struck: {result}"
    );
    diag.stop();
}

/// A bundle never copies the secrets file or the token cache, redacted or
/// not: ciphertext is still a credential, and no support case needs it.
#[test]
fn a_bundle_never_copies_the_secrets_file_or_the_token_cache() {
    let diag = Diag::start();
    fs::write(diag.root().join("secrets.enc"), b"MPSEC\x01ciphertext")
        .expect("write a secrets file");
    fs::create_dir_all(diag.root().join("tokens")).expect("create the token cache");
    fs::write(diag.root().join("tokens").join("alpha.json"), "{}").expect("write a token cache");

    let result = block_on(async {
        let mut conn = client(diag.root()).await;
        run_operation(
            &mut conn,
            "diagnostic.support_bundle",
            json!({"redact": false}),
        )
        .await
    });

    assert_eq!(
        tree(&bundle_dir(&result)),
        BUNDLE_FILES.map(str::to_string).to_vec(),
        "five files, and neither the secrets file nor a token among them"
    );
    diag.stop();
}

/// `out` is where the bundle goes, and it is absolute or it is refused.
#[test]
fn a_bundle_goes_where_out_names_and_out_is_absolute() {
    let diag = Diag::start();
    let chosen = diag.root().join("elsewhere").join("bundle");
    block_on(async {
        let mut conn = client(diag.root()).await;
        let result = run_operation(
            &mut conn,
            "diagnostic.support_bundle",
            json!({"out": chosen.display().to_string()}),
        )
        .await;
        assert_eq!(
            text(&result, "path"),
            chosen.display().to_string(),
            "the bundle is where the caller asked, parents created"
        );
        assert_eq!(tree(&chosen), BUNDLE_FILES.map(str::to_string).to_vec());

        let error = call_err(
            &mut conn,
            "diagnostic.support_bundle",
            json!({"out": "bundle"}),
        )
        .await;
        assert_eq!(
            error.code, INVALID_PARAMS,
            "a relative path means nothing to a daemon whose cwd is not the caller's: {error:?}"
        );

        let unknown = call_err(
            &mut conn,
            "diagnostic.support_bundle",
            json!({"format": "tar.gz"}),
        )
        .await;
        assert_eq!(unknown.code, INVALID_PARAMS);
    });
    diag.stop();
}

// ===========================================================================
// 6. The bootstrap snapshot and the event
// ===========================================================================

/// `snapshot.diagnostics`, which nothing filled until this unit, carries the
/// checks that are not `ok` - and nothing when they all are.
#[test]
fn the_bootstrap_carries_the_checks_that_are_not_ok() {
    let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
    seed(tmp.path());
    let held = EngineLock::try_acquire_at(&engine_lock_path(tmp.path(), BETA), BETA)
        .expect("the engine lock is readable")
        .expect("and free before the daemon starts");
    let daemon = DaemonFixture::start_with(tmp.path(), None, &[]);
    wait_settled(tmp.path());

    let (bootstrap, health) = block_on(async {
        let mut conn = client(tmp.path()).await;
        let bootstrap = call(&mut conn, "state.bootstrap", json!({})).await;
        let health = call(&mut conn, "diagnostic.health", json!({})).await;
        (bootstrap, health)
    });

    let diagnostics = bootstrap["snapshot"]["diagnostics"]
        .as_array()
        .expect("the snapshot always carries the array")
        .clone();
    let not_ok: Vec<Value> = checks(&health)
        .into_iter()
        .filter(|check| check["status"] != json!("ok"))
        .collect();
    assert!(
        !not_ok.is_empty(),
        "the held lock makes one check a warning, or this row proves nothing"
    );
    assert_eq!(
        diagnostics, not_ok,
        "the snapshot carries the checks that are not ok, verbatim and in report order"
    );
    for entry in &diagnostics {
        assert_eq!(keys(entry), CHECK_KEYS, "the same shape the report uses");
    }

    daemon.stop();
    drop(held);
}

/// A daemon over a healthy root reports an empty array, which is what every
/// suite that pins the bootstrap shape already asserts.
#[test]
fn a_healthy_daemon_bootstraps_with_no_diagnostics() {
    let diag = Diag::start();
    let bootstrap = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "state.bootstrap", json!({})).await
    });
    assert_eq!(
        bootstrap["snapshot"]["diagnostics"],
        json!([]),
        "nothing to report is an empty array, not a missing key"
    );
    diag.stop();
}

/// A check that flips publishes, and what it publishes is the check.
///
/// The trigger is a `config.reload` of a file that no longer loads: the daemon
/// keeps serving the configuration it has (that is `config.reload`'s own
/// contract) and `config_loaded` stops being `ok`.
#[test]
fn a_check_that_flips_publishes_an_event() {
    let diag = Diag::start();
    let payload = block_on(async {
        let mut conn = subscribed(diag.root()).await;
        fs::write(
            diag.root().join("config.toml"),
            "[[accounts]\nname = \"broken\"\n",
        )
        .expect("break config.toml");
        let error = call_err(&mut conn, "config.reload", json!({})).await;
        assert_eq!(
            error.code, CONFIG_INVALID,
            "the reload is refused, which is what flips the check: {error:?}"
        );
        await_kind(&mut conn, KIND_CHECK_CHANGED).await
    });

    assert_eq!(keys(&payload), CHECK_KEYS, "the event carries one check");
    assert_eq!(text(&payload, "name"), "config_loaded");
    assert_eq!(text(&payload, "status"), "fail");
    assert!(
        !text(&payload, "detail").is_empty(),
        "and the parser's sentence with it: {payload}"
    );

    let health = block_on(async {
        let mut conn = client(diag.root()).await;
        call(&mut conn, "diagnostic.health", json!({})).await
    });
    assert_eq!(
        check(&health, "config_loaded"),
        payload,
        "the event and the report are one value"
    );
    diag.stop();
}

/// A check that did not move publishes nothing: a daemon that republished its
/// whole check set on every evaluation would fill the activity overlay with
/// news that nothing happened.
#[test]
fn a_check_that_did_not_flip_publishes_nothing() {
    let diag = Diag::start();
    block_on(async {
        let mut conn = subscribed(diag.root()).await;
        for _ in 0..3 {
            call(&mut conn, "diagnostic.health", json!({})).await;
        }
        let quiet = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match conn.next_notification().await {
                    Some(notification) if notification.params["kind"] == KIND_CHECK_CHANGED => {
                        return notification
                    }
                    Some(_) => continue,
                    None => panic!("the daemon closed the connection"),
                }
            }
        })
        .await;
        assert!(
            quiet.is_err(),
            "three evaluations of an unchanged check set published {quiet:?}"
        );
    });
    diag.stop();
}

// ===========================================================================
// 7. The CLI, under the hidden `daemon` tree
// ===========================================================================

/// The three commands are under `mp daemon`, where `mp --help` never looks.
#[test]
fn the_three_commands_are_hidden_under_the_daemon_tree() {
    let diag = Diag::start();
    let daemon_help = stdout_text(&diag.mp(&["daemon", "--help"]));
    for command in ["health", "logs", "support-bundle"] {
        assert!(
            daemon_help.contains(command),
            "`mp daemon --help` lists {command}: {daemon_help}"
        );
    }
    let top = stdout_text(&diag.mp(&["--help"]));
    assert!(
        !top.contains("support-bundle"),
        "and `mp --help` does not, so the frozen help baseline does not move: {top}"
    );
    diag.stop();
}

/// `mp daemon health`, line for line, in the `✓`/`✗` style `mp daemon status`
/// and `mp daemon stop` already use.
#[test]
fn mp_daemon_health_prints_the_block_it_promises() {
    let diag = Diag::start();
    let out = diag.mp(&["daemon", "health"]);
    assert_eq!(
        code(&out),
        0,
        "a healthy daemon exits 0: {}",
        stderr_text(&out)
    );

    let lines = stdout_lines(&out);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("\u{2713} daemon healthy"),
        "the first line is the verdict: {lines:?}"
    );
    let labels: Vec<&str> = vec![
        "  instance:   ",
        "  version:    ",
        "  protocol:   ",
        "  pid:        ",
        "  uptime:     ",
        "  socket:     ",
        "  log:        ",
        "  clients:    ",
        "  holds:      ",
        "  operations: ",
        "  store:      ",
    ];
    for (line, label) in lines[1..=labels.len()].iter().zip(&labels) {
        assert!(
            line.starts_with(label),
            "expected a line starting {label:?}, got {line:?}; whole block: {lines:?}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("  account:    alpha (ready,")),
        "one line per account, with its runtime: {lines:?}"
    );
    for name in FIXED_CHECKS {
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("  \u{2713} {name}: "))),
            "a passing check is a ✓ line naming it: {lines:?}"
        );
    }
    diag.stop();
}

/// A warning is not a failure: exit 0, and a `⚠` line naming the check.
#[test]
fn mp_daemon_health_treats_a_warning_as_healthy() {
    let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
    seed(tmp.path());
    let held = EngineLock::try_acquire_at(&engine_lock_path(tmp.path(), BETA), BETA)
        .expect("the engine lock is readable")
        .expect("and free before the daemon starts");
    let daemon = DaemonFixture::start_with(tmp.path(), None, &[]);
    wait_settled(tmp.path());

    let warned = daemon.mp(&["daemon", "health"]);
    let lines = stdout_lines(&warned);
    assert_eq!(
        code(&warned),
        0,
        "a lock held elsewhere is a normal state of this machine: {lines:?}"
    );
    assert_eq!(
        lines.first().map(String::as_str),
        Some("\u{2713} daemon healthy, 1 check needs attention"),
        "and the first line says so: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(&format!("  \u{26a0} account:{BETA}: "))),
        "a warning is a ⚠ line: {lines:?}"
    );

    daemon.stop();
    drop(held);
}

/// A failing check is a `✗` first line and exit 1, so a script that runs this
/// in a health probe learns something.
#[test]
fn mp_daemon_health_exits_one_on_a_failing_check() {
    let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
    seed(tmp.path());
    fs::write(
        tmp.path().join("config.toml"),
        format!("{CONFIG}\n[[accounts]]\nname = \"gamma\"\ndefault_from = \"gamma@example.com\"\n"),
    )
    .expect("write config.toml");
    let daemon = DaemonFixture::start_with(tmp.path(), None, &[]);
    wait_settled(tmp.path());

    let out = daemon.mp(&["daemon", "health"]);
    let lines = stdout_lines(&out);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("\u{2717} daemon unhealthy, 1 check failing"),
        "an account with no store never came up: {lines:?}"
    );
    assert_eq!(code(&out), 1, "and the exit code carries the verdict");
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("  \u{2717} account:gamma: ")),
        "a failing check is a ✗ line: {lines:?}"
    );

    daemon.stop();
}

/// `--json` prints the wire object and nothing else, so a script parses one
/// line.
#[test]
fn mp_daemon_health_json_is_the_wire_object() {
    let diag = Diag::start();
    let out = diag.mp(&["daemon", "health", "--json"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr_text(&out));
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 1, "one line: {lines:?}");
    let object: Value = serde_json::from_str(&lines[0])
        .unwrap_or_else(|e| panic!("`--json` prints one JSON object: {e}: {lines:?}"));
    assert_eq!(keys(&object), HEALTH_KEYS, "the report, unchanged");
    diag.stop();
}

/// `mp daemon logs` prints the lines and nothing around them, so a pipe into
/// `grep` sees log lines only.
#[test]
fn mp_daemon_logs_prints_the_lines_and_no_header() {
    let diag = Diag::start();
    let out = diag.mp(&["daemon", "logs", "--lines", "5"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr_text(&out));
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 5, "five lines, and no banner: {lines:?}");
    assert!(
        !lines[0].starts_with('\u{2713}'),
        "a header would end up in every `mp daemon logs | grep`: {lines:?}"
    );

    let everything = diag.mp(&["daemon", "logs", "--lines", "500"]);
    let levelled = diag.mp(&["daemon", "logs", "--lines", "500", "--level", "warn"]);
    assert_eq!(code(&levelled), 0);
    assert!(
        stdout_lines(&levelled).len() < stdout_lines(&everything).len(),
        "a minimum level prints less than everything"
    );

    let json = diag.mp(&["daemon", "logs", "--lines", "5", "--json"]);
    let object: Value = serde_json::from_str(stdout_text(&json).trim_end())
        .unwrap_or_else(|e| panic!("`--json` prints one object: {e}: {}", stdout_text(&json)));
    assert_eq!(
        keys(&object),
        vec![
            "lines".to_string(),
            "path".to_string(),
            "truncated".to_string()
        ],
        "the wire answer, unchanged"
    );
    diag.stop();
}

/// `mp daemon support-bundle` writes where it is told and reports what it did.
#[test]
fn mp_daemon_support_bundle_reports_what_it_wrote() {
    let diag = Diag::start();
    let out = diag.mp(&["daemon", "support-bundle"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr_text(&out));
    let lines = stdout_lines(&out);
    let first = lines.first().cloned().unwrap_or_default();
    assert!(
        first.starts_with("\u{2713} wrote "),
        "the first line names the bundle: {lines:?}"
    );
    let written = PathBuf::from(first.trim_start_matches("\u{2713} wrote ").to_string());
    assert_eq!(tree(&written), BUNDLE_FILES.map(str::to_string).to_vec());
    assert_eq!(lines.get(1).map(String::as_str), Some("  files:      5"));
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("  redactions: 5"),
        "one per marker in the seeded configuration: {lines:?}"
    );

    let chosen = diag.root().join("chosen-bundle");
    let placed = diag.mp(&["daemon", "support-bundle", &chosen.display().to_string()]);
    assert_eq!(code(&placed), 0, "stderr: {}", stderr_text(&placed));
    assert_eq!(
        stdout_lines(&placed).first().map(String::as_str),
        Some(format!("\u{2713} wrote {}", chosen.display()).as_str())
    );
    assert_eq!(tree(&chosen), BUNDLE_FILES.map(str::to_string).to_vec());
    diag.stop();
}

/// `--no-redact` says so on the line where the count would have been, because
/// a bundle full of credentials must not look like any other bundle.
#[test]
fn mp_daemon_support_bundle_says_when_it_did_not_redact() {
    let diag = Diag::start();
    let out = diag.mp(&["daemon", "support-bundle", "--no-redact"]);
    assert_eq!(code(&out), 0, "stderr: {}", stderr_text(&out));
    let lines = stdout_lines(&out);
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("  redactions: none, --no-redact was given"),
        "{lines:?}"
    );
    diag.stop();
}

/// None of the three invents a daemon, and all three say the same thing when
/// there is none.
#[test]
fn the_three_commands_refuse_when_no_daemon_is_running() {
    let tmp = tempfile::tempdir().expect("a temporary diagnostics root");
    seed(tmp.path());

    for args in [
        vec!["daemon", "health"],
        vec!["daemon", "logs"],
        vec!["daemon", "support-bundle"],
    ] {
        let out = mp_no_daemon(&args, tmp.path());
        let lines = stdout_lines(&out);
        assert_eq!(
            code(&out),
            1,
            "`mp {}` with no daemon exits 1: {lines:?} {}",
            args.join(" "),
            stderr_text(&out)
        );
        assert_eq!(
            lines.first().map(String::as_str),
            Some("\u{2717} no daemon running"),
            "`mp {}`: {lines:?}",
            args.join(" ")
        );
        assert_eq!(
            lines.get(1).map(String::as_str),
            Some("  start one:  mp daemon start"),
            "`mp {}`: {lines:?}",
            args.join(" ")
        );
        assert!(
            !socket_path(tmp.path()).exists(),
            "and none of them started one behind the user's back"
        );
    }
}
