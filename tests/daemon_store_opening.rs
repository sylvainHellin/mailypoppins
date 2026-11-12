//! A store that takes long to open holds up its own account and nothing else
//! (PERSO-109).
//!
//! `Store::open` runs `PRAGMA integrity_check` on the first open of a file in
//! a process, which took 12 to 16 s on a 1.6 GB store. The daemon used to run
//! one such open per account on its main thread, to seed each account's
//! mailbox hierarchy, before it served anything, so every `mp` command, the
//! TUI and the desktop app waited that long after each start. What this file
//! pins:
//!
//! - the daemon answers `daemon.status`, `diagnostic.health` and the handshake
//!   while an account's store open is held, reporting that account `opening`
//!   and the other one `ready`;
//! - a store method on the held account is refused with `-32006`
//!   `account_not_ready` and `state: "opening"` instead of opening the store a
//!   second time (a second open would run a second integrity check, and here
//!   would block on the same hold until the call timed out);
//! - once the open is released the account turns `ready` and its hierarchy is
//!   the store's, a listed folder the configuration does not name included,
//!   announced to a bootstrapped client before the `ready` event.
//!
//! The slow open is the daemon's `MAILYPOPPINS_DAEMON_FAKE_OPEN_HOLD` hook
//! (`mailypoppins::store::FAKE_OPEN_HOLD_ENV`): the first open of an account's
//! store waits before its integrity check for as long as `<dir>/<account>.hold`
//! exists, after writing `<dir>/<account>.held`.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mailypoppins::catalogue::listing::{self, ListedFolder};
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::parse::FetchedEmail;
use mailypoppins::store::{BlobStore, Store, FAKE_OPEN_HOLD_ENV};

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::ErrorCode;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait. Shorter than the hook's own 30 s limit, so
/// a daemon whose answer waits on the held open fails here rather than
/// passing once the hook gives up.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// The account whose store opens at once.
const SMALL: &str = "small";

/// The account whose store open the test holds.
const LARGE: &str = "large";

/// The folder `large`'s server listed and its configuration does not name.
const LISTED: &str = "Projects";

struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "hold"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Self { root };
        let config = format!(
            r#"
[[accounts]]
name = "{SMALL}"
default_from = "small@example.com"

[[accounts]]
name = "{LARGE}"
default_from = "large@example.com"

[accounts.imap]
host = "127.0.0.1"
port = 9
username = "large@example.com"
sync_interval_secs = 0
"#
        );
        fs::write(sandbox.config_dir().join("config.toml"), config).expect("write config.toml");
        for account in [SMALL, LARGE] {
            sandbox.ingest(account);
        }
        let store = Store::open(sandbox.account_dir(LARGE).join("store.sqlite3")).expect("store");
        listing::replace(
            &store,
            LARGE,
            &[
                ListedFolder::named("INBOX").with_attributes(&["\\Inbox"]),
                ListedFolder::named(LISTED),
                ListedFolder::named("Junk").with_attributes(&["\\Junk"]),
            ],
        )
        .expect("record the listing");
        sandbox
    }

    fn ingest(&self, account: &str) {
        let dir = self.account_dir(account);
        fs::create_dir_all(&dir).expect("account dir");
        let store = Store::open(dir.join("store.sqlite3")).expect("store");
        let email = FetchedEmail {
            from: "someone@example.com".to_string(),
            to: format!("{account}@example.com"),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: format!("Hello {account}"),
            date: "Fri, 1 May 2026 05:00:00 +0000".to_string(),
            body_text: "body\n".to_string(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(format!("<{account}@example.com>")),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        ingest_message(
            &store,
            &BlobStore::new(dir.join("blobs")),
            &IngestInput {
                account,
                mailbox: "inbox",
                uid: 1,
                email: &email,
                raw: None,
            },
        )
        .expect("ingest");
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn hold_dir(&self) -> PathBuf {
        self.root.path().join("hold")
    }

    fn account_dir(&self, account: &str) -> PathBuf {
        self.data_dir().join("accounts").join(account)
    }

    fn socket(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.sock")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Spawn `mp daemon run` with the open hold armed, killed on drop. It
    /// does not wait for the daemon: when it answers is what is under test.
    fn spawn_daemon(&self) -> Proc {
        let child = Command::new(MP)
            .args(["daemon", "run"])
            .env("HOME", self.root.path().join("home"))
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env("MAILYPOPPINS_DAEMON_AUTOSTART", "0")
            .env(FAKE_OPEN_HOLD_ENV, self.hold_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        Proc(Some(child))
    }
}

/// A spawned `mp daemon run`, killed and reaped on drop.
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

/// One `daemon.status` round trip on a fresh connection, `None` when nothing
/// answered within `budget`.
async fn status(socket: &Path, budget: Duration) -> Option<Value> {
    let mut conn = tokio::time::timeout(budget, Connection::connect(socket))
        .await
        .ok()?
        .ok()?;
    tokio::time::timeout(budget, conn.call("daemon.status", json!({})))
        .await
        .ok()?
        .ok()
}

/// Poll `daemon.status` until `accept` takes the answer, failing with `what`
/// after [`DEADLINE`].
async fn status_until(socket: &Path, what: &str, accept: impl Fn(&Value) -> bool) -> Value {
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < DEADLINE {
        if let Some(answer) = status(socket, TICK.max(Duration::from_millis(500))).await {
            if accept(&answer) {
                return answer;
            }
            last = Some(answer);
        }
        tokio::time::sleep(TICK).await;
    }
    panic!("{what} within {DEADLINE:?}; the last daemon.status was {last:?}");
}

/// The state `daemon.status` (or a health report's `runtime`) gives `name`.
fn state_of<'a>(accounts: &'a Value, name: &str, key: &str) -> Option<&'a str> {
    accounts
        .as_array()?
        .iter()
        .find(|a| a["name"] == name)?
        .get(key)?
        .as_str()
}

async fn connected(sandbox: &Sandbox) -> Connection {
    let mut conn = within("connect", Connection::connect(&sandbox.socket()))
        .await
        .expect("connect");
    within(
        "initialize",
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
    .expect("the handshake is answered while a store is still opening");
    conn
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Result<Value, ClientError> {
    within(method, conn.call(method, params)).await
}

/// The rows of `account` in a bootstrap snapshot.
fn snapshot_rows<'a>(bootstrap: &'a Value, account: &str) -> &'a Vec<Value> {
    bootstrap["snapshot"]["mailboxes"][account]
        .as_array()
        .unwrap_or_else(|| panic!("the snapshot carries {account}'s mailboxes: {bootstrap}"))
}

fn lists(rows: &[Value], server: &str) -> bool {
    rows.iter().any(|row| row["server"] == server)
}

#[tokio::test]
async fn a_held_store_open_holds_up_its_account_and_nothing_else() {
    let sandbox = Sandbox::new();
    let gate = sandbox.hold_dir().join(format!("{LARGE}.hold"));
    fs::write(&gate, b"").expect("arm the hold");
    let _daemon = sandbox.spawn_daemon();
    let socket = sandbox.socket();

    // The daemon answers while the large store's open is held, and the small
    // account comes up beside it.
    let answer = status_until(
        &socket,
        "daemon.status answered with small ready",
        |answer| state_of(&answer["accounts"], SMALL, "state") == Some("ready"),
    )
    .await;
    assert_eq!(
        state_of(&answer["accounts"], LARGE, "state"),
        Some("opening"),
        "the held account is opening: {answer}"
    );
    assert!(
        sandbox.hold_dir().join(format!("{LARGE}.held")).exists(),
        "the large store's open reached the hold"
    );

    let mut conn = connected(&sandbox).await;
    let health = call(&mut conn, "diagnostic.health", json!({}))
        .await
        .expect("diagnostic.health answers while a store is opening");
    assert_eq!(
        state_of(&health["accounts"], LARGE, "runtime"),
        Some("opening"),
        "{health}"
    );
    assert_eq!(
        state_of(&health["accounts"], SMALL, "runtime"),
        Some("ready"),
        "{health}"
    );

    // The held account's hierarchy is its configuration's until its store is
    // open: the listed folder is not in it yet.
    let bootstrap = call(&mut conn, "state.bootstrap", json!({}))
        .await
        .expect("state.bootstrap answers while a store is opening");
    assert!(
        !lists(snapshot_rows(&bootstrap, LARGE), LISTED),
        "a hierarchy read from the store the daemon has not opened: {bootstrap}"
    );

    // A store method on the held account is refused, and the small account is
    // served.
    let mut other = connected(&sandbox).await;
    let refused = call(&mut other, "mailbox.list", json!({"account": LARGE})).await;
    let Err(ClientError::Rpc(error)) = refused else {
        panic!("mailbox.list on the opening account must be refused, got {refused:?}");
    };
    assert_eq!(error.code, ErrorCode::AccountNotReady.code(), "{error:?}");
    let data = error.data.clone().expect("the refusal carries data");
    assert_eq!(data["account"], LARGE, "{error:?}");
    assert_eq!(data["state"], "opening", "{error:?}");
    let listed = call(&mut other, "account.list", json!({}))
        .await
        .expect("account.list");
    assert_eq!(
        state_of(&listed["accounts"], LARGE, "state"),
        Some("opening"),
        "account.list and the refusal agree: {listed}"
    );
    call(&mut other, "mailbox.list", json!({"account": SMALL}))
        .await
        .expect("the small account is served while the large one opens");

    // Release the open: the account turns ready with the store's hierarchy,
    // and the bootstrapped client hears of the listed folder first.
    fs::remove_file(&gate).expect("release the hold");
    let mut announced = Vec::new();
    loop {
        let notification = within("the ready event", conn.next_notification())
            .await
            .expect("the daemon keeps the connection open");
        let params = &notification.params;
        if params["kind"] == "account.state_changed" && params["payload"]["account"] == LARGE {
            assert_eq!(params["payload"]["state"], "ready", "{params}");
            break;
        }
        // A count change travels as an invalidation of the mailbox resource.
        if let Some(resource) = params["payload"]["resource"].as_str() {
            announced.push(resource.to_string());
        }
    }

    let mailboxes = call(&mut other, "mailbox.list", json!({"account": LARGE}))
        .await
        .expect("mailbox.list answers once the store is open");
    let rows = mailboxes["mailboxes"].as_array().expect("mailbox rows");
    let projects = rows
        .iter()
        .find(|row| row["server"] == LISTED)
        .unwrap_or_else(|| panic!("the listed folder is a mailbox: {mailboxes}"));
    assert!(rows.iter().any(|row| row["role"] == "inbox"), "{mailboxes}");
    let resource = format!(
        "mailbox:{LARGE}/{}",
        projects["slug"].as_str().expect("a slug")
    );
    assert!(
        announced.contains(&resource),
        "the reseed announced {resource} before the ready event, got {announced:?}"
    );

    let fresh = call(&mut other, "state.bootstrap", json!({}))
        .await
        .expect("state.bootstrap");
    assert!(lists(snapshot_rows(&fresh, LARGE), LISTED), "{fresh}");
    let answer = status(&socket, DEADLINE).await.expect("daemon.status");
    assert_eq!(
        state_of(&answer["accounts"], LARGE, "state"),
        Some("ready"),
        "{answer}"
    );
}

/// A daemon serving [`Sandbox`] with `large`'s store open held, once it
/// reports `small` ready and `large` opening. The hold is released by
/// removing the returned gate.
async fn held() -> (Sandbox, Proc, PathBuf) {
    let sandbox = Sandbox::new();
    let gate = sandbox.hold_dir().join(format!("{LARGE}.hold"));
    fs::write(&gate, b"").expect("arm the hold");
    let daemon = sandbox.spawn_daemon();
    let answer = status_until(
        &sandbox.socket(),
        "daemon.status answered with small ready",
        |answer| state_of(&answer["accounts"], SMALL, "state") == Some("ready"),
    )
    .await;
    assert_eq!(
        state_of(&answer["accounts"], LARGE, "state"),
        Some("opening"),
        "{answer}"
    );
    (sandbox, daemon, gate)
}

/// Assert `answer` is the `-32006` `opening` refusal of `large`.
fn assert_opening(method: &str, answer: Result<Value, ClientError>) {
    let Err(ClientError::Rpc(error)) = answer else {
        panic!("{method} on the opening account must be refused, got {answer:?}");
    };
    assert_eq!(
        error.code,
        ErrorCode::AccountNotReady.code(),
        "{method}: {error:?}"
    );
    let data = error.data.clone().expect("the refusal carries data");
    assert_eq!(data["account"], LARGE, "{method}: {error:?}");
    assert_eq!(data["state"], "opening", "{method}: {error:?}");
}

/// Wait for `large`'s `account.state_changed` to `ready` on `conn`.
async fn until_ready(conn: &mut Connection) {
    loop {
        let notification = within("the ready event", conn.next_notification())
            .await
            .expect("the daemon keeps the connection open");
        let params = &notification.params;
        if params["kind"] == "account.state_changed" && params["payload"]["account"] == LARGE {
            assert_eq!(params["payload"]["state"], "ready", "{params}");
            return;
        }
    }
}

/// `hook.list` and `contact.rebuild` refuse an account still opening its
/// store, as every other store method does, rather than opening the store a
/// second time on the request path (PERSO-109); the CLI waits on that
/// refusal. Once the account is ready both are served.
#[tokio::test]
async fn hook_list_and_contact_rebuild_refuse_an_opening_account() {
    let (sandbox, _daemon, gate) = held().await;
    let mut conn = connected(&sandbox).await;
    for method in ["hook.list", "contact.rebuild"] {
        let answer = call(&mut conn, method, json!({"account": LARGE})).await;
        assert_opening(method, answer);
    }
    call(&mut conn, "hook.list", json!({"account": SMALL}))
        .await
        .expect("the small account's hooks are listed while the large one opens");

    let mut events = connected(&sandbox).await;
    call(&mut events, "state.bootstrap", json!({}))
        .await
        .expect("state.bootstrap");
    fs::remove_file(&gate).expect("release the hold");
    until_ready(&mut events).await;
    call(&mut conn, "hook.list", json!({"account": LARGE}))
        .await
        .expect("hook.list answers once the store is open");
}
