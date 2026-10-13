//! `draft.html`: the HTML part a draft would be sent with, as a browser
//! rendition, inline in the answer (PERSO-136).
//!
//! ```text
//! draft.html {account, id|selector} -> {account, id, selector, html, bytes}
//! ```
//!
//! The property that matters is that the preview is the send: the body comes
//! out of `send::draft_html_body`, the function the SMTP bytes and the Graph
//! body are built by, so a reply shows its signature once and its quoted
//! original as the companion HTML carries it. Everything else here is the
//! refusals.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::draft::{DraftCreated, DraftHtml, METHOD_DRAFT_HTML};
use mp_protocol::RpcError;

use mailypoppins::draft::parse_email_draft;

use support::draft_fixture as fixture;
use support::parity::{socket_path, DaemonFixture};

const DEADLINE: Duration = Duration::from_secs(20);

/// The signature the reply below asks for by name.
const SIGNATURE: &str = "kurz";

/// Its one distinctive line.
const SIGNATURE_LINE: &str = "Gruss aus dem Daemon";

/// A seeded root with one signature file, and a daemon serving it. The field
/// order is the drop order.
struct Slice {
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary root");
        fixture::seed(tmp.path());
        let signatures = tmp.path().join("signatures");
        std::fs::create_dir_all(&signatures).expect("the signatures directory");
        std::fs::write(
            signatures.join(format!("{SIGNATURE}.md")),
            format!("{SIGNATURE_LINE}\n"),
        )
        .expect("a signature file");
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
                    kind: ClientKind::Tui,
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

async fn rendition(conn: &mut Connection, params: Value) -> DraftHtml {
    serde_json::from_value(call(conn, METHOD_DRAFT_HTML, params).await).expect("a DraftHtml")
}

/// A reply with a named signature, to the fixture's HTML-only message:
/// the rendition carries the signature once, the quoted original as its
/// companion HTML holds it, and is byte for byte the send path's HTML part
/// under `message.html`'s charset and CSP.
#[tokio::test]
async fn a_reply_renders_its_signature_and_its_quoted_original_as_the_send_would() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let reply: DraftCreated = serde_json::from_value(
        call(
            &mut conn,
            "draft.reply",
            json!({
                "account": fixture::ACCOUNT,
                "source": {"selector": support::read_fixture::MARKUP},
                "signature": SIGNATURE,
            }),
        )
        .await,
    )
    .expect("a DraftCreated");
    let companion = Path::new(&reply.path).with_extension("html");
    assert!(companion.exists(), "the reply quotes the sender's markup");

    let shown = rendition(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "id": reply.id}),
    )
    .await;

    assert_eq!(shown.account, fixture::ACCOUNT);
    assert_eq!(shown.id, reply.id);
    assert_eq!(shown.selector, reply.selector);
    assert_eq!(shown.bytes, shown.html.len() as u64);
    // The policy goes right after the doctype, before anything a sender wrote.
    assert!(
        shown
            .html
            .starts_with("<!DOCTYPE html><meta http-equiv=\"Content-Security-Policy\""),
        "hardened as message.html is: {}",
        shown.html
    );
    assert!(shown.html.contains("<meta charset=\"UTF-8\">"));
    assert_eq!(
        shown.html.matches(SIGNATURE_LINE).count(),
        1,
        "the signature the body carries, once: {}",
        shown.html
    );
    let signature = shown.html.find(SIGNATURE_LINE).unwrap();
    let quote = shown
        .html
        .find("quarterly summary follows")
        .expect("the quoted original");
    assert!(signature < quote, "the signature sits above the quote");
    assert!(
        shown.html.contains("wrote:</p>"),
        "the quote is the companion HTML, not the Markdown fallback: {}",
        shown.html
    );
    assert!(
        !shown.html.contains("<!-- mp:"),
        "no sentinel or quote marker reaches the rendition: {}",
        shown.html
    );

    // The preview is the send: the same function over the same file, with
    // the fixture's `[email]` defaults and no send-time signature.
    let draft = parse_email_draft(Path::new(&reply.path)).expect("the reply parses");
    let sent = mailypoppins::send::draft_html_body(
        &draft,
        &mailypoppins::config::EmailSettings::default(),
        None,
    );
    assert_eq!(
        shown.html,
        mailypoppins::parse::inject_csp_meta(&mailypoppins::parse::ensure_utf8_charset(&sent))
    );

    // A selector addresses the same draft.
    let by_selector = rendition(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "selector": reply.selector}),
    )
    .await;
    assert_eq!(by_selector, shown);
}

/// A draft made from nothing has no companion: its body renders on its own,
/// with no quote block, and an unapproved status previews all the same.
#[tokio::test]
async fn a_plain_draft_renders_its_body_whatever_its_status() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let shown = rendition(
        &mut conn,
        json!({"account": fixture::ACCOUNT, "id": fixture::VALID}),
    )
    .await;
    assert!(shown.html.contains("Body."), "{}", shown.html);
    assert!(!shown.html.contains("padding-top:1em"), "{}", shown.html);
    assert_eq!(
        shown.selector,
        fixture::selector(fixture::ACCOUNT, fixture::VALID)
    );
}

/// A draft id nothing resolves to is the family's `-32602`, worded as every
/// draft command words it and carrying `{account, id}`; an unknown account
/// is `-32005`.
#[tokio::test]
async fn a_missing_draft_and_an_unknown_account_are_refused() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let refused = call_err(
        &mut conn,
        METHOD_DRAFT_HTML,
        json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_ID}),
    )
    .await;
    assert_eq!(refused.code, -32602);
    assert_eq!(
        refused.data,
        Some(json!({"account": fixture::ACCOUNT, "id": fixture::UNKNOWN_ID}))
    );

    let refused = call_err(
        &mut conn,
        METHOD_DRAFT_HTML,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "id": fixture::VALID}),
    )
    .await;
    assert_eq!(refused.code, -32005);

    let refused = call_err(
        &mut conn,
        METHOD_DRAFT_HTML,
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    assert_eq!(
        refused.code, -32602,
        "a draft is addressed by id or selector"
    );
}
