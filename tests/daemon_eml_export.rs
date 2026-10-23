//! PERSO-174: a stored message as an `.eml` file, through the daemon
//! (`message.materialise_eml`) and through the CLI (`mp export`), and as the
//! one attachment of a forward (`draft.forward` with `as_attachment`,
//! `mp forward --as-attachment`).
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

/// A server-search hit is addressed as `message.fetch` addresses it. One the
/// store has caught up with is that row's export; one it has not goes to the
/// server, which the test has none of, so the refusal is the server leg's
/// and not an address the daemon does not know (PERSO-174).
#[tokio::test]
async fn a_server_search_hit_is_exported_through_its_message_id() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let result = call(
        &mut conn,
        json!({
            "account": fixture::ACCOUNT,
            "hit": {"message_id": "<deal@example.com>", "mailbox": "inbox"},
        }),
    )
    .await
    .expect("a hit the store holds is exported");
    assert_eq!(result["name"], json!(EXPECTED_NAME));
    let path = Path::new(result["path"].as_str().expect("path"));
    assert_eq!(fs::read(path).expect("the exported file"), RAW);

    let error = call(
        &mut conn,
        json!({
            "account": fixture::ACCOUNT,
            "hit": {"message_id": "<only-on-the-server@example.com>", "mailbox": "inbox"},
        }),
    )
    .await
    .expect_err("there is no server to fetch the hit from");
    assert_eq!(error.code, -32603, "a server leg failure, got {error:?}");
    assert_eq!(error.data, Some(json!({"account": fixture::ACCOUNT})));

    for params in [
        json!({"account": fixture::ACCOUNT, "hit": {"message_id": "<x@y>"}}),
        json!({"account": fixture::ACCOUNT, "hit": {"message_id": " ", "mailbox": "inbox"}}),
        json!({
            "account": fixture::ACCOUNT,
            "id": "inbox/5",
            "hit": {"message_id": "<deal@example.com>", "mailbox": "inbox"},
        }),
    ] {
        let error = call(&mut conn, params.clone())
            .await
            .expect_err("a bad hit is refused");
        assert_eq!(error.code, -32602, "{params}: got {error:?}");
    }
}

/// `message.get`, `message.materialise_markdown` and
/// `message.materialise_attachment` take the same hit address, so every
/// action on a server-only hit reads it through the daemon without a fetch
/// first: a hit the store has caught up with is answered by its row, one it
/// has not by the server's copy, which the test has no server for, so that
/// refusal is the server leg's (PERSO-155).
#[tokio::test]
async fn a_server_search_hit_is_read_and_materialised_through_its_message_id() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let held = json!({"message_id": "<deal@example.com>", "mailbox": "inbox"});
    let missing = json!({"message_id": "<only-on-the-server@example.com>", "mailbox": "inbox"});
    let call = async |conn: &mut Connection, method: &str, params: Value| match within(
        method,
        conn.call(method, params),
    )
    .await
    {
        Ok(value) => Ok(value),
        Err(ClientError::Rpc(error)) => Err(error),
        Err(other) => panic!("{method}: expected a result or a typed RPC error, got {other:?}"),
    };

    let shown = call(
        &mut conn,
        "message.get",
        json!({"account": fixture::ACCOUNT, "hit": held, "body": false}),
    )
    .await
    .expect("a hit the store holds is read as its row");
    assert_eq!(
        shown["selector"],
        json!(format!("mp://{}/inbox/deal@example.com", fixture::ACCOUNT))
    );
    assert!(shown.get("body").is_none(), "body: false is honoured");

    let view = call(
        &mut conn,
        "message.materialise_markdown",
        json!({"account": fixture::ACCOUNT, "hit": held}),
    )
    .await
    .expect("a held hit's view");
    let text = fs::read_to_string(view["path"].as_str().expect("path")).expect("the view");
    assert!(text.contains("Elsevier DEAL"), "{text}");

    let error = call(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": fixture::ACCOUNT, "hit": held, "part": 0}),
    )
    .await
    .expect_err("the held message carries no attachment");
    assert_eq!(error.code, -32602, "{error:?}");

    for (method, extra) in [
        ("message.get", json!({})),
        ("message.materialise_markdown", json!({})),
        ("message.materialise_attachment", json!({"part": 0})),
    ] {
        let mut params = json!({"account": fixture::ACCOUNT, "hit": missing});
        for (key, value) in extra.as_object().expect("an object") {
            params[key] = value.clone();
        }
        let error = call(&mut conn, method, params)
            .await
            .expect_err("there is no server to read the hit from");
        assert_eq!(
            error.code, -32603,
            "{method}: a server leg failure, got {error:?}"
        );
        assert_eq!(error.data, Some(json!({"account": fixture::ACCOUNT})));

        let error = call(
            &mut conn,
            method,
            json!({"account": fixture::ACCOUNT, "id": "inbox/5", "hit": held, "part": 0}),
        )
        .await
        .expect_err("a hit beside a row address is refused");
        assert_eq!(error.code, -32602, "{method}: {error:?}");
    }
}

/// The `attachments:` entries of a draft file, unquoted.
///
/// Line ends are normalised first: a draft built from a CRLF message is
/// written with CRLF throughout.
fn attachments_of(draft: &str) -> Vec<String> {
    let draft = draft.replace("\r\n", "\n");
    let frontmatter = draft
        .strip_prefix("---\n")
        .and_then(|rest| rest.split("\n---\n").next())
        .expect("a draft opens with frontmatter");
    let mut entries = Vec::new();
    let mut inside = false;
    for line in frontmatter.lines() {
        if line == "attachments:" {
            inside = true;
        } else if inside {
            match line.strip_prefix("  - ") {
                Some(entry) => entries.push(entry.trim_matches('"').to_string()),
                None => break,
            }
        }
    }
    entries
}

/// A forward with `as_attachment` is the usual forward draft carrying one
/// part, the source's own `.eml`, byte for byte, over a body that quotes
/// nothing: Outlook's forward as attachment, the signature alone.
#[tokio::test]
async fn a_forward_as_attachment_carries_the_eml() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let created = within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"selector": "mp://alpha/inbox/deal@example.com"},
                "as_attachment": true,
            }),
        ),
    )
    .await
    .expect("the forward is written");
    assert_eq!(
        created["subject"],
        json!("FW: Re: Elsevier DEAL open access")
    );
    let path = Path::new(created["path"].as_str().expect("path")).to_path_buf();
    let draft = fs::read_to_string(&path).expect("the draft");
    assert!(
        draft.contains("forwarded_from: \"<deal@example.com>\""),
        "the send still flags the source as forwarded: {draft}"
    );
    let lf = draft.replace("\r\n", "\n");
    let body = lf
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .expect("a draft opens with frontmatter")
        .1;
    assert!(
        !body.contains(mailypoppins::draft::QUOTE_MARKER) && !body.contains("Sent:"),
        "the body quotes nothing: {draft}"
    );
    assert!(
        !path.with_extension("html").exists(),
        "no companion quote either"
    );
    let attachments = attachments_of(&draft);
    assert_eq!(attachments.len(), 1, "one part, the .eml: {draft}");
    let eml = Path::new(&attachments[0]);
    assert_eq!(
        eml.file_name().and_then(|n| n.to_str()),
        Some(EXPECTED_NAME)
    );
    assert!(
        eml.starts_with(fixture::account_dir(tmp.path(), fixture::ACCOUNT).join("attachments")),
        "the .eml lives where a forward's carried parts live, got {}",
        eml.display()
    );
    assert_eq!(fs::read(eml).expect("the attached .eml"), RAW);
}

/// A server-search hit forwards as attachment through the same `.eml`, with
/// the forward built from its bytes; one the server cannot be asked for
/// writes no draft (PERSO-174).
#[tokio::test]
async fn a_server_search_hit_forwards_as_attachment() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let created = within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"hit": {"message_id": "<deal@example.com>", "mailbox": "inbox"}},
                "as_attachment": true,
            }),
        ),
    )
    .await
    .expect("the forward of a hit is written");
    assert_eq!(
        created["subject"],
        json!("FW: Re: Elsevier DEAL open access")
    );
    let draft = fs::read_to_string(created["path"].as_str().expect("path")).expect("the draft");
    assert!(
        draft.contains("forwarded_from: \"<deal@example.com>\""),
        "{draft}"
    );
    let attachments = attachments_of(&draft);
    assert_eq!(attachments.len(), 1, "{draft}");
    assert_eq!(
        Path::new(&attachments[0])
            .file_name()
            .and_then(|n| n.to_str()),
        Some(EXPECTED_NAME)
    );
    assert_eq!(fs::read(&attachments[0]).expect("the attached .eml"), RAW);

    let drafts = fixture::account_dir(tmp.path(), fixture::ACCOUNT).join("drafts");
    let before = fs::read_dir(&drafts).map(|d| d.count()).unwrap_or(0);
    let refused = within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"hit": {"message_id": "<only-on-the-server@example.com>", "mailbox": "inbox"}},
                "as_attachment": true,
            }),
        ),
    )
    .await;
    match refused {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, -32603, "{error:?}"),
        other => panic!("a hit with no server is refused, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(&drafts).map(|d| d.count()).unwrap_or(0),
        before
    );
}

/// An inline forward takes a hit too, with the wizard's recipients and
/// subject, so a forward of a server-only hit asks for its recipients first
/// in every client as a stored row's does: a held hit is that row's forward,
/// and one only the server holds is refused when there is no server, writing
/// nothing (PERSO-155).
#[tokio::test]
async fn a_server_search_hit_forwards_inline_with_the_wizards_headers() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let headers = json!({"to": "bob@example.com", "cc": "", "bcc": "", "subject": "FYI"});
    let created = within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"hit": {"message_id": "<deal@example.com>", "mailbox": "inbox"}},
                "headers": headers,
            }),
        ),
    )
    .await
    .expect("the inline forward of a held hit is written");
    assert_eq!(created["subject"], json!("FYI"));
    let draft = fs::read_to_string(created["path"].as_str().expect("path")).expect("the draft");
    assert!(draft.contains("bob@example.com"), "{draft}");
    assert!(
        draft.contains("the answer"),
        "the forward quotes the body: {draft}"
    );
    assert!(
        attachments_of(&draft).is_empty(),
        "no .eml is attached: {draft}"
    );

    let drafts = fixture::account_dir(tmp.path(), fixture::ACCOUNT).join("drafts");
    let before = fs::read_dir(&drafts).map(|d| d.count()).unwrap_or(0);
    let refused = within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"hit": {"message_id": "<only-on-the-server@example.com>", "mailbox": "inbox"}},
                "headers": headers,
            }),
        ),
    )
    .await;
    match refused {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, -32603, "{error:?}"),
        other => panic!("a hit with no server is refused, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(&drafts).map(|d| d.count()).unwrap_or(0),
        before
    );

    let reply = within(
        "draft.reply",
        conn.call(
            "draft.reply",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"hit": {"message_id": "<deal@example.com>", "mailbox": "inbox"}},
            }),
        ),
    )
    .await;
    match reply {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, -32602, "{error:?}"),
        other => panic!("a reply to a hit source is refused, got {other:?}"),
    }
}

/// Without the flag a forward still carries the original's own parts, and
/// with it those parts are not carried twice: they are inside the `.eml`.
#[tokio::test]
async fn a_plain_forward_still_carries_the_original_parts() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let created = within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"selector": format!("mp://alpha/inbox/{}", fixture::BERICHT)},
                "as_attachment": false,
            }),
        ),
    )
    .await
    .expect("the forward is written");
    let draft = fs::read_to_string(created["path"].as_str().expect("path")).expect("the draft");
    let names: Vec<String> = attachments_of(&draft)
        .iter()
        .filter_map(|p| {
            Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect();
    assert_eq!(names, ["notes.pdf", "agenda.txt"], "{draft}");
}

/// A source with no raw message and no reachable server writes no draft: the
/// refusal is the server leg's, and nothing half-built is left behind.
#[tokio::test]
async fn a_forward_as_attachment_that_cannot_get_the_eml_writes_no_draft() {
    let (tmp, _daemon) = seeded();
    let (mut conn, _) = connect(tmp.path()).await;
    let drafts = fixture::account_dir(tmp.path(), fixture::ACCOUNT).join("drafts");
    let before = fs::read_dir(&drafts).map(|d| d.count()).unwrap_or(0);
    let error = match within(
        "draft.forward",
        conn.call(
            "draft.forward",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"id": "inbox/1"},
                "as_attachment": true,
            }),
        ),
    )
    .await
    {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("the forward is refused, got {other:?}"),
    };
    assert_eq!(error.code, -32603, "{error:?}");
    let after = fs::read_dir(&drafts).map(|d| d.count()).unwrap_or(0);
    assert_eq!(before, after, "no draft was written");
}

/// `mp forward --as-attachment` prints the new draft's selector.
#[test]
fn mp_forward_as_attachment_prints_the_draft() {
    let (_tmp, daemon) = seeded();
    let out = daemon.mp(&[
        "forward",
        "--as-attachment",
        "mp://alpha/inbox/deal@example.com",
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let selector = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(selector.starts_with("mp://alpha/drafts/"), "{selector}");
    let path = daemon.mp(&["path", &selector]);
    let path = String::from_utf8_lossy(&path.stdout).trim().to_string();
    let draft = fs::read_to_string(&path).expect("the draft");
    let attachments = attachments_of(&draft);
    assert_eq!(attachments.len(), 1, "{draft}");
    assert!(attachments[0].ends_with(EXPECTED_NAME), "{draft}");
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
