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

// ---------------------------------------------------------------------------
// 5. A refusal's data
// ---------------------------------------------------------------------------

/// `draft.approve` on a file that will not parse refuses with `-32010` and
/// the `draft.invalid` payload as its `data`: the file and the parser's
/// diagnostics, which a client renders instead of rebuilding them from a
/// listing (`mp_client::session::refusal` keeps it on the blocking path).
#[tokio::test]
async fn an_unparseable_draft_is_refused_with_its_payload() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let stem = fixture::UNPARSEABLE_FILE.trim_end_matches(".md");
    // The watcher announces a file with no id under its stem once its first
    // poll settled; until then the stem resolves to nothing (`-32602`).
    let mut refused = None;
    for _ in 0..100 {
        let error = call_err(
            &mut conn,
            "draft.approve",
            json!({"account": fixture::ACCOUNT, "id": stem}),
        )
        .await;
        if error.code != -32602 {
            refused = Some(error);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let refused = refused.expect("the watcher announced the file");
    assert_eq!(refused.code, -32010);
    let payload: mp_protocol::events::DraftInvalid =
        serde_json::from_value(refused.data.expect("the refusal carries data"))
            .expect("the data is the draft.invalid payload");
    assert_eq!(payload.id, stem);
    assert!(payload.path.ends_with(fixture::UNPARSEABLE_FILE));
    assert!(!payload.diagnostics.is_empty());
}

// ---------------------------------------------------------------------------
// 7. signature.list
// ---------------------------------------------------------------------------

/// `signature.list` answers the signature names, sorted, and the account's
/// default, for any configured account; an unknown one is `-32005`.
#[tokio::test]
async fn signature_list_answers_the_names_and_the_default() {
    let slice = Slice::start();
    std::fs::write(slice.root().join("signatures/lang.md"), "Lang\n").expect("a second file");
    let mut conn = slice.connect().await;

    let listing: mp_protocol::signature::SignatureListing = serde_json::from_value(
        call(
            &mut conn,
            "signature.list",
            json!({"account": fixture::ACCOUNT}),
        )
        .await,
    )
    .expect("a SignatureListing");
    assert_eq!(listing.account, fixture::ACCOUNT);
    assert_eq!(listing.names, vec!["kurz".to_string(), "lang".to_string()]);
    assert_eq!(listing.default, None, "the fixture records no default");

    let storeless = call(
        &mut conn,
        "signature.list",
        json!({"account": fixture::STORELESS_ACCOUNT}),
    )
    .await;
    assert_eq!(storeless["names"], json!(["kurz", "lang"]));

    let unknown = call_err(
        &mut conn,
        "signature.list",
        json!({"account": fixture::UNKNOWN_ACCOUNT}),
    )
    .await;
    assert_eq!(unknown.code, -32005);
}

// ---------------------------------------------------------------------------
// 8. A draft's attachments
// ---------------------------------------------------------------------------

/// `draft.attach` appends a file after the TUI prompt's checks,
/// `draft.attachments` lists the entries resolved, and `draft.detach` removes
/// one by index and leaves the file; each answers the list as it is now.
#[tokio::test]
async fn a_draft_attaches_lists_and_detaches_its_files() {
    let slice = Slice::start();
    let files = slice.root().join("files");
    std::fs::create_dir_all(&files).expect("a files dir");
    let (a, b) = (files.join("a.pdf"), files.join("b.pdf"));
    std::fs::write(&a, b"a").expect("a");
    std::fs::write(&b, b"b").expect("b");
    let mut conn = slice.connect().await;
    let id = json!({"account": fixture::ACCOUNT, "id": fixture::VALID});
    let with = |extra: Value| {
        let mut params = id.clone();
        for (k, v) in extra.as_object().expect("an object") {
            params[k] = v.clone();
        }
        params
    };
    let list = |value: Value| -> mp_protocol::draft::DraftAttachments {
        serde_json::from_value(value).expect("DraftAttachments")
    };

    let empty = list(call(&mut conn, "draft.attachments", id.clone()).await);
    assert!(empty.attachments.is_empty());
    assert!(empty.path.ends_with("angebot.md"));

    list(
        call(
            &mut conn,
            "draft.attach",
            with(json!({"path": a.display().to_string()})),
        )
        .await,
    );
    let two = list(
        call(
            &mut conn,
            "draft.attach",
            with(json!({"path": format!(" {} ", b.display())})),
        )
        .await,
    );
    let entries: Vec<&str> = two.attachments.iter().map(|e| e.entry.as_str()).collect();
    assert_eq!(
        entries,
        vec![a.display().to_string(), b.display().to_string()]
    );
    assert!(two.attachments.iter().all(|e| e.exists));
    assert_eq!(two.attachments[1].index, 1);

    let twice = call_err(
        &mut conn,
        "draft.attach",
        with(json!({"path": a.display().to_string()})),
    )
    .await;
    assert_eq!(twice.code, -32602);
    assert!(
        twice.message.ends_with("is already attached"),
        "{}",
        twice.message
    );
    let missing = call_err(
        &mut conn,
        "draft.attach",
        with(json!({"path": files.join("nope.pdf").display().to_string()})),
    )
    .await;
    assert!(
        missing.message.starts_with("No such file"),
        "{}",
        missing.message
    );

    let one = list(call(&mut conn, "draft.detach", with(json!({"index": 0}))).await);
    let entries: Vec<&str> = one.attachments.iter().map(|e| e.entry.as_str()).collect();
    assert_eq!(entries, vec![b.display().to_string()]);
    assert!(a.is_file(), "the file the entry named stays");
    let out_of_range = call_err(&mut conn, "draft.detach", with(json!({"index": 3}))).await;
    assert_eq!(out_of_range.code, -32602);
}
