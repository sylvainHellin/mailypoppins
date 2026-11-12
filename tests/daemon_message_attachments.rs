//! PERSO-222: `message.attachments`, the parts of one received message, each
//! carrying the `part` `message.materialise_attachment` takes.
//!
//! Before it, a client learned the part indexes by reading the `attachments`
//! of a `message.get` record, whose position is the index, and put them in
//! reading order itself; the CLI, the TUI and the desktop app each did both.
//! The method answers the list in reading order (the real attachments, then
//! the body's inline images, each in stored order) with the index on every
//! entry, for a stored row and for a server-search hit alike.
//!
//! The fixture is the read slice's, plus one message whose first part is an
//! inline image of its HTML body, so reading order and stored order differ.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::parse::AttachmentData;

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(20);
const METHOD: &str = "message.attachments";

/// The message whose logo is an inline image stored before its invoice.
const LOGO: &str = "logo@example.com";

/// The read fixture plus [`LOGO`], as `alpha inbox/6`.
fn seeded() -> (TempDir, DaemonFixture) {
    let tmp = TempDir::new().expect("a temporary root");
    fixture::seed(tmp.path());
    let mut logo = fixture::email(
        "billing@example.com",
        "sylvain@example.com",
        "Rechnung",
        "Fri, 3 Jul 2026 10:00:00 +0200",
        "die Rechnung ist beigefügt\n",
    );
    logo.message_id = Some(format!("<{LOGO}>"));
    logo.html_body = Some("<p><img src=\"cid:image001.png@01DA\"></p>".to_string());
    logo.has_attachments = true;
    logo.attachments = vec![
        AttachmentData {
            filename: "image001.png".to_string(),
            content: b"\x89PNG\r\n\x1a\nlogo".to_vec(),
            content_id: Some("image001.png@01DA".to_string()),
        },
        AttachmentData {
            filename: "Rechnung.pdf".to_string(),
            content: b"%PDF-1.7".to_vec(),
            content_id: None,
        },
    ];
    fixture::ingest(tmp.path(), fixture::ACCOUNT, "inbox", 6, &logo);
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

async fn call_method(
    conn: &mut Connection,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    match within(method, conn.call(method, params)).await {
        Ok(value) => Ok(value),
        Err(ClientError::Rpc(error)) => Err(error),
        Err(other) => panic!("{method}: expected a result or a typed RPC error, got {other:?}"),
    }
}

async fn call(conn: &mut Connection, params: Value) -> Result<Value, RpcError> {
    call_method(conn, METHOD, params).await
}

fn selector(mailbox: &str, message_id: &str) -> String {
    format!("mp://{}/{mailbox}/{message_id}", fixture::ACCOUNT)
}

#[tokio::test]
async fn the_daemon_advertises_the_listing() {
    let (tmp, _daemon) = seeded();
    let (_conn, capabilities) = connect(tmp.path()).await;
    assert!(
        capabilities.iter().any(|c| c == METHOD),
        "{METHOD} is advertised, got {capabilities:?}"
    );
}

/// The four row addresses answer one listing: the account, the canonical
/// selector and every part with its index, its name, its size and whether it
/// is an inline image.
#[tokio::test]
async fn a_row_lists_its_parts_with_their_indexes() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let expected = json!({
        "account": fixture::ACCOUNT,
        "selector": selector("inbox", fixture::BERICHT),
        "attachments": [
            {"part": 0, "name": "notes.pdf", "size": 14, "inline": false},
            {"part": 1, "name": "agenda.txt", "size": 3, "inline": false},
        ],
    });
    for params in [
        json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
        json!({"account": fixture::ACCOUNT, "row_id": 1}),
        json!({"account": fixture::ACCOUNT, "selector": fixture::BERICHT}),
        json!({"account": fixture::ACCOUNT, "selector": fixture::BERICHT, "mailbox": "inbox"}),
    ] {
        let listing = call(&mut conn, params.clone())
            .await
            .unwrap_or_else(|e| panic!("{params} is listed, got {e:?}"));
        assert_eq!(listing, expected, "{params}");
    }
}

/// Reading order, not stored order: the invoice before the logo, each still
/// carrying the index that materialises it.
#[tokio::test]
async fn inline_images_follow_the_real_attachments_and_keep_their_part() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let listing = call(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "id": "inbox/6"}),
    )
    .await
    .expect("the logo message is listed");
    assert_eq!(
        listing["attachments"],
        json!([
            {"part": 1, "name": "Rechnung.pdf", "size": 8, "inline": false},
            {"part": 0, "name": "image001.png", "size": 12, "inline": true},
        ])
    );

    for entry in listing["attachments"].as_array().expect("a list") {
        let handle = call_method(
            &mut conn,
            "message.materialise_attachment",
            json!({"account": fixture::ACCOUNT, "id": "inbox/6", "part": entry["part"]}),
        )
        .await
        .expect("each listed part materialises");
        assert_eq!(handle["name"], entry["name"]);
        assert_eq!(handle["bytes"], entry["size"]);
        let path = handle["path"].as_str().expect("path");
        assert_eq!(fs::metadata(path).expect("the file").len(), entry["size"]);
    }
}

/// A message with no attachment is an empty listing, not a refusal: whether
/// to say "no attachments" is the client's.
#[tokio::test]
async fn a_message_without_attachments_lists_none() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let listing = call(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "id": "inbox/2"}),
    )
    .await
    .expect("a message without attachments is listed");
    assert_eq!(
        listing["selector"],
        json!(selector("inbox", fixture::KICKOFF))
    );
    assert_eq!(listing["attachments"], json!([]));
}

/// A server-search hit is addressed as `message.fetch` addresses it: one the
/// store has caught up with is its row's listing, and one it has not goes to
/// the server, which the test has none of, so that refusal is the server
/// leg's. A hit beside a row address is the caller disagreeing with itself.
#[tokio::test]
async fn a_server_search_hit_is_listed_through_its_message_id() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let held = call(
        &mut conn,
        json!({
            "account": fixture::ACCOUNT,
            "hit": {"message_id": format!("<{LOGO}>"), "mailbox": "inbox"},
        }),
    )
    .await
    .expect("a hit the store holds is its row's listing");
    let row = call(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "id": "inbox/6"}),
    )
    .await
    .expect("the row's listing");
    assert_eq!(held, row);

    let error = call(
        &mut conn,
        json!({
            "account": fixture::ACCOUNT,
            "hit": {"message_id": "<only-on-the-server@example.com>", "mailbox": "inbox"},
        }),
    )
    .await
    .expect_err("there is no server to read the hit from");
    assert_eq!(error.code, -32603, "a server leg failure, got {error:?}");
    assert_eq!(error.data, Some(json!({"account": fixture::ACCOUNT})));

    let error = call(
        &mut conn,
        json!({
            "account": fixture::ACCOUNT,
            "id": "inbox/6",
            "hit": {"message_id": format!("<{LOGO}>"), "mailbox": "inbox"},
        }),
    )
    .await
    .expect_err("a hit beside a row address is refused");
    assert_eq!(error.code, -32602, "{error:?}");
}

/// An address that names nothing, or more than one thing, is the caller's
/// mistake; an unknown account and one with no store are the two refusals
/// every read makes.
#[tokio::test]
async fn the_refusals_are_the_read_family_s() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    for params in [
        json!({"account": fixture::ACCOUNT}),
        json!({"account": fixture::ACCOUNT, "id": "inbox/9999"}),
        json!({"account": fixture::ACCOUNT, "row_id": 1, "id": "inbox/1"}),
        json!({"account": fixture::ACCOUNT, "selector": "nope@example.com"}),
    ] {
        let error = call(&mut conn, params.clone())
            .await
            .expect_err("a bad address is refused");
        assert_eq!(error.code, -32602, "{params}: got {error:?}");
    }

    let unknown = call(
        &mut conn,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "row_id": 1}),
    )
    .await
    .expect_err("an unknown account is refused");
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(
        unknown.data,
        Some(json!({"account": fixture::UNKNOWN_ACCOUNT}))
    );

    let storeless = call(
        &mut conn,
        json!({"account": fixture::STORELESS_ACCOUNT, "row_id": 1}),
    )
    .await
    .expect_err("an account with no store is refused");
    assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
}
