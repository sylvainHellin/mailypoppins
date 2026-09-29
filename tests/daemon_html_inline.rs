//! `message.html`: the browser rendition of a stored message, inline in the
//! answer rather than in a leased file.
//!
//! ```text
//! message.html {account, row_id|id|selector, mailbox?}
//!                  -> {account, row_id, html, bytes}
//! ```
//!
//! The property that matters is that the string is the file: a webview showing
//! `html` shows exactly what a browser opening `message.materialise_html`'s
//! file shows, with the same charset, the same CSP meta tag and the same `cid:`
//! images inlined as `data:` URIs. Everything else here is the refusals.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::rendition::{
    InlineHtmlRefusal, MessageHtml, HTML_FALLBACK_METHOD, MAX_INLINE_HTML_BYTES,
};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::store::{read, BlobStore};

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(20);

const METHOD: &str = "message.html";

/// The uid of the seeded message whose markup references a `cid:` image.
const CID_UID: i64 = 90;

/// The uid of the seeded message whose markup is over the inline limit.
const HUGE_UID: i64 = 91;

/// A 1x1 PNG, base64, as the image part carries it.
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

struct Slice {
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary html-slice root");
        fixture::seed(tmp.path());
        seed_cid_message(tmp.path());
        seed_huge_message(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    async fn connect(&self) -> Connection {
        let mut conn = within(
            "Connection::connect",
            Connection::connect(&socket_path(self.root())),
        )
        .await
        .expect("connecting to a live daemon socket succeeds");
        within(
            "Connection::initialize",
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Gui,
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
        .expect("a compatible handshake succeeds");
        conn
    }

    fn row_id(&self, mailbox: &str, uid: i64) -> i64 {
        let store = fixture::store(self.root(), fixture::ACCOUNT);
        read::find_row_by_uid(&store, fixture::ACCOUNT, mailbox, uid)
            .expect("looking the row up")
            .unwrap_or_else(|| panic!("the fixture holds {mailbox}/{uid}"))
    }
}

/// A `multipart/related` message whose HTML shows an image by `cid:`, ingested
/// with its raw bytes the way an IMAP sync ingests one, so the inline-image
/// scan has a message to resolve the reference against.
fn seed_cid_message(root: &Path) {
    let html = "<html><head><title>Logo</title></head><body><p>Grüße</p><p><img src=\"cid:logo@x\"></p></body></html>";
    let raw = format!(
        "From: designer@example.com\r\nTo: sylvain@example.com\r\nSubject: Logo\r\n\
Message-ID: <logo@example.com>\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"B\"\r\n\r\n\
--B\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n\
--B\r\nContent-Type: image/png; name=\"logo.png\"\r\nContent-ID: <logo@x>\r\n\
Content-Transfer-Encoding: base64\r\nContent-Disposition: inline; filename=\"logo.png\"\r\n\r\n{PNG_B64}\r\n--B--\r\n"
    );
    let mut email = fixture::email(
        "designer@example.com",
        "sylvain@example.com",
        "Logo",
        "Mon, 6 Jul 2026 10:00:00 +0200",
        "Grüße\n",
    );
    email.message_id = Some("<logo@example.com>".to_string());
    email.html_body = Some(html.to_string());
    email.has_attachments = true;
    ingest_raw(root, CID_UID, &email, Some(raw.as_bytes()));
}

/// A message whose markup is one byte over the inline limit before the
/// rendition grows it by its charset and CSP tags.
fn seed_huge_message(root: &Path) {
    let filler = "a".repeat(MAX_INLINE_HTML_BYTES + 1);
    let mut email = fixture::email(
        "bulk@example.com",
        "sylvain@example.com",
        "Huge",
        "Mon, 6 Jul 2026 11:00:00 +0200",
        "huge\n",
    );
    email.message_id = Some("<huge@example.com>".to_string());
    email.html_body = Some(format!("<html><body><p>{filler}</p></body></html>"));
    ingest_raw(root, HUGE_UID, &email, None);
}

fn ingest_raw(
    root: &Path,
    uid: i64,
    email: &mailypoppins::parse::FetchedEmail,
    raw: Option<&[u8]>,
) {
    let dir = fixture::account_dir(root, fixture::ACCOUNT);
    let store = fixture::store(root, fixture::ACCOUNT);
    let blobs = BlobStore::new(dir.join("blobs"));
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account: fixture::ACCOUNT,
            mailbox: "inbox",
            uid,
            email,
            raw,
        },
    )
    .expect("ingest the fixture message");
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

async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("this call must be refused");
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{method}: expected a typed RPC error, got {other:?}"),
    }
}

/// The method is served, which the handshake's capability list says.
#[tokio::test]
async fn the_daemon_advertises_the_inline_rendition() {
    let slice = Slice::start();
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&socket_path(slice.root())),
    )
    .await
    .expect("connect");
    let hello = within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Gui,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: slice.root().to_path_buf(),
                config_dir: slice.root().to_path_buf(),
            },
            &[],
            &[],
        ),
    )
    .await
    .expect("handshake");
    assert!(
        hello.capabilities.iter().any(|cap| cap == METHOD),
        "{METHOD} is advertised, got {:?}",
        hello.capabilities
    );
}

/// The inline string is the file `message.materialise_html` writes for the
/// same row, byte for byte, on a message whose image is referenced by `cid:`.
#[tokio::test]
async fn the_inline_rendition_is_byte_identical_to_the_materialised_file() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id("inbox", CID_UID);
    let address = json!({"account": fixture::ACCOUNT, "row_id": row_id});

    let inline: MessageHtml =
        serde_json::from_value(call(&mut conn, METHOD, address.clone()).await)
            .expect("the answer is a MessageHtml");
    let handle = call(&mut conn, "message.materialise_html", address).await;
    let path = Path::new(handle["path"].as_str().expect("path is a string"));
    let written = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    assert_eq!(
        inline.html.as_bytes(),
        written.as_slice(),
        "the inline rendition and the materialised file are one byte sequence"
    );
    assert_eq!(inline.bytes, written.len() as u64);
    assert_eq!(inline.bytes, handle["bytes"].as_u64().expect("bytes"));
    assert_eq!(inline.row_id, row_id);
    assert_eq!(inline.account, fixture::ACCOUNT);

    assert!(
        inline
            .html
            .starts_with("<meta http-equiv=\"Content-Security-Policy\""),
        "the CSP tag is the first thing in the document, got {:?}",
        inline.html.chars().take(80).collect::<String>()
    );
    assert!(inline.html.contains("<meta charset=\"UTF-8\">"));
    assert!(inline.html.contains("Grüße"));
    assert!(
        inline
            .html
            .contains(&format!("src=\"data:image/png;base64,{PNG_B64}\"")),
        "the cid: image is inlined as a data: URI, got {}",
        inline.html
    );
    assert!(
        !inline.html.contains("cid:logo@x"),
        "no cid: reference survives, got {}",
        inline.html
    );

    let released = call(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle["handle"]}),
    )
    .await;
    assert_eq!(released, json!({}));
}

/// The three addresses `message.get` takes all reach the same rendition, and
/// naming none is `-32602`.
#[tokio::test]
async fn the_rendition_is_addressed_as_message_get_addresses_a_message() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id("inbox", CID_UID);

    let by_row = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": row_id}),
    )
    .await;
    let by_id = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "id": format!("inbox/{CID_UID}")}),
    )
    .await;
    let by_selector = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "selector": "logo@example.com", "mailbox": "inbox"}),
    )
    .await;
    assert_eq!(by_row, by_id);
    assert_eq!(by_row, by_selector);

    let none = call_err(&mut conn, METHOD, json!({"account": fixture::ACCOUNT})).await;
    assert_eq!(none.code, -32602, "{none:?}");
}

/// A message with no markup and an account nobody configured are the
/// refusals `message.materialise_html` answers for them.
#[tokio::test]
async fn the_refusals_are_the_file_path_s_own() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let plain =
        json!({"account": fixture::ACCOUNT, "selector": fixture::BERICHT, "mailbox": "inbox"});
    let inline = call_err(&mut conn, METHOD, plain.clone()).await;
    let file = call_err(&mut conn, "message.materialise_html", plain).await;
    assert_eq!(inline.code, -32602, "{inline:?}");
    assert_eq!(inline.code, file.code);
    assert_eq!(inline.message, file.message);

    let unknown = call_err(
        &mut conn,
        METHOD,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "row_id": 1}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
}

/// A rendition over the inline limit is `frame_too_large` naming the file
/// path, the file path serves it, and the connection stays usable.
#[tokio::test]
async fn a_rendition_over_the_inline_limit_falls_back_to_the_file_path() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id("inbox", HUGE_UID);
    let address = json!({"account": fixture::ACCOUNT, "row_id": row_id});

    let refused = call_err(&mut conn, METHOD, address.clone()).await;
    assert_eq!(refused.code, ErrorCode::FrameTooLarge.code(), "{refused:?}");
    let data: InlineHtmlRefusal =
        serde_json::from_value(refused.data.expect("the refusal carries data"))
            .expect("the data is an InlineHtmlRefusal");
    assert_eq!(data.limit, MAX_INLINE_HTML_BYTES as u64);
    assert!(data.seen > data.limit, "{data:?}");
    assert_eq!(data.fallback, HTML_FALLBACK_METHOD);

    let handle = call(&mut conn, &data.fallback, address).await;
    assert_eq!(
        handle["bytes"].as_u64(),
        Some(data.seen),
        "the file is the rendition the inline answer refused"
    );
    call(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle["handle"]}),
    )
    .await;
}
