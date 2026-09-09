//! The `initialize` handshake: `mp-client` against a live daemon (#0120, unit
//! P2-U8).
//!
//! This file is a **contract test**: it is written before `mp_client` exists,
//! against the API fixed in `.agents/workflow/native-gui-daemon/plan.md`
//! section 3.3 (unit P2-U8), the error table in section 3.0, the handshake
//! prose in `docs/daemon-protocol.md`, and the two committed fixtures
//! `crates/mp-protocol/fixtures/initialize.{request,response}.json`. It does
//! not compile under `--features daemon` today, because `mp_client` is an
//! empty crate; that failure *is* the proof the contract has no stub behind
//! it. An implementer (P2-U9) does not edit this file; they make it pass.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! pub struct ClientInfo { pub kind: ClientKind, pub app_version: String }
//! pub enum ClientKind { Cli, Tui, Gui }
//! pub struct Identity { pub data_dir: PathBuf, pub config_dir: PathBuf }
//! pub struct Connection;
//! impl Connection {
//!     pub async fn connect(socket: &Path) -> Result<Self, ClientError>;
//!     pub async fn initialize(&mut self, info: ClientInfo, id: Identity,
//!                             required: &[&str], optional: &[&str])
//!         -> Result<InitializeResult, ClientError>;
//!     pub async fn call(&mut self, method: &str, params: serde_json::Value)
//!         -> Result<serde_json::Value, ClientError>;
//! }
//! pub struct InitializeResult { pub app_version: String, pub protocol: u32,
//!                               pub instance_id: String, pub capabilities: Vec<String>,
//!                               pub platform: PlatformInfo, pub config: ConfigStatus }
//! pub enum ConfigStatus { Absent, Loaded { accounts: usize }, Invalid { message: String } }
//! pub enum ClientError { Rpc(mp_protocol::RpcError), Io(std::io::Error), NotRunning, Protocol(String) }
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! - **The wire shape is the fixture, not an approximation.**
//!   [`initialize_request_matches_the_pinned_fixture_shape`] runs `mp_client`
//!   against a stub listener in this file and compares the request it emits,
//!   key by key and type by type, with
//!   `crates/mp-protocol/fixtures/initialize.request.json`; the stub then
//!   answers with the `result` of `initialize.response.json` verbatim and the
//!   parsed [`InitializeResult`] is asserted field by field. Producing and
//!   consuming exactly those two shapes is therefore checked without a daemon,
//!   and the daemon tests below are free to assert only on behaviour.
//! - **Forcing an incompatible protocol range.** `Connection::initialize`
//!   takes no range: `mp_client` speaks `{min: PROTOCOL_MIN, max: PROTOCOL_MAX}`
//!   and cannot ask for `{min: 2, max: 2}`. The `-32002` test therefore
//!   bypasses `mp_client` and writes one raw NDJSON frame over
//!   `tokio::net::UnixStream`, which is legitimate for a protocol test: the
//!   assertion is about the daemon's answer, and no client API is being
//!   exercised. Every other test goes through `mp_client`.
//! - **`ClientKind::Tui` serialises as `"tui"`** and `ClientInfo::app_version`
//!   travels as `client.version`, both taken from the fixture.
//! - **`ConfigStatus` mapping.** `config_status.state` of `"ok"` with an
//!   `accounts` count maps to `Loaded { accounts }`, `"absent"` to `Absent`,
//!   and `"invalid"` to `Invalid { message }` carrying the first problem's
//!   message. A sandbox with no `config.toml` therefore hands the client
//!   `ConfigStatus::Absent`.
//! - **`PlatformInfo`** carries at least `os` (the host, `std::env::consts::OS`)
//!   and `transport` (`"unix_socket"`), the two fields the fixture pins.
//! - **`ClientError` is `Debug`**, so a test can print the variant it did not
//!   expect. `ClientError::Rpc` carries the daemon's `RpcError` unchanged:
//!   code, message, and `data` exactly as they arrived.
//! - **Capabilities are checked against the daemon's own list.** Phase 2 does
//!   not fix which identifiers the daemon offers, so
//!   [`compatible_handshake_reports_version_instance_and_capabilities`] takes
//!   one identifier out of the handshake result and requires it on a second
//!   connection. The `-32003` test requires two identifiers no build can offer.
//! - **`daemon.status` before `initialize`** stays reachable. `src/daemon/server.rs`
//!   documents it as lifecycle rather than domain surface, and this file pins
//!   it so the P2-U9 gate cannot swallow it.
//!
//! # Process hygiene
//!
//! Every daemon this file starts is killed before the test returns, including
//! on panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it,
//! and [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`; nothing here sleeps and hopes.
//! Tests never touch the test process's environment: each passes `HOME`,
//! `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` to the child through
//! `Command::env`, so they are safe to run in parallel.
//!
//! The harness is a trimmed copy of the one in `tests/daemon_lifecycle.rs`
//! rather than a shared `tests/common/` module: the two files need different
//! halves of it (this one is async throughout and never runs `mp daemon
//! status`), and a shared module would have to be built into both explicit
//! `[[test]]` targets.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use mp_protocol::{ErrorCode, RpcError, PROTOCOL_MAX, PROTOCOL_MIN};

use mp_client::{
    ClientError, ClientInfo, ClientKind, ConfigStatus, Connection, Identity, InitializeResult,
};

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: the socket appearing, a frame arriving, a
/// connection closing. Generous, because it is a ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// JSON-RPC's own "invalid request", which a second `initialize` earns.
const INVALID_REQUEST: i32 = -32600;

/// JSON-RPC's own "parse error", which malformed JSON earns.
const PARSE_ERROR: i32 = -32700;

/// Connect / initialize / close cycles before the descriptor assertions.
const CYCLES: usize = 50;

/// How many descriptors the daemon may gain across [`CYCLES`] cycles. Not zero:
/// a tokio runtime is free to open an eventfd or grow a poll registration at
/// any point. A leak of one descriptor per connection would be 50.
const FD_SLACK: usize = 8;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory, plus a spare pair of
/// directories for the identity-mismatch tests.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives
/// the test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "config-other", "data-other"] {
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

    /// A config directory the daemon is not using, for the `-32001` tests.
    fn other_config_dir(&self) -> PathBuf {
        self.root.path().join("config-other")
    }

    /// A data directory the daemon is not using, for the `-32001` tests.
    fn other_data_dir(&self) -> PathBuf {
        self.root.path().join("data-other")
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

    /// The identity a well-behaved client of this daemon sends.
    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Spawn `mp daemon run` in the background, killed on drop, and wait until
    /// its socket accepts a connection.
    async fn start_daemon(&self) -> Proc {
        let child = Command::new(MP)
            .env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove("MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        let proc = Proc(Some(child));
        self.wait_socket_live().await;
        proc
    }

    /// Block until the socket accepts a connection, or fail the test.
    async fn wait_socket_live(&self) {
        let start = Instant::now();
        loop {
            if UnixStream::connect(self.socket()).await.is_ok() {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                self.socket().display()
            );
            tokio::time::sleep(TICK).await;
        }
    }

    /// `instance_id` out of `daemon.json`, which the handshake must echo.
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
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    // Safety: a pid read from a pid file we own.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
}

/// A spawned `mp` process, killed and reaped on drop, so a panicking assertion
/// never leaves a daemon behind.
struct Proc(Option<Child>);

impl Proc {
    fn pid(&self) -> i32 {
        self.0.as_ref().expect("process owned").id() as i32
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

/// Await `fut` under [`DEADLINE`], failing the test rather than the suite's
/// patience if the daemon never answers.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

// ---------------------------------------------------------------------------
// Client helpers
// ---------------------------------------------------------------------------

/// The client identification every test sends unless it is testing that field.
fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Tui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Connect to the daemon's socket, or fail the test.
async fn connect(sandbox: &Sandbox) -> Connection {
    within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds")
}

/// Connect and complete a well-formed handshake, or fail the test.
async fn connect_initialized(sandbox: &Sandbox) -> (Connection, InitializeResult) {
    let mut conn = connect(sandbox).await;
    let result = within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    (conn, result)
}

/// The typed RPC error behind a [`ClientError`], or a failure naming what came
/// instead.
fn rpc_error(error: ClientError, label: &str) -> RpcError {
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{label}: expected a typed RPC error, got {other:?}"),
    }
}

/// Assert an error's code and return its `data`, which every daemon-range code
/// in this file carries.
fn assert_code(error: &RpcError, expected: ErrorCode, label: &str) -> Value {
    assert_eq!(
        error.code,
        expected.code(),
        "{label}: expected {expected}, got code {} ({})",
        error.code,
        error.message
    );
    assert!(
        !error.message.trim().is_empty(),
        "{label}: {expected} carries a human-readable message"
    );
    error
        .data
        .clone()
        .unwrap_or_else(|| panic!("{label}: {expected} carries a `data` payload"))
}

/// `TempDir` paths differ from resolved paths on macOS (`/var` is a symlink to
/// `/private/var`), so compare canonicalised.
fn assert_same_path(reported: &Value, expected: &Path, label: &str) {
    let reported = reported
        .as_str()
        .unwrap_or_else(|| panic!("{label} is a string, got {reported}"));
    let lhs = fs::canonicalize(reported).unwrap_or_else(|_| PathBuf::from(reported));
    let rhs = fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
    assert_eq!(lhs, rhs, "{label} names the expected directory");
}

// ---------------------------------------------------------------------------
// Raw NDJSON helpers
//
// Used by the tests whose subject is the wire itself: the protocol-range
// refusal `mp_client` cannot ask for, and the malformed frame it would never
// send.
// ---------------------------------------------------------------------------

/// Read one `\n`-terminated frame, or `None` at end of stream.
async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Option<Value> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte).await.expect("reading a frame");
        if read == 0 {
            assert!(
                line.is_empty(),
                "the peer closed mid-frame, leaving {:?}",
                String::from_utf8_lossy(&line)
            );
            return None;
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    Some(serde_json::from_slice(&line).unwrap_or_else(|e| {
        panic!(
            "a frame is JSON: {e}\nframe: {}",
            String::from_utf8_lossy(&line)
        )
    }))
}

/// Write one frame: the JSON body plus the single `\n` terminator.
async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) {
    let mut bytes = serde_json::to_vec(value).expect("a frame is serialisable");
    bytes.push(b'\n');
    writer.write_all(&bytes).await.expect("writing a frame");
    writer.flush().await.expect("flushing a frame");
}

/// An `initialize` request in the shape `initialize.request.json` pins, with a
/// caller-chosen protocol range.
fn raw_initialize(id: i64, sandbox: &Sandbox, min: u32, max: u32) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "client": {"type": "tui", "version": env!("CARGO_PKG_VERSION")},
            "protocol": {"min": min, "max": max},
            "capabilities": {"required": [], "optional": []},
            "identity": {
                "data_dir": sandbox.data_dir().display().to_string(),
                "config_dir": sandbox.config_dir().display().to_string(),
            }
        }
    })
}

/// One committed fixture, parsed.
fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/mp-protocol/fixtures")
        .join(name);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("fixture {name} is JSON: {e}"))
}

/// The structure of a JSON value with its leaf values erased: objects keep
/// their keys, arrays collapse to `"array"`, scalars to their type name.
///
/// This is what makes "the same shape as the fixture" a mechanical assertion
/// that ignores the fixture's example values but catches a renamed, missing,
/// or invented field at any depth.
fn key_shape(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut shape = serde_json::Map::new();
            for (key, inner) in map {
                shape.insert(key.clone(), key_shape(inner));
            }
            Value::Object(shape)
        }
        Value::Array(_) => json!("array"),
        Value::String(_) => json!("string"),
        Value::Number(_) => json!("number"),
        Value::Bool(_) => json!("bool"),
        Value::Null => json!("null"),
    }
}

// ---------------------------------------------------------------------------
// A stub listener, for the two tests whose subject is `mp_client`'s own wire
// behaviour rather than the daemon's answers.
// ---------------------------------------------------------------------------

/// A one-connection Unix listener that captures the client's first frame and
/// answers it with a caller-supplied `result`.
struct Stub {
    _dir: TempDir,
    path: PathBuf,
    served: tokio::task::JoinHandle<Value>,
}

impl Stub {
    fn serving(result: Value) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("stub.sock");
        let listener = UnixListener::bind(&path).expect("bind the stub socket");
        let served = tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.expect("stub accept");
            let (mut reader, mut writer) = tokio::io::split(stream);
            let request = read_frame(&mut reader)
                .await
                .expect("the client sends a frame before it waits");
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": result,
            });
            write_frame(&mut writer, &response).await;
            request
        });
        Self {
            _dir: dir,
            path,
            served,
        }
    }

    /// The request the client sent, once it has been answered.
    async fn captured(self) -> Value {
        within("the stub server", self.served)
            .await
            .expect("the stub task did not panic")
    }
}

// ---------------------------------------------------------------------------
// The handshake against a live daemon
// ---------------------------------------------------------------------------

/// A compatible handshake succeeds and reports version, instance, protocol,
/// platform, config status and capabilities; and a capability the daemon
/// advertises is accepted when a second client requires it.
#[tokio::test]
async fn compatible_handshake_reports_version_instance_and_capabilities() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let (_conn, result) = connect_initialized(&sandbox).await;

    assert_eq!(
        result.app_version,
        env!("CARGO_PKG_VERSION"),
        "the handshake reports this build's application version"
    );
    assert!(
        result.protocol >= PROTOCOL_MIN && result.protocol <= PROTOCOL_MAX,
        "the selected protocol version {} is inside the daemon's range {PROTOCOL_MIN}..={PROTOCOL_MAX}",
        result.protocol
    );
    assert_eq!(
        result.instance_id,
        sandbox.recorded_instance_id(),
        "the handshake and daemon.json agree on instance_id"
    );
    assert_eq!(
        result.platform.os,
        std::env::consts::OS,
        "platform.os names the host operating system"
    );
    assert_eq!(
        result.platform.transport, "unix_socket",
        "platform.transport names the transport in use"
    );
    assert!(
        matches!(result.config, ConfigStatus::Absent),
        "a sandbox with no config.toml reports an absent configuration, got {:?}",
        result.config
    );

    let mut sorted = result.capabilities.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        result.capabilities.len(),
        "the capability list has no duplicates: {:?}",
        result.capabilities
    );
    assert!(
        result.capabilities.iter().all(|c| !c.trim().is_empty()),
        "no capability identifier is blank: {:?}",
        result.capabilities
    );

    // Whatever the daemon advertises must be acceptable as a requirement,
    // which is the only capability round trip Phase 2 can assert without
    // freezing the identifier list.
    if let Some(offered) = result.capabilities.first() {
        let mut second = connect(&sandbox).await;
        let accepted = within(
            "requiring an advertised capability",
            second.initialize(client_info(), sandbox.identity(), &[offered.as_str()], &[]),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("requiring the advertised capability {offered:?} failed: {e:?}")
        });
        assert_eq!(
            accepted.instance_id, result.instance_id,
            "both connections reached the same daemon instance"
        );
    }
}

/// A domain method before `initialize` is refused with `-32000`, and the
/// connection stays usable for the handshake that should have come first.
#[tokio::test]
async fn a_domain_method_before_initialize_is_not_initialized() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let mut conn = connect(&sandbox).await;
    let error = within(
        "account.list before initialize",
        conn.call("account.list", json!({})),
    )
    .await
    .expect_err("a domain method before initialize is refused");
    let error = rpc_error(error, "account.list before initialize");
    let data = assert_code(&error, ErrorCode::NotInitialized, "account.list");
    assert_eq!(
        data,
        json!({}),
        "not_initialized carries an empty data object, as the error table fixes"
    );

    // The refusal is per-request, not a connection kill: the handshake still
    // works on the same connection.
    within(
        "initialize after a refused domain method",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("the connection survives a not_initialized refusal");
}

/// `daemon.status` is lifecycle surface and answers before any handshake, which
/// is what lets `mp daemon status` describe a daemon it cannot negotiate with.
#[tokio::test]
async fn daemon_status_answers_without_an_initialize() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let mut conn = connect(&sandbox).await;
    let status = within(
        "daemon.status before initialize",
        conn.call("daemon.status", json!({})),
    )
    .await
    .expect("daemon.status is reachable before initialize");

    assert_eq!(
        status["instance_id"].as_str(),
        Some(sandbox.recorded_instance_id().as_str()),
        "daemon.status reports the running instance, got {status}"
    );
}

/// A client whose `data_dir` matches but whose `config_dir` differs is refused
/// with `-32001`, naming all four directories.
#[tokio::test]
async fn a_differing_config_dir_is_an_identity_mismatch() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let identity = Identity {
        data_dir: sandbox.data_dir(),
        config_dir: sandbox.other_config_dir(),
    };
    let mut conn = connect(&sandbox).await;
    let error = within(
        "initialize with a differing config_dir",
        conn.initialize(client_info(), identity, &[], &[]),
    )
    .await
    .expect_err("a differing config_dir is refused");
    let error = rpc_error(error, "differing config_dir");
    let data = assert_code(&error, ErrorCode::IdentityMismatch, "differing config_dir");

    assert_same_path(
        &data["daemon"]["data_dir"],
        &sandbox.data_dir(),
        "daemon.data_dir",
    );
    assert_same_path(
        &data["daemon"]["config_dir"],
        &sandbox.config_dir(),
        "daemon.config_dir",
    );
    assert_same_path(
        &data["client"]["data_dir"],
        &sandbox.data_dir(),
        "client.data_dir",
    );
    assert_same_path(
        &data["client"]["config_dir"],
        &sandbox.other_config_dir(),
        "client.config_dir",
    );
}

/// The reverse: `config_dir` matches, `data_dir` differs, same `-32001` with
/// all four directories.
#[tokio::test]
async fn a_differing_data_dir_is_an_identity_mismatch() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let identity = Identity {
        data_dir: sandbox.other_data_dir(),
        config_dir: sandbox.config_dir(),
    };
    let mut conn = connect(&sandbox).await;
    let error = within(
        "initialize with a differing data_dir",
        conn.initialize(client_info(), identity, &[], &[]),
    )
    .await
    .expect_err("a differing data_dir is refused");
    let error = rpc_error(error, "differing data_dir");
    let data = assert_code(&error, ErrorCode::IdentityMismatch, "differing data_dir");

    assert_same_path(
        &data["daemon"]["data_dir"],
        &sandbox.data_dir(),
        "daemon.data_dir",
    );
    assert_same_path(
        &data["daemon"]["config_dir"],
        &sandbox.config_dir(),
        "daemon.config_dir",
    );
    assert_same_path(
        &data["client"]["data_dir"],
        &sandbox.other_data_dir(),
        "client.data_dir",
    );
    assert_same_path(
        &data["client"]["config_dir"],
        &sandbox.config_dir(),
        "client.config_dir",
    );
}

/// A client that can only speak version 2 is refused with `-32002` carrying
/// both ranges.
///
/// `Connection::initialize` takes no range, so this one frame is written raw
/// over the socket: the subject is the daemon's refusal, not the client API.
#[tokio::test]
async fn an_unsupported_protocol_range_is_protocol_incompatible() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let stream = UnixStream::connect(sandbox.socket())
        .await
        .expect("connect to the daemon socket");
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_frame(&mut writer, &raw_initialize(1, &sandbox, 2, 2)).await;

    let reply = within("the -32002 refusal", read_frame(&mut reader))
        .await
        .expect("the daemon answers an incompatible initialize");
    assert_eq!(reply["id"], json!(1), "the daemon echoes the request id");
    assert_eq!(
        reply["error"]["code"],
        json!(ErrorCode::ProtocolIncompatible.code()),
        "an unsupported range is protocol_incompatible, got {reply}"
    );
    assert_eq!(
        reply["error"]["data"]["client"],
        json!({"min": 2, "max": 2}),
        "the refusal repeats the client's range, got {reply}"
    );
    assert_eq!(
        reply["error"]["data"]["daemon"],
        json!({"min": PROTOCOL_MIN, "max": PROTOCOL_MAX}),
        "the refusal names the daemon's range, got {reply}"
    );
}

/// A required capability the daemon does not offer is refused with `-32003`
/// listing exactly the missing identifiers.
#[tokio::test]
async fn a_missing_required_capability_is_capability_missing() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let missing = [
        "mailypoppins.no_such_capability.alpha",
        "mailypoppins.no_such_capability.beta",
    ];
    let mut conn = connect(&sandbox).await;
    let error = within(
        "initialize requiring an unknown capability",
        conn.initialize(client_info(), sandbox.identity(), &missing, &[]),
    )
    .await
    .expect_err("an unofferable required capability is refused");
    let error = rpc_error(error, "missing capability");
    let data = assert_code(&error, ErrorCode::CapabilityMissing, "missing capability");

    let mut listed: Vec<String> = data["missing"]
        .as_array()
        .unwrap_or_else(|| panic!("capability_missing carries a `missing` array, got {data}"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("`missing` holds identifier strings, got {data}"))
                .to_string()
        })
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        missing.map(str::to_string).to_vec(),
        "capability_missing lists exactly the required identifiers the daemon lacks"
    );
}

/// A second `initialize` on one connection is `-32600 invalid request`: the
/// handshake happens once per connection.
#[tokio::test]
async fn a_second_initialize_is_an_invalid_request() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    let (mut conn, _first) = connect_initialized(&sandbox).await;
    let error = within(
        "a second initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect_err("initialize twice on one connection is refused");
    let error = rpc_error(error, "second initialize");
    assert_eq!(
        error.code, INVALID_REQUEST,
        "a second initialize is JSON-RPC's invalid request, got code {} ({})",
        error.code, error.message
    );
}

/// Malformed JSON closes the offending connection and nothing else.
#[tokio::test]
async fn malformed_json_closes_only_the_offending_connection() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    // A well-behaved connection, opened first so it is demonstrably older than
    // the offence.
    let (mut good, before) = connect_initialized(&sandbox).await;

    let offender = UnixStream::connect(sandbox.socket())
        .await
        .expect("connect to the daemon socket");
    let (mut reader, mut writer) = tokio::io::split(offender);
    writer
        .write_all(b"{ this is not JSON at all \n")
        .await
        .expect("write the malformed frame");
    writer.flush().await.expect("flush the malformed frame");

    // The daemon may answer with a parse error before it closes, or close
    // straight away; either way the stream ends and it ends soon.
    let first = within("the answer to a malformed frame", read_frame(&mut reader)).await;
    if let Some(reply) = &first {
        assert_eq!(
            reply["error"]["code"],
            json!(PARSE_ERROR),
            "malformed JSON is answered with a parse error if it is answered at all, got {reply}"
        );
        let trailing = within("the close after a parse error", read_frame(&mut reader)).await;
        assert!(
            trailing.is_none(),
            "the offending connection is closed after the parse error, got {trailing:?}"
        );
    }

    // The other connection is untouched.
    let status = within(
        "daemon.status on the surviving connection",
        good.call("daemon.status", json!({})),
    )
    .await
    .expect("the second connection keeps working after another one was closed");
    assert_eq!(
        status["instance_id"].as_str(),
        Some(before.instance_id.as_str()),
        "the surviving connection still talks to the same daemon instance, got {status}"
    );

    // And a fresh connection still gets served, so the listener survived too.
    let (_third, after) = connect_initialized(&sandbox).await;
    assert_eq!(
        after.instance_id, before.instance_id,
        "the daemon still accepts new connections after a malformed frame"
    );
}

/// Fifty connect / initialize / close cycles leak no descriptor: the fifty
/// first connection is served, and on Linux the daemon's descriptor count has
/// not grown.
#[tokio::test]
async fn fifty_connect_initialize_close_cycles_leak_no_descriptor() {
    let sandbox = Sandbox::new();
    let daemon = sandbox.start_daemon().await;

    // One warm-up cycle first, so the baseline includes whatever the daemon
    // opens lazily on its first client rather than counting it as a leak.
    {
        let (conn, _) = connect_initialized(&sandbox).await;
        drop(conn);
    }
    let baseline = descriptor_count(daemon.pid());

    for cycle in 0..CYCLES {
        let mut conn = within("connect in a cycle", Connection::connect(&sandbox.socket()))
            .await
            .unwrap_or_else(|e| panic!("cycle {cycle}: connect failed: {e:?}"));
        let result = within(
            "initialize in a cycle",
            conn.initialize(client_info(), sandbox.identity(), &[], &[]),
        )
        .await
        .unwrap_or_else(|e| panic!("cycle {cycle}: initialize failed: {e:?}"));
        assert_eq!(
            result.instance_id,
            sandbox.recorded_instance_id(),
            "cycle {cycle}: reached the daemon this sandbox started"
        );
        drop(conn);
    }

    // The 51st connection, which is the portable half of the assertion.
    let (_conn, result) = connect_initialized(&sandbox).await;
    assert_eq!(
        result.instance_id,
        sandbox.recorded_instance_id(),
        "the connection after {CYCLES} cycles is served like the first"
    );

    if let Some(baseline) = baseline {
        // The daemon reaps a closed connection asynchronously, so give it the
        // usual bounded poll before believing the count.
        let start = Instant::now();
        let mut observed = descriptor_count(daemon.pid()).expect("descriptor count on Linux");
        while observed > baseline + FD_SLACK && start.elapsed() < DEADLINE {
            tokio::time::sleep(TICK).await;
            observed = descriptor_count(daemon.pid()).expect("descriptor count on Linux");
        }
        assert!(
            observed <= baseline + FD_SLACK,
            "the daemon held {observed} descriptors after {CYCLES} connect/initialize/close \
             cycles, up from {baseline}: more than the {FD_SLACK} a runtime may legitimately \
             add, so a connection is leaking one"
        );
    }
}

/// Open descriptors of `pid`, or `None` where `/proc` does not answer.
fn descriptor_count(pid: i32) -> Option<usize> {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()
        .map(|entries| entries.count())
}

// ---------------------------------------------------------------------------
// The wire shape, against the committed fixtures
// ---------------------------------------------------------------------------

/// `mp_client` emits exactly the `initialize` request the fixture pins, and
/// parses exactly the `initialize` result the fixture pins.
#[tokio::test]
async fn initialize_request_matches_the_pinned_fixture_shape() {
    let expected = fixture("initialize.request.json");
    let response = fixture("initialize.response.json");
    let stub = Stub::serving(response["result"].clone());

    let mut conn = within("Connection::connect", Connection::connect(&stub.path))
        .await
        .expect("connect to the stub socket");
    let result = within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Tui,
                app_version: "0.9.0".to_string(),
            },
            Identity {
                data_dir: PathBuf::from("/home/alice/.local/share/mailypoppins"),
                config_dir: PathBuf::from("/home/alice/.config/mailypoppins"),
            },
            &["state.bootstrap", "state.events"],
            &["message.browser_open", "calendar.rsvp"],
        ),
    )
    .await
    .expect("the fixture result parses");

    // What went out.
    let sent = stub.captured().await;
    assert_eq!(
        key_shape(&sent),
        key_shape(&expected),
        "the initialize request has the shape crates/mp-protocol/fixtures/initialize.request.json \
         pins\nsent: {sent:#}"
    );
    assert_eq!(sent["jsonrpc"], json!("2.0"), "sent: {sent:#}");
    assert_eq!(sent["method"], json!("initialize"), "sent: {sent:#}");
    assert_eq!(
        sent["params"]["client"],
        json!({"type": "tui", "version": "0.9.0"}),
        "ClientKind::Tui travels as \"tui\" and app_version as client.version\nsent: {sent:#}"
    );
    assert_eq!(
        sent["params"]["protocol"],
        json!({"min": PROTOCOL_MIN, "max": PROTOCOL_MAX}),
        "the client declares the range this build supports\nsent: {sent:#}"
    );
    assert_eq!(
        sent["params"]["capabilities"],
        json!({
            "required": ["state.bootstrap", "state.events"],
            "optional": ["message.browser_open", "calendar.rsvp"],
        }),
        "both capability lists travel in declaration order\nsent: {sent:#}"
    );
    assert_eq!(
        sent["params"]["identity"],
        json!({
            "data_dir": "/home/alice/.local/share/mailypoppins",
            "config_dir": "/home/alice/.config/mailypoppins",
        }),
        "the identity pair travels always and as a pair\nsent: {sent:#}"
    );

    // What came back.
    assert_eq!(result.app_version, "0.9.0", "daemon.version");
    assert_eq!(result.protocol, 1, "protocol.selected");
    assert_eq!(
        result.instance_id, "01J8Z6Q9X4V3N2M1K0H7G5F4D3",
        "instance_id"
    );
    assert_eq!(
        result.capabilities,
        vec![
            "state.bootstrap".to_string(),
            "state.events".to_string(),
            "message.browser_open".to_string(),
            "calendar.rsvp".to_string(),
        ],
        "capabilities arrive in the daemon's order"
    );
    assert_eq!(result.platform.os, "linux", "platform.os");
    assert_eq!(
        result.platform.transport, "unix_socket",
        "platform.transport"
    );
    assert!(
        matches!(result.config, ConfigStatus::Loaded { accounts: 2 }),
        "config_status state \"ok\" with two accounts is Loaded {{ accounts: 2 }}, got {:?}",
        result.config
    );
}

/// The three `config_status` states map onto the three [`ConfigStatus`]
/// variants, which is the only part of the result whose parse is a decision
/// rather than a copy.
#[tokio::test]
async fn config_status_states_map_onto_the_client_variants() {
    let template = fixture("initialize.response.json")["result"].clone();

    // Absent: no config file on disk.
    let mut absent = template.clone();
    absent["config_status"] = json!({
        "state": "absent",
        "path": "/home/alice/.config/mailypoppins/config.toml",
        "accounts": 0,
        "problems": [],
    });
    let result = handshake_against(absent).await;
    assert!(
        matches!(result.config, ConfigStatus::Absent),
        "state \"absent\" is ConfigStatus::Absent, got {:?}",
        result.config
    );

    // Invalid: the daemon serves, and says why the configuration does not load.
    let mut invalid = template;
    invalid["config_status"] = json!({
        "state": "invalid",
        "path": "/home/alice/.config/mailypoppins/config.toml",
        "accounts": 0,
        "problems": ["line 4: unknown backend \"pigeon\""],
    });
    let result = handshake_against(invalid).await;
    match result.config {
        ConfigStatus::Invalid { message } => assert!(
            message.contains("pigeon"),
            "Invalid carries the daemon's problem text, got {message:?}"
        ),
        other => panic!("state \"invalid\" is ConfigStatus::Invalid, got {other:?}"),
    }
}

/// Run one handshake against a stub answering with `result`.
async fn handshake_against(result: Value) -> InitializeResult {
    let stub = Stub::serving(result);
    let mut conn = within("Connection::connect", Connection::connect(&stub.path))
        .await
        .expect("connect to the stub socket");
    let parsed = within(
        "Connection::initialize",
        conn.initialize(
            client_info(),
            Identity {
                data_dir: PathBuf::from("/home/alice/.local/share/mailypoppins"),
                config_dir: PathBuf::from("/home/alice/.config/mailypoppins"),
            },
            &[],
            &[],
        ),
    )
    .await
    .expect("the stub result parses");
    let _ = stub.captured().await;
    parsed
}
