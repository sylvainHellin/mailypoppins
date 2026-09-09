//! The read-only method family behind the hidden `--daemon` flag (#0120, unit
//! P2-U10): `account.list`, `message.list`, and the two CLI routes that reach
//! them.
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/methods/` exists, against the shapes fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.3 (unit P2-U10), the
//! error table in section 3.0, and the prose in `docs/daemon-protocol.md`. It
//! fails against today's binary, which knows neither method and neither flag;
//! that failure *is* the proof the contract has no stub behind it. An
//! implementer (P2-U11) does not edit this file; they make it pass.
//!
//! # Surface under test
//!
//! ```text
//! account.list  {}
//!   -> {"accounts":[{"name":str,"default":bool,
//!                    "backend":"imap"|"graph",
//!                    "state":"opening"|"ready"|"blocked"}]}
//! message.list  {"account":str,"mailbox":str,"limit":u32|null}
//!   -> {"account":str,"mailbox":str,"total":u64,
//!       "messages":[{"uid":i64,"message_id":str,"from":str,"subject":str,
//!                    "date_sort":str,"date_display":str,
//!                    "flags":{"seen":bool,"answered":bool,"forwarded":bool},
//!                    "has_attachments":bool}]}
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! The plan fixes the shapes and leaves the semantics to the unit that pins
//! them. These are the decisions taken here; nothing may change them without a
//! protocol-changelog entry.
//!
//! - **`account.list` reports the configuration, not the runtimes.** Phase 2
//!   starts no account runtime (`MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES` is
//!   absent in every test here), so the list is every `[[accounts]]` entry of
//!   `config.toml`, in the file's order, which is the order `-A`-less commands
//!   already treat as authoritative. This is deliberately *not*
//!   `daemon.status`'s `accounts`, which stays empty without that environment
//!   variable (`src/daemon/lifecycle.rs::account_statuses`): a client asking
//!   which accounts exist must get an answer in Phase 2.
//! - **`default` is the first configured account**, and only it. The CLI has no
//!   other notion of a default: every `-A`-less command means "first in
//!   config".
//! - **`backend` follows `auth_method` alone.** `auth_method = "graph"` is
//!   `"graph"`; everything else is `"imap"`. Independent of `state`, which is
//!   why the fixture has a Graph account *with* a store and an IMAP account
//!   *without* one.
//! - **`state` is decided on disk.** An account whose store
//!   (`<data_dir>/accounts/<name>/store.sqlite3`) exists and opens is
//!   `"ready"`; an account configured but never synced, so with no store file,
//!   is not ready. Which of `"opening"` / `"blocked"` names that condition is
//!   the implementer's call, and the tests require only that it is one of the
//!   three states the protocol allows, that it is not `"ready"`, and that
//!   `account.list` and the `-32006` payload agree on it: two answers about the
//!   same account may not contradict each other.
//! - **`message.list` reads the store the CLI reads**, so every field is
//!   asserted against `mailypoppins::store::read::list_mailbox` rather than
//!   against a transcription: `uid`, `message_id` verbatim as ingest stored it
//!   (angle brackets included), `from` and `subject` as stored with an absent
//!   header travelling as `""` (the shape says `str`, not `str|null`),
//!   `has_attachments`, and `date_sort` from the same
//!   `mailypoppins::tui::app::resolve_date` the TUI and `mp dump-mailbox` use.
//! - **Order is the store's order**, `date_sort DESC, id DESC`, which is both
//!   the "newest first" the plan asks for and the order
//!   `tests/cli_read_surface_integration.rs` already pins for
//!   `mp list-messages`. The tie-break on `id` is why the assertion compares
//!   against `list_mailbox` instead of only checking that `date_sort` never
//!   increases.
//! - **`flags` carries exactly three bits.** The store's fourth axis,
//!   `\Flagged`, is not in the protocol shape, so a flagged message must not
//!   grow a fourth key; one fixture message sets all four flags to catch that.
//! - **`total` ignores `limit`**: it is how many messages the mailbox holds,
//!   the "In the store: N" of `mp list-messages`. `limit: null` and an omitted
//!   `limit` both mean "all"; `limit: 0` means none, because `null` is how
//!   "all" is spelled and a number may not mean the opposite of itself.
//! - **An empty mailbox of a ready account is not an error**, matching
//!   `mp list-messages` on the same store.
//! - **`--daemon` does not fall back.** With no daemon listening, `mp --daemon
//!   …` exits `4` (the "daemon unavailable" code of section 3.0) rather than
//!   quietly running the in-process path. Without that, "the routed output
//!   matches the direct output" would be satisfiable by ignoring the flag.
//! - **`mp account list` exists without `--daemon` too**, and is the oracle the
//!   routed run must match byte for byte. Since `mp --help` may not move, that
//!   subcommand is `hide = true` like `mp daemon` already is.
//!
//! # Process hygiene
//!
//! Every daemon this file starts is killed before the test returns, including
//! on panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it,
//! and [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`, including every `mp` invocation,
//! which is spawned and polled rather than waited on forever. Tests never touch
//! the test process's environment: each passes `HOME`,
//! `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` to the child through
//! `Command::env`, so they are safe to run in parallel.
//!
//! The harness is a trimmed copy of `tests/daemon_handshake.rs`'s, with the
//! fixture-store builder of `tests/cli_read_surface_integration.rs` folded in,
//! rather than a shared `tests/common/` module: the daemon files each need a
//! different half of it, and a shared module would have to be built into every
//! explicit `[[test]]` target.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mailypoppins::engine_lock::EngineLock;
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::parse::{AttachmentData, FetchedEmail};
use mailypoppins::store::{read, BlobStore, Store};
use mailypoppins::tui::app::resolve_date;
use mailypoppins::types::MessageFlags;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: the socket appearing, a frame arriving, an
/// `mp` run finishing. Generous, because it is a ceiling and never a sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// Exit code of a client that wanted the daemon and could not have it
/// (plan section 3.0, `EXIT_UNAVAILABLE` in `src/daemon/lifecycle.rs`).
const EXIT_UNAVAILABLE: i32 = 4;

/// The three account states `docs/daemon-protocol.md` allows.
const ACCOUNT_STATES: [&str; 3] = ["opening", "ready", "blocked"];

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory, holding the fixture
/// store every test in this file reads.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives
/// the test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// Four accounts, because the contract has four independent axes to get
    /// wrong: order and default (`alpha` first), a second ready account
    /// (`beta`), backend independent of state (`gamma`, Graph *and* ready),
    /// and state independent of backend (`delta`, IMAP and storeless).
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Self { root };
        sandbox.write_config();
        sandbox.ingest_fixture_messages();
        sandbox
    }

    fn write_config(&self) {
        // `delta` is configured and has no `[data]/accounts/delta` directory at
        // all: that absence is how this file produces a not-ready account,
        // since Phase 2 starts no runtimes and has no other way to make one.
        let config = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts]]
name = "beta"
default_from = "beta@example.com"

[[accounts]]
name = "gamma"
default_from = "gamma@example.com"
auth_method = "graph"

[accounts.oauth2]
client_id = "00000000-0000-0000-0000-000000000000"
tenant_id = "11111111-1111-1111-1111-111111111111"

[[accounts]]
name = "delta"
default_from = "delta@example.com"
"#;
        let path = self.config_dir().join("config.toml");
        fs::write(&path, config).expect("write config.toml");
    }

    fn ingest_fixture_messages(&self) {
        // The message with every axis set: read, answered, forwarded *and*
        // flagged, plus an attachment. The fourth flag is the one the protocol
        // shape does not carry.
        let mut reported = email(
            "Ivana <ivana@example.com>",
            "Bericht",
            "Thu, 2 Jul 2026 13:57:30 +0200",
            "the body of the report\n",
        );
        reported.cc = Some("petzold@example.com".to_string());
        reported.flags = MessageFlags {
            seen: true,
            answered: true,
            forwarded: true,
            flagged: true,
        };
        reported.has_attachments = true;
        reported.attachments = vec![AttachmentData {
            filename: "notes.pdf".to_string(),
            content: b"%PDF-1.4 notes".to_vec(),
            content_id: None,
        }];
        self.ingest("alpha", "inbox", 1, &reported);

        // Unread, no attachment, older: the second half of every field
        // assertion, and the message a `limit` of 1 must drop.
        self.ingest(
            "alpha",
            "inbox",
            2,
            &email(
                "bot@example.com",
                "Kickoff",
                "Wed, 1 Jul 2026 09:00:00 +0200",
                "kickoff\n",
            ),
        );
        // Another mailbox of the same account, which `--mailbox inbox` may not
        // reach.
        self.ingest(
            "alpha",
            "sent",
            1,
            &email(
                "alpha@example.com",
                "Re-Bericht",
                "Mon, 29 Jun 2026 12:00:00 +0000",
                "sent\n",
            ),
        );
        // Another account, which no listing of alpha may reach.
        self.ingest(
            "beta",
            "inbox",
            1,
            &email(
                "someone@example.com",
                "Only-In-Beta",
                "Fri, 1 May 2026 05:00:00 +0000",
                "beta body\n",
            ),
        );
        // The Graph account has a store like any other: `backend` and `state`
        // are independent.
        self.ingest(
            "gamma",
            "inbox",
            1,
            &email(
                "graph@example.com",
                "Only-In-Gamma",
                "Sat, 2 May 2026 05:00:00 +0000",
                "gamma body\n",
            ),
        );
    }

    fn ingest(&self, account: &str, mailbox: &str, uid: i64, message: &FetchedEmail) {
        let account_dir = self.account_dir(account);
        fs::create_dir_all(&account_dir).expect("account dir");
        let store = Store::open(account_dir.join("store.sqlite3")).expect("store");
        let blobs = BlobStore::new(account_dir.join("blobs"));
        ingest_message(
            &store,
            &blobs,
            &IngestInput {
                account,
                mailbox,
                uid,
                email: message,
                raw: None,
            },
        )
        .expect("ingest");
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

    /// `<data_dir>/accounts/<name>`, which is where `crate::config::account_dir`
    /// puts an account's store, blobs and `store.lock`.
    fn account_dir(&self, account: &str) -> PathBuf {
        self.data_dir().join("accounts").join(account)
    }

    /// The engine lock file of an account: `<account_dir>/store.lock`, the real
    /// path `EngineLock::try_acquire` builds.
    fn engine_lock_path(&self, account: &str) -> PathBuf {
        self.account_dir(account).join("store.lock")
    }

    fn socket(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.pid")
    }

    /// The identity a well-behaved client of this daemon sends.
    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// One account's store, opened read-only-ish by the test itself, so every
    /// expectation about `message.list` comes from the same rows the CLI reads
    /// rather than from a copy of them in this file.
    fn store(&self, account: &str) -> Store {
        Store::open(self.account_dir(account).join("store.sqlite3")).expect("open fixture store")
    }

    /// Spawn `mp daemon run` in the background, killed on drop, and wait until
    /// its socket accepts a connection.
    async fn start_daemon(&self) -> Proc {
        let child = self
            .command(&["daemon", "run"])
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

    /// An `mp` invocation pointed at this sandbox and at nothing else.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(MP);
        cmd.args(args)
            .env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env("NO_COLOR", "1")
            .env_remove("MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES")
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START");
        cmd
    }

    /// Run `mp` to completion under [`DEADLINE`], killing it rather than
    /// hanging the suite if it never exits.
    fn run(&self, args: &[&str]) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn mp {args:?}: {e}"));
        let start = Instant::now();
        loop {
            match child.try_wait().expect("polling an mp run") {
                Some(_) => break,
                None => {
                    if start.elapsed() >= DEADLINE {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("mp {args:?} did not exit within {DEADLINE:?}");
                    }
                    std::thread::sleep(TICK);
                }
            }
        }
        child
            .wait_with_output()
            .unwrap_or_else(|e| panic!("collecting the output of mp {args:?}: {e}"))
    }

    /// Run `mp`, require success, and return stdout.
    fn run_ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "mp {args:?} failed with {:?}\nstdout: {}\nstderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("stdout is UTF-8")
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

/// A spawned `mp daemon run`, killed and reaped on drop, so a panicking
/// assertion never leaves a daemon behind.
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One fixture email, in the shape ingest consumes.
fn email(from: &str, subject: &str, date: &str, body: &str) -> FetchedEmail {
    FetchedEmail {
        from: from.to_string(),
        to: "sylvain@example.com".to_string(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.to_string(),
        date: date.to_string(),
        body_text: body.to_string(),
        html_body: None,
        has_attachments: false,
        message_id: Some(format!("<{subject}@example.com>")),
        attachments: Vec::new(),
        flags: Default::default(),
        calendar_ics: None,
        event: None,
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

/// Connect and complete a well-formed handshake, requiring no capability: what
/// the daemon offers is P2-U9's business, and this file only calls methods.
async fn connected(sandbox: &Sandbox) -> Connection {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            sandbox.identity(),
            &[],
            &[],
        ),
    )
    .await
    .expect("a compatible handshake succeeds");
    conn
}

/// Call a method that must succeed.
async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method.to_string().as_str(), conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

/// Call a method that must fail, and return the typed RPC error behind it.
async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method.to_string().as_str(), conn.call(method, params))
        .await
        .expect_err("this call must be refused");
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{method}: expected a typed RPC error, got {other:?}"),
    }
}

/// Assert an error's code and return its `data`.
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

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

/// The keys of a JSON object, sorted, or a failure naming what came instead.
fn keys(value: &Value, label: &str) -> Vec<String> {
    let map = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} is a JSON object, got {value}"));
    let mut out: Vec<String> = map.keys().cloned().collect();
    out.sort();
    out
}

/// Assert an object carries exactly `expected` keys: no field renamed, none
/// missing, none invented.
fn assert_keys(value: &Value, expected: &[&str], label: &str) {
    let mut want: Vec<String> = expected.iter().map(|k| (*k).to_string()).collect();
    want.sort();
    assert_eq!(
        keys(value, label),
        want,
        "{label} carries exactly the pinned keys, got {value}"
    );
}

fn as_str<'a>(value: &'a Value, label: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{label} is a string, got {value}"))
}

fn as_bool(value: &Value, label: &str) -> bool {
    value
        .as_bool()
        .unwrap_or_else(|| panic!("{label} is a boolean, got {value}"))
}

fn as_i64(value: &Value, label: &str) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("{label} is an integer, got {value}"))
}

/// The `messages` array of a `message.list` result, as a slice.
fn messages<'a>(result: &'a Value, label: &str) -> &'a Vec<Value> {
    result["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: `messages` is an array, got {result}"))
}

/// The `accounts` entry named `name`, or a failure listing what was there.
fn account_entry<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("account.list: `accounts` is an array, got {result}"))
        .iter()
        .find(|entry| entry["name"] == json!(name))
        .unwrap_or_else(|| panic!("account.list has an entry for {name}, got {result}"))
}

/// What `message.list` must report for one stored row: the store's own values,
/// with the three nullable headers flattened to the empty string.
fn expected_message(row: &read::MessageRow) -> Value {
    let (_display, date_sort) = resolve_date(&row.date_display, &None, Path::new(""));
    let flags = row.flags();
    json!({
        "uid": row.uid,
        "message_id": row.message_id,
        "from": row.from.clone().unwrap_or_default(),
        "subject": row.subject.clone().unwrap_or_default(),
        "date_sort": date_sort,
        "date_display": row.date_display.clone().unwrap_or_default(),
        "flags": {
            "seen": flags.seen,
            "answered": flags.answered,
            "forwarded": flags.forwarded,
        },
        "has_attachments": row.has_attachments,
    })
}

// ---------------------------------------------------------------------------
// account.list
// ---------------------------------------------------------------------------

/// `account.list` answers with every configured account, in the file's order,
/// in exactly the pinned shape.
#[tokio::test]
async fn account_list_reports_every_configured_account_in_config_order() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let result = call(&mut conn, "account.list", json!({})).await;
    assert_keys(&result, &["accounts"], "the account.list result");

    let accounts = result["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("`accounts` is an array, got {result}"));
    let names: Vec<&str> = accounts
        .iter()
        .map(|entry| as_str(&entry["name"], "accounts[].name"))
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "beta", "gamma", "delta"],
        "every configured account is listed, in config.toml's order, got {result}"
    );

    for (index, entry) in accounts.iter().enumerate() {
        let name = as_str(&entry["name"], "accounts[].name").to_string();
        assert_keys(entry, &["name", "default", "backend", "state"], &name);
        assert_eq!(
            as_bool(&entry["default"], "accounts[].default"),
            index == 0,
            "only the first configured account is the default one, got {entry}"
        );
        let state = as_str(&entry["state"], "accounts[].state");
        assert!(
            ACCOUNT_STATES.contains(&state),
            "{name}: state is one of {ACCOUNT_STATES:?}, got {entry}"
        );
    }

    // `backend` follows `auth_method` and nothing else.
    for (name, backend) in [
        ("alpha", "imap"),
        ("beta", "imap"),
        ("gamma", "graph"),
        ("delta", "imap"),
    ] {
        assert_eq!(
            account_entry(&result, name)["backend"],
            json!(backend),
            "{name} is a {backend} account, got {result}"
        );
    }

    // `state` follows the store on disk. The Graph account has one, so a
    // backend cannot decide a state; `delta` has none, so it is not ready.
    for name in ["alpha", "beta", "gamma"] {
        assert_eq!(
            account_entry(&result, name)["state"],
            json!("ready"),
            "{name} has a store on disk and is therefore ready, got {result}"
        );
    }
    assert!(
        !sandbox.account_dir("delta").exists(),
        "the fixture leaves delta without an account directory"
    );
    assert_ne!(
        account_entry(&result, "delta")["state"],
        json!("ready"),
        "delta has no store on disk and is therefore not ready, got {result}"
    );
}

// ---------------------------------------------------------------------------
// message.list
// ---------------------------------------------------------------------------

/// `message.list` answers in the pinned shape, with the store's own values and
/// the store's own order.
#[tokio::test]
async fn message_list_reports_the_store_rows_newest_first() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let result = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox", "limit": null}),
    )
    .await;

    assert_keys(
        &result,
        &["account", "mailbox", "total", "messages"],
        "the message.list result",
    );
    assert_eq!(result["account"], json!("alpha"), "got {result}");
    assert_eq!(result["mailbox"], json!("inbox"), "got {result}");

    let rows = read::list_mailbox(&sandbox.store("alpha"), "alpha", "inbox").expect("store rows");
    assert_eq!(rows.len(), 2, "the fixture has two messages in alpha/inbox");
    assert_eq!(
        as_i64(&result["total"], "total"),
        rows.len() as i64,
        "`total` is how many messages the mailbox holds, got {result}"
    );

    let listed = messages(&result, "message.list");
    assert_eq!(
        listed.len(),
        rows.len(),
        "an unlimited listing returns every message, got {result}"
    );

    // Field for field against the store, in the store's order
    // (`date_sort DESC, id DESC`), which is what `mp list-messages` prints.
    for (index, (message, row)) in listed.iter().zip(rows.iter()).enumerate() {
        assert_keys(
            message,
            &[
                "uid",
                "message_id",
                "from",
                "subject",
                "date_sort",
                "date_display",
                "flags",
                "has_attachments",
            ],
            &format!("messages[{index}]"),
        );
        assert_keys(
            &message["flags"],
            &["seen", "answered", "forwarded"],
            &format!("messages[{index}].flags"),
        );
        assert_eq!(
            message,
            &expected_message(row),
            "messages[{index}] carries the stored row {} verbatim",
            row.message_id
        );
    }

    // The ordering, spelled out once as the property it is, so a listing that
    // happened to match a two-row store by luck is still caught.
    let dates: Vec<&str> = listed
        .iter()
        .map(|m| as_str(&m["date_sort"], "date_sort"))
        .collect();
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        dates, sorted,
        "the listing is ordered by date_sort, newest first, got {result}"
    );
    assert_eq!(
        as_str(&listed[0]["subject"], "subject"),
        "Bericht",
        "the newest message comes first, got {result}"
    );

    // The flagged bit of the newest message is stored and is not on the wire:
    // `flags` carries exactly the three axes the protocol names.
    assert!(
        rows[0].flags().flagged,
        "the fixture's newest message is flagged in the store"
    );

    // Nothing from another mailbox or another account leaks in.
    let printed = serde_json::to_string(&result).expect("serialise");
    assert!(
        !printed.contains("Re-Bericht") && !printed.contains("Only-In-Beta"),
        "one mailbox of one account is listed and nothing else, got {result}"
    );
}

/// `limit` caps the messages and never the total; `null` and an absent `limit`
/// both mean "all"; `0` means none.
#[tokio::test]
async fn message_list_honours_limit_and_null_means_all() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let limited = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox", "limit": 1}),
    )
    .await;
    assert_eq!(
        messages(&limited, "limit 1").len(),
        1,
        "a limit of 1 returns one message, got {limited}"
    );
    assert_eq!(
        as_i64(&limited["total"], "total"),
        2,
        "`total` counts the mailbox, not the page, got {limited}"
    );
    assert_eq!(
        limited["messages"][0]["subject"],
        json!("Bericht"),
        "a limited listing keeps the newest messages, got {limited}"
    );

    let none = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox", "limit": 0}),
    )
    .await;
    assert!(
        messages(&none, "limit 0").is_empty(),
        "a limit of 0 returns no message, since `null` is how `all` is spelled, got {none}"
    );
    assert_eq!(
        as_i64(&none["total"], "total"),
        2,
        "`total` is unaffected by a limit of 0, got {none}"
    );

    let generous = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox", "limit": 500}),
    )
    .await;
    assert_eq!(
        messages(&generous, "limit 500").len(),
        2,
        "a limit above the total returns the whole mailbox, got {generous}"
    );

    let omitted = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox"}),
    )
    .await;
    assert_eq!(
        messages(&omitted, "no limit").len(),
        2,
        "an absent `limit` means all, like an explicit null, got {omitted}"
    );

    // A mailbox of a ready account that holds nothing is an empty listing, not
    // an error, exactly as `mp list-messages -A beta --mailbox sent` is.
    let empty = call(
        &mut conn,
        "message.list",
        json!({"account": "beta", "mailbox": "sent", "limit": null}),
    )
    .await;
    assert_eq!(as_i64(&empty["total"], "total"), 0, "got {empty}");
    assert!(
        messages(&empty, "empty mailbox").is_empty(),
        "an empty mailbox is an empty listing, got {empty}"
    );
}

// ---------------------------------------------------------------------------
// The two error codes
// ---------------------------------------------------------------------------

/// An account no configuration names is `-32005`, carrying the name that was
/// asked for.
#[tokio::test]
async fn an_unconfigured_account_is_account_unknown() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let error = call_err(
        &mut conn,
        "message.list",
        json!({"account": "nowhere", "mailbox": "inbox", "limit": null}),
    )
    .await;
    let data = assert_code(&error, ErrorCode::AccountUnknown, "an unconfigured account");
    assert_keys(&data, &["account"], "the account_unknown data");
    assert_eq!(
        data["account"],
        json!("nowhere"),
        "account_unknown names the account that was asked for, got {data}"
    );
}

/// A configured account with no store on disk is `-32006`, carrying the account
/// and the same state `account.list` reports for it.
#[tokio::test]
async fn a_configured_account_without_a_store_is_account_not_ready() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    assert!(
        !sandbox.account_dir("delta").exists(),
        "the fixture leaves delta without an account directory"
    );

    let error = call_err(
        &mut conn,
        "message.list",
        json!({"account": "delta", "mailbox": "inbox", "limit": null}),
    )
    .await;
    let data = assert_code(&error, ErrorCode::AccountNotReady, "a storeless account");
    assert_keys(&data, &["account", "state"], "the account_not_ready data");
    assert_eq!(
        data["account"],
        json!("delta"),
        "account_not_ready names the account, got {data}"
    );

    let state = as_str(&data["state"], "account_not_ready.state");
    assert!(
        ACCOUNT_STATES.contains(&state),
        "the reported state is one of {ACCOUNT_STATES:?}, got {data}"
    );
    assert_ne!(
        state, "ready",
        "an account that is not ready is not reported as ready, got {data}"
    );

    // The two answers about one account agree.
    let listed = call(&mut conn, "account.list", json!({})).await;
    assert_eq!(
        account_entry(&listed, "delta")["state"],
        json!(state),
        "account.list and account_not_ready report the same state for delta, got {listed}"
    );
}

// ---------------------------------------------------------------------------
// The CLI routes
// ---------------------------------------------------------------------------

/// `mp --daemon account list` and `mp --daemon list-messages` print exactly
/// what the same commands print without the flag, on the same fixture store.
#[tokio::test]
async fn the_routed_cli_prints_what_the_direct_cli_prints() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;

    // `mp account list`: the routed run's oracle is the direct run, so this
    // pins the routing rather than a chosen output format.
    let direct_accounts = sandbox.run_ok(&["account", "list"]);
    let routed_accounts = sandbox.run_ok(&["--daemon", "account", "list"]);
    assert_eq!(
        routed_accounts, direct_accounts,
        "`mp --daemon account list` prints what `mp account list` prints"
    );
    for name in ["alpha", "beta", "gamma", "delta"] {
        assert!(
            direct_accounts.contains(name),
            "the account listing names {name}: {direct_accounts}"
        );
    }

    // `mp list-messages --mailbox inbox`, on the default account.
    let direct_inbox = sandbox.run_ok(&["list-messages", "--mailbox", "inbox"]);
    let routed_inbox = sandbox.run_ok(&["--daemon", "list-messages", "--mailbox", "inbox"]);
    assert_eq!(
        routed_inbox, direct_inbox,
        "`mp --daemon list-messages --mailbox inbox` prints what the direct command prints"
    );
    assert!(
        direct_inbox.contains("Bericht") && direct_inbox.contains("Kickoff"),
        "the listing is the fixture's inbox: {direct_inbox}"
    );

    // And `-A` still selects the account on the routed path, so a router that
    // ignored it and always answered for the default account is caught.
    let direct_beta = sandbox.run_ok(&["list-messages", "-A", "beta", "--mailbox", "inbox"]);
    let routed_beta = sandbox.run_ok(&[
        "--daemon",
        "list-messages",
        "-A",
        "beta",
        "--mailbox",
        "inbox",
    ]);
    assert_eq!(
        routed_beta, direct_beta,
        "the routed listing honours -A like the direct one"
    );
    assert!(
        direct_beta.contains("Only-In-Beta"),
        "the beta listing is beta's: {direct_beta}"
    );
    assert_ne!(
        direct_beta, direct_inbox,
        "the fixture's two accounts do not print the same listing"
    );
}

/// `--daemon` means the daemon: with none listening, the run fails with the
/// "daemon unavailable" exit code instead of silently taking the in-process
/// path.
#[tokio::test]
async fn the_daemon_flag_does_not_fall_back_to_the_direct_path() {
    let sandbox = Sandbox::new();
    // Deliberately no daemon.

    for args in [
        vec!["--daemon", "account", "list"],
        vec!["--daemon", "list-messages", "--mailbox", "inbox"],
    ] {
        let out = sandbox.run(&args);
        assert_eq!(
            out.status.code(),
            Some(EXIT_UNAVAILABLE),
            "mp {args:?} without a daemon exits {EXIT_UNAVAILABLE}, got {:?}\nstdout: {}\nstderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.stderr.is_empty(),
            "mp {args:?} says why it could not reach the daemon"
        );
    }
}

// ---------------------------------------------------------------------------
// The help surface
// ---------------------------------------------------------------------------

/// The whole `mp --help` surface is byte-identical to the pre-daemon baseline:
/// the `--daemon` flag is `hide = true`, and so is any subcommand the daemon
/// work added.
#[test]
fn the_help_surface_still_matches_the_pre_daemon_baseline() {
    let sandbox = Sandbox::new();

    let mut walked = String::new();
    collect_help(&sandbox, &[], &mut walked);
    let screens = walked.lines().filter(|l| l.starts_with("$ mp")).count();
    assert!(screens > 20, "the help walk collected {screens} screens");

    let baseline_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/baselines/pre-daemon/cli-help.txt");
    let baseline = fs::read_to_string(&baseline_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", baseline_path.display()));

    // Only the trailing newline count is normalised, which is the one
    // difference `docs/baselines/pre-daemon/README.md` records between the
    // captured artifact and the walk that produced it.
    let walked_body = walked.trim_end_matches('\n');
    let baseline_body = baseline.trim_end_matches('\n');
    if walked_body != baseline_body {
        let first = walked_body
            .lines()
            .zip(baseline_body.lines())
            .position(|(a, b)| a != b);
        let detail = match first {
            Some(index) => {
                let mine: Vec<&str> = walked_body.lines().collect();
                let theirs: Vec<&str> = baseline_body.lines().collect();
                format!(
                    "first difference at line {}:\n  built:    {:?}\n  baseline: {:?}",
                    index + 1,
                    mine.get(index),
                    theirs.get(index)
                )
            }
            None => format!(
                "the texts share a prefix; built has {} lines, the baseline {}",
                walked_body.lines().count(),
                baseline_body.lines().count()
            ),
        };
        panic!(
            "the help surface moved away from {}: the daemon flag and any daemon-era subcommand \
             must be hidden.\n{detail}",
            baseline_path.display()
        );
    }

    // Said directly as well, so the failure names the cause and not only the
    // symptom.
    let top = sandbox.run_ok(&["--help"]);
    assert!(
        !top.contains("--daemon"),
        "the --daemon flag is hidden from the top-level help: {top}"
    );
    for hidden in ["  daemon", "  account"] {
        assert!(
            !top.contains(hidden),
            "the daemon-era subcommand{hidden} is hidden from the top-level help: {top}"
        );
    }
}

/// The recursive help walk of `tests/cli_help_snapshot.rs` and
/// `scripts/capture-cli-help.sh`, reimplemented here so this test compares the
/// same document the baseline holds.
fn collect_help(sandbox: &Sandbox, path: &[&str], out: &mut String) {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    let help = sandbox.run_ok(&args);

    if path.is_empty() {
        out.push_str("$ mp --help\n");
    } else {
        out.push_str(&format!("$ mp {} --help\n", path.join(" ")));
    }
    out.push_str(&help);
    out.push('\n');

    for name in subcommand_names(&help) {
        let mut child: Vec<&str> = path.to_vec();
        child.push(&name);
        collect_help(sandbox, &child, out);
    }
}

/// Subcommand names listed in the `Commands:` block of a help text, in the
/// order clap prints them, skipping clap's auto-generated `help`.
fn subcommand_names(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line == "Commands:" {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        if !line.starts_with("  ") || line.starts_with("   ") {
            continue;
        }
        let name = line.trim_start().split_whitespace().next().unwrap_or("");
        if !name.is_empty() && name != "help" {
            names.push(name.to_string());
        }
    }
    names
}

// ---------------------------------------------------------------------------
// The engine lock
// ---------------------------------------------------------------------------

/// A daemon that serves `message.list` takes no engine lock, and neither does a
/// `mp --daemon` run: `<account_dir>/store.lock` stays free for whoever wants
/// to be the account's engine.
///
/// This is the Phase 2 invariant that keeps the daemon out of the way of a
/// running TUI or `mp sync`: reads go through the store, writes and queue
/// drains stay behind [`EngineLock`], and the daemon claims neither before
/// Phase 5.
#[tokio::test]
async fn serving_reads_holds_no_engine_lock() {
    let sandbox = Sandbox::new();
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    // Serve a read first, so the assertion is about a daemon that has actually
    // touched the account's store rather than one that never opened it.
    let served = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox", "limit": null}),
    )
    .await;
    assert_eq!(
        messages(&served, "message.list").len(),
        2,
        "the daemon answered the read, got {served}"
    );

    // The real lock file, at the real path.
    let lock_path = sandbox.engine_lock_path("alpha");
    assert!(
        sandbox.account_dir("alpha").is_dir(),
        "the fixture built alpha's account directory"
    );

    let held = EngineLock::try_acquire_at(&lock_path, "alpha")
        .expect("the engine lock file opens")
        .unwrap_or_else(|| {
            panic!(
                "{} was already locked while the daemon served a read: the daemon must not take \
                 the engine lock before Phase 5",
                lock_path.display()
            )
        });

    // And it stays true the other way round: with the test process holding the
    // engine lock, the daemon still serves, and so does the routed CLI.
    let again = call(
        &mut conn,
        "message.list",
        json!({"account": "alpha", "mailbox": "inbox", "limit": null}),
    )
    .await;
    assert_eq!(
        messages(&again, "message.list").len(),
        2,
        "the daemon serves reads while another process is the engine, got {again}"
    );

    let routed = sandbox.run_ok(&["--daemon", "list-messages", "--mailbox", "inbox"]);
    assert!(
        routed.contains("Bericht"),
        "a routed listing works while another process holds the engine lock: {routed}"
    );

    drop(held);

    // Once released, the lock is free again: nothing in this test leaked it.
    let after =
        EngineLock::try_acquire_at(&lock_path, "alpha").expect("the engine lock file opens");
    assert!(
        after.is_some(),
        "{} is free again once the test releases it",
        lock_path.display()
    );
}
