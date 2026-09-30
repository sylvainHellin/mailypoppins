//! The `hook.*` family over a real socket, and `mp hooks` in front of it
//! (#0135).
//!
//! The scan, the cursor and the sender check are unit-tested beside their
//! code (`src/daemon/hooks/`); this file pins what a client sees: the listing,
//! the dry run of one hook against one stored message, and a replay that runs
//! the command in the daemon and refuses a message the `match` table rejects.
//! The two stored messages differ only in the verdict Gmail's own
//! `Authentication-Results` records, so the sender gate is what decides.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};

use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{BlobStore, Store};

use support::parity::{mp_command, socket_path, DaemonFixture};

const DEADLINE: Duration = Duration::from_secs(20);
const ACCOUNT: &str = "alpha";
const GENUINE: &str = "genuine@hellin.me";
const SPOOFED: &str = "spoofed@hellin.me";

/// A seeded root and a daemon serving it. The field order is the drop order.
struct Slice {
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

fn config(sink: &Path) -> String {
    format!(
        r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"

[[accounts.hooks]]
name = "pi"
exec = ["/bin/sh", "-c", "cat > {sink}; echo ran"]
match = {{ authenticated_from = ["sylvain@hellin.me"], authserv_id = "mx.google.com", subject = "^Task" }}
"#,
        sink = sink.display()
    )
}

fn raw(id: &str, genuine: bool) -> Vec<u8> {
    let verdict = if genuine {
        "dkim=pass header.i=@hellin.me header.s=protonmail3; spf=pass smtp.mailfrom=sylvain@hellin.me"
    } else {
        "dkim=none; spf=fail smtp.mailfrom=x@evil.example"
    };
    format!(
        "Delivered-To: alpha@example.com\r\nAuthentication-Results: mx.google.com; {verdict}\r\nFrom: Sylvain <sylvain@hellin.me>\r\nTo: alpha@example.com\r\nSubject: Task {id}\r\nMessage-ID: <{id}>\r\nDate: Wed, 30 Sep 2026 10:00:00 +0200\r\n\r\nreply with PONG\r\n"
    )
    .into_bytes()
}

fn seed(root: &Path, sink: &Path) {
    std::fs::write(root.join("config.toml"), config(sink)).unwrap();
    let dir = root.join("accounts").join(ACCOUNT);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(dir.join("store.sqlite3")).unwrap();
    let blobs = BlobStore::new(dir.join("blobs"));
    for (uid, id, genuine) in [(1, GENUINE, true), (2, SPOOFED, false)] {
        let bytes = raw(id, genuine);
        let email = mailypoppins::parse::parse_rfc822_to_fetched_email(&bytes).unwrap();
        ingest_message(
            &store,
            &blobs,
            &IngestInput {
                account: ACCOUNT,
                mailbox: "inbox",
                uid,
                email: &email,
                raw: Some(&bytes),
            },
        )
        .unwrap();
    }
}

impl Slice {
    fn start() -> (Slice, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let sink = tmp.path().join("stdin.json");
        seed(tmp.path(), &sink);
        let daemon = DaemonFixture::start(tmp.path());
        (Slice { daemon, tmp }, sink)
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    async fn connect(&self) -> Connection {
        let mut conn =
            tokio::time::timeout(DEADLINE, Connection::connect(&socket_path(self.root())))
                .await
                .unwrap()
                .unwrap();
        tokio::time::timeout(
            DEADLINE,
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.root().to_path_buf(),
                    config_dir: self.root().to_path_buf(),
                },
                &[],
                &[],
            ),
        )
        .await
        .unwrap()
        .unwrap();
        conn
    }
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Result<Value, ClientError> {
    tokio::time::timeout(DEADLINE, conn.call(method, params))
        .await
        .unwrap_or_else(|_| panic!("{method} went unanswered"))
}

async fn settle(conn: &mut Connection, started: &Value) -> Value {
    let id = started["operation_id"].as_str().expect("an operation id");
    let start = Instant::now();
    loop {
        let status = call(conn, "operation.status", json!({"operation_id": id}))
            .await
            .unwrap();
        if matches!(
            status["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            return status;
        }
        assert!(start.elapsed() < DEADLINE, "{id} did not settle: {status}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn selector(id: &str) -> String {
    format!("mp://{ACCOUNT}/inbox/{id}")
}

#[tokio::test]
async fn the_listing_names_each_hook_with_its_match_table_and_no_cursor_yet() {
    let (slice, _) = Slice::start();
    let mut conn = slice.connect().await;
    let listed = call(&mut conn, "hook.list", json!({"account": ACCOUNT}))
        .await
        .unwrap();
    let hook = &listed["hooks"][0];
    assert_eq!(hook["name"], "pi");
    assert_eq!(hook["mailbox"], "inbox");
    assert_eq!(hook["mailbox_key"], "inbox");
    assert_eq!(hook["timeout_secs"], 60);
    assert_eq!(hook["match"]["authserv_id"], "mx.google.com");
    assert!(
        hook["cursor"].is_null(),
        "no runtime ran, so nothing armed: {hook}"
    );
}

#[tokio::test]
async fn a_dry_run_reports_each_criterion_and_runs_nothing() {
    let (slice, sink) = Slice::start();
    let mut conn = slice.connect().await;
    let genuine = call(
        &mut conn,
        "hook.test",
        json!({"account": ACCOUNT, "hook": "pi", "selector": selector(GENUINE)}),
    )
    .await
    .unwrap();
    assert_eq!(genuine["matched"], true, "{genuine}");
    assert_eq!(genuine["checks"].as_array().unwrap().len(), 2);
    assert_eq!(
        genuine["payload"]["authenticated_sender"],
        "sylvain@hellin.me"
    );
    assert!(
        genuine["payload"]["dir"].is_null(),
        "a dry run materialises nothing"
    );

    let spoofed = call(
        &mut conn,
        "hook.test",
        json!({"account": ACCOUNT, "hook": "pi", "selector": selector(SPOOFED)}),
    )
    .await
    .unwrap();
    assert_eq!(spoofed["matched"], false);
    assert_eq!(spoofed["checks"][0]["criterion"], "authenticated_from");
    assert_eq!(spoofed["checks"][0]["passed"], false);
    assert!(!sink.exists(), "nothing ran");
}

#[tokio::test]
async fn an_unknown_hook_or_message_is_the_callers_mistake() {
    let (slice, _) = Slice::start();
    let mut conn = slice.connect().await;
    for params in [
        json!({"account": ACCOUNT, "hook": "nope", "selector": selector(GENUINE)}),
        json!({"account": ACCOUNT, "hook": "pi", "selector": selector("absent@x")}),
        json!({"account": ACCOUNT, "hook": "pi", "selector": selector(GENUINE), "extra": 1}),
    ] {
        match call(&mut conn, "hook.test", params.clone()).await {
            Err(ClientError::Rpc(error)) => assert_eq!(error.code, -32602, "{params}: {error:?}"),
            other => panic!("{params}: expected -32602, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_replay_runs_the_command_in_the_daemon_behind_the_same_gate() {
    let (slice, sink) = Slice::start();
    let mut conn = slice.connect().await;
    let started = call(
        &mut conn,
        "hook.replay",
        json!({"account": ACCOUNT, "hook": "pi", "selector": selector(GENUINE)}),
    )
    .await
    .unwrap();
    let status = settle(&mut conn, &started).await;
    assert_eq!(status["state"], "succeeded", "{status}");
    assert_eq!(status["result"]["outcome"], "exit 0");
    assert_eq!(status["result"]["stdout"], "ran");
    let payload: Value = serde_json::from_slice(&std::fs::read(&sink).unwrap()).unwrap();
    assert_eq!(payload["message_id"], format!("<{GENUINE}>"));
    assert_eq!(payload["replay"], true);

    std::fs::remove_file(&sink).unwrap();
    let started = call(
        &mut conn,
        "hook.replay",
        json!({"account": ACCOUNT, "hook": "pi", "selector": selector(SPOOFED)}),
    )
    .await
    .unwrap();
    let status = settle(&mut conn, &started).await;
    assert_eq!(status["state"], "failed", "{status}");
    assert!(!sink.exists(), "a spoofed sender never reaches the command");

    let listed = call(&mut conn, "hook.list", json!({"account": ACCOUNT}))
        .await
        .unwrap();
    // A replay records no cursor of its own; `last_run` rides on one, so the
    // listing still shows none until a runtime arms the hook.
    assert!(listed["hooks"][0]["cursor"].is_null());
}

#[test]
fn mp_hooks_test_prints_the_verdict_and_finds_the_account_by_hook_name() {
    let (slice, _) = Slice::start();
    let out = mp_command(slice.root())
        .args(["hooks", "test", "pi", &selector(SPOOFED)])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("authenticated_from"), "{stdout}");
    assert!(
        stdout.contains("pi does not match this message"),
        "{stdout}"
    );

    let listed = mp_command(slice.root())
        .args(["hooks", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.status.success());
    assert!(stdout.contains("pi  inbox  /bin/sh -c"), "{stdout}");
    assert!(stdout.contains("not armed yet"), "{stdout}");
}
