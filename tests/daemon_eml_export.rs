//! PERSO-174: a stored message as an `.eml` file, through the daemon
//! (`message.materialise_eml`) and through the CLI (`mp export`).
//!
//! The fixture is the read slice's, plus one message ingested with its raw
//! RFC 822 bytes, which is what an IMAP sync stores. The read fixture's own
//! rows carry no raw message, as a Graph row does not; for those the daemon
//! goes to the server, which a test has none of, so the assertion there is
//! only that the miss is a server refusal and not a file of something else.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{BlobStore, Store};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(20);
const METHOD: &str = "message.materialise_eml";

/// The message stored with its raw bytes, CRLF line ends and all, so a test
/// can tell the original from anything rebuilt out of the parsed row.
const RAW: &[u8] = b"From: Bibliothek <library@example.com>\r\n\
To: Sylvain Hellin <sylvain@example.com>\r\n\
Subject: Re: Elsevier DEAL open access\r\n\
Date: Wed, 21 Oct 2026 09:12:00 +0200\r\n\
Message-ID: <deal@example.com>\r\n\
X-Original-Only: kept verbatim\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
> quoted thread\r\n\
the answer\r\n";

const EXPECTED_NAME: &str = "2026-10-21_Re Elsevier DEAL open access.eml";

/// The read fixture plus the raw-backed message, as `alpha inbox/5`.
fn seeded() -> (TempDir, DaemonFixture) {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());
    let dir = fixture::account_dir(tmp.path(), fixture::ACCOUNT);
    let store = Store::open(dir.join("store.sqlite3")).expect("open the fixture store");
    let blobs = BlobStore::new(dir.join("blobs"));
    let email = mailypoppins::parse::parse_rfc822_to_fetched_email(RAW).expect("the raw parses");
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account: fixture::ACCOUNT,
            mailbox: "inbox",
            uid: 5,
            email: &email,
            raw: Some(RAW),
        },
    )
    .expect("ingest the raw-backed message");
    drop(store);
    let daemon = DaemonFixture::start(tmp.path());
    (tmp, daemon)
}

async fn within<F, T>(what: &str, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not answer within {DEADLINE:?}"),
    }
}

async fn connect(root: &Path) -> (Connection, Vec<String>) {
    let mut conn = within("connect", Connection::connect(&socket_path(root)))
        .await
        .expect("connecting to a live daemon socket succeeds");
    let result = within(
        "initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: root.to_path_buf(),
                config_dir: root.to_path_buf(),
            },
            &[],
            &[],
        ),
    )
    .await
    .expect("a compatible handshake succeeds");
    (conn, result.capabilities)
}

async fn call(conn: &mut Connection, params: Value) -> Result<Value, RpcError> {
    match within(METHOD, conn.call(METHOD, params)).await {
        Ok(value) => Ok(value),
        Err(ClientError::Rpc(error)) => Err(error),
        Err(other) => panic!("{METHOD}: expected a result or a typed RPC error, got {other:?}"),
    }
}

#[tokio::test]
async fn the_daemon_advertises_the_eml_export() {
    let (tmp, _daemon) = seeded();
    let (_conn, capabilities) = connect(tmp.path()).await;
    assert!(
        capabilities.iter().any(|c| c == METHOD),
        "{METHOD} is advertised, got {capabilities:?}"
    );
}

/// The file is the stored raw message byte for byte, named after the day and
/// the subject, in the handle family's directory, and released by its release.
#[tokio::test]
async fn a_stored_raw_message_is_exported_verbatim() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;

    for params in [
        json!({"account": fixture::ACCOUNT, "id": "inbox/5"}),
        json!({
            "account": fixture::ACCOUNT,
            "selector": format!("mp://{}/inbox/deal@example.com", fixture::ACCOUNT),
        }),
    ] {
        let result = call(&mut conn, params.clone())
            .await
            .unwrap_or_else(|e| panic!("{params} is exported, got {e:?}"));
        let mut keys: Vec<&str> = result
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["bytes", "expires_at", "handle", "name", "path"]);
        assert_eq!(result["name"], json!(EXPECTED_NAME));
        let path = Path::new(result["path"].as_str().expect("path"));
        assert!(path.starts_with(tmp.path().join("runtime").join("handles")));
        assert_eq!(fs::read(path).expect("the client can read the file"), RAW);
        assert_eq!(result["bytes"], json!(RAW.len()));

        let released = within(
            "release",
            conn.call(
                "message.release_handle",
                json!({"handle": result["handle"]}),
            ),
        )
        .await
        .expect("the release succeeds");
        assert_eq!(released, json!({}));
        assert!(!path.exists(), "the release removes the file");
    }
}

/// A row with no stored raw message and an account the test gives no server
/// is a server refusal naming the account, never a rebuilt message.
#[tokio::test]
async fn a_row_without_a_raw_message_goes_to_the_server() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let error = call(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
    )
    .await
    .expect_err("there is no server to fetch the raw message from");
    assert_eq!(error.code, -32603, "a server leg failure, got {error:?}");
    assert_eq!(error.data, Some(json!({"account": fixture::ACCOUNT})));
}

#[tokio::test]
async fn a_bad_address_is_invalid_params() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    for params in [
        json!({"account": fixture::ACCOUNT}),
        json!({"account": fixture::ACCOUNT, "id": "inbox/9999"}),
        json!({"account": fixture::ACCOUNT, "row_id": 1, "id": "inbox/5"}),
    ] {
        let error = call(&mut conn, params.clone())
            .await
            .expect_err("a bad address is refused");
        assert_eq!(error.code, -32602, "{params}: got {error:?}");
    }
}

/// `mp export` writes the file into the directory it is given, and prints the
/// path it wrote into a pipe.
#[test]
fn mp_export_writes_the_eml_into_the_output_directory() {
    let (_tmp, daemon) = seeded();
    let out_dir = TempDir::new().expect("an output directory");
    let target = out_dir.path().join("exports");
    let out = daemon.mp(&[
        "export",
        "mp://alpha/inbox/deal@example.com",
        "-o",
        target.to_str().expect("a UTF-8 path"),
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = target.join(EXPECTED_NAME);
    assert_eq!(fs::read(&written).expect("the export is there"), RAW);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        written.display().to_string()
    );
}
