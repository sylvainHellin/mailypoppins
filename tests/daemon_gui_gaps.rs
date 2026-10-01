//! The daemon gaps the desktop's M3 and M4 worked around client-side (#0131).
//!
//! Each section is one gap from the `BACKLOG.md` "Next" bullet, in the
//! bullet's order, and pins the daemon half of it over a live daemon and the
//! draft fixture: what a client may now rely on instead of reading or writing
//! a file behind the daemon's back.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::draft::{DraftCreated, DraftListing};
use mp_protocol::RpcError;

use mailypoppins::draft::{parse_email_draft, SIG_START};

use support::draft_fixture as fixture;
use support::parity::{socket_path, DaemonFixture};

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The signature the rows below ask for by name.
const SIGNATURE: &str = "kurz";

/// Its one distinctive line, which a draft carrying it shows in its body.
const SIGNATURE_LINE: &str = "Gruss aus dem Daemon";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

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

async fn created(conn: &mut Connection, method: &str, params: Value) -> DraftCreated {
    serde_json::from_value(call(conn, method, params).await).expect("a DraftCreated")
}

/// The body of the draft file a writer answered with.
fn body_of(created: &DraftCreated) -> String {
    parse_email_draft(Path::new(&created.path))
        .expect("the written draft parses")
        .body_markdown
}

// ---------------------------------------------------------------------------
// 1. A signature for a reply and a forward
// ---------------------------------------------------------------------------

/// `draft.reply` and `draft.forward` take the `signature` and `no_signature`
/// `draft.create` takes, so the forward wizard's signature select reaches the
/// file: a named signature is spliced in its sentinels, and `no_signature`
/// leaves the body without one.
#[tokio::test]
async fn a_reply_and_a_forward_carry_the_signature_they_name_or_none() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let source = json!({"selector": support::read_fixture::BERICHT});

    let reply = created(
        &mut conn,
        "draft.reply",
        json!({"account": fixture::ACCOUNT, "source": source, "signature": SIGNATURE}),
    )
    .await;
    let body = body_of(&reply);
    assert!(body.contains(SIG_START), "the block is spliced: {body}");
    assert!(body.contains(SIGNATURE_LINE), "the named signature: {body}");

    let forward = created(
        &mut conn,
        "draft.forward",
        json!({"account": fixture::ACCOUNT, "source": source, "signature": SIGNATURE}),
    )
    .await;
    assert!(body_of(&forward).contains(SIGNATURE_LINE));

    let bare = created(
        &mut conn,
        "draft.forward",
        json!({"account": fixture::ACCOUNT, "source": source, "no_signature": true}),
    )
    .await;
    let body = body_of(&bare);
    assert!(
        !body.contains(SIG_START),
        "no_signature splices none: {body}"
    );
    assert!(!body.contains(SIGNATURE_LINE));
}

// ---------------------------------------------------------------------------
// 2. A body and the recipients on draft.create
// ---------------------------------------------------------------------------

/// `draft.create` takes the compose wizard's inline body and its `headers`,
/// so the new draft is written whole in one call: the recipients and the
/// subject in the frontmatter, the body above the signature.
#[tokio::test]
async fn a_new_draft_is_written_with_its_body_and_its_headers() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let draft = created(
        &mut conn,
        "draft.create",
        json!({
            "account": fixture::ACCOUNT,
            "name": "mit-text",
            "signature": SIGNATURE,
            "body": "Kurze Frage:\nmorgen um zehn?\n",
            "headers": {"to": "robin@example.com", "cc": "", "bcc": "chef@example.com", "subject": "Termin"},
        }),
    )
    .await;
    let parsed = parse_email_draft(Path::new(&draft.path)).expect("it parses");
    assert_eq!(parsed.frontmatter.to.as_deref(), Some("robin@example.com"));
    assert_eq!(parsed.frontmatter.bcc.as_deref(), Some("chef@example.com"));
    assert_eq!(parsed.frontmatter.subject, "Termin");
    let body = parsed.body_markdown;
    let text = body.find("morgen um zehn?").expect("the body is written");
    let signature = body.find(SIGNATURE_LINE).expect("the signature is spliced");
    assert!(
        text < signature,
        "the body sits above the signature: {body}"
    );

    // Absent, both leave the skeleton `mp new` has always written.
    let bare = created(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "leer", "no_signature": true}),
    )
    .await;
    let parsed = parse_email_draft(Path::new(&bare.path)).expect("it parses");
    assert_eq!(parsed.frontmatter.to, None);
    assert_eq!(parsed.body_markdown.trim(), "");

    // A partial override is refused before anything is written.
    let refused = call_err(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "halb", "headers": {"to": "x@example.com"}}),
    )
    .await;
    assert_eq!(refused.code, -32602);
    assert!(!fixture::draft_path(slice.root(), fixture::ACCOUNT, "halb.md").exists());
}

// ---------------------------------------------------------------------------
// 3. Bcc on a listed draft
// ---------------------------------------------------------------------------

/// A `draft.list` row carries the `bcc:` field beside `to` and `cc`, so a
/// recipients dialog fills all three from the row it lists; a draft that
/// blind-copies nobody lists `null`.
#[tokio::test]
async fn a_listed_draft_carries_its_bcc() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let draft = created(
        &mut conn,
        "draft.create",
        json!({
            "account": fixture::ACCOUNT,
            "name": "blind",
            "headers": {"to": "robin@example.com", "cc": "", "bcc": "chef@example.com", "subject": "Blind"},
        }),
    )
    .await;

    let listing: DraftListing = serde_json::from_value(
        call(
            &mut conn,
            "draft.list",
            json!({"account": fixture::ACCOUNT}),
        )
        .await,
    )
    .expect("a DraftListing");
    let row = |id: &str| {
        listing
            .drafts
            .iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("{id} is listed"))
            .clone()
    };
    assert_eq!(row(&draft.id).bcc.as_deref(), Some("chef@example.com"));
    assert_eq!(
        row(fixture::VALID).bcc,
        None,
        "the seeded draft has an empty bcc:"
    );
}

// ---------------------------------------------------------------------------
// 4. The subject on DraftCreated
// ---------------------------------------------------------------------------

/// Every writer answers the subject the file was written with: the builder's
/// `Re:` for a reply, the override for a forward with `headers`, and `""`
/// for a bare skeleton.
#[tokio::test]
async fn a_created_draft_names_the_subject_it_was_written_with() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let source = json!({"selector": support::read_fixture::BERICHT});

    let reply = created(
        &mut conn,
        "draft.reply",
        json!({"account": fixture::ACCOUNT, "source": source}),
    )
    .await;
    assert_eq!(reply.subject, "Re: Bericht über Anträge");

    let forward = created(
        &mut conn,
        "draft.forward",
        json!({
            "account": fixture::ACCOUNT,
            "source": source,
            "headers": {"to": "x@example.com", "cc": "", "bcc": "", "subject": "Zur Info"},
        }),
    )
    .await;
    assert_eq!(forward.subject, "Zur Info");

    let bare = created(
        &mut conn,
        "draft.create",
        json!({"account": fixture::ACCOUNT, "name": "ohne"}),
    )
    .await;
    assert_eq!(bare.subject, "");
}
