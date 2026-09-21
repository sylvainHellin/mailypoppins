//! `RD-06`: the store's own Markdown rendition of a stored message, as a file
//! a client opens read-only in `$EDITOR` (#0126, plan unit P5-U10c).
//!
//! This is a **contract test**, written before `message.materialise_markdown`
//! exists and against the shape fixed here, in `docs/daemon-protocol.md` and in
//! `crates/mp-protocol/fixtures/message.materialise_markdown.{request,response}.json`.
//! It compiles at HEAD and fails at HEAD with `-32601`, which is the proof
//! there is no stub behind it. The implementer does not edit this file.
//!
//! # The surface under test
//!
//! ```text
//! message.materialise_markdown {account, row_id|id|selector, mailbox?}
//!                              -> {handle, path, name, bytes, expires_at}
//! ```
//!
//! # Why a handle and not a path
//!
//! **The store keeps no Markdown file per message.** `src/store/read.rs`'s
//! `render_markdown` builds the frontmatter from the `messages` row and the
//! body from the blob store on every call; the file era's `.md` files died with
//! #0037 and #0075 replaced them with a scratch rendition the TUI writes under
//! `parse::materialisation_dir`. So there is no path that is "the whole
//! answer": the daemon has to write the bytes somewhere the client can open,
//! which is exactly what `message.materialise_attachment` and
//! `message.materialise_html` already do.
//!
//! **So it is the third member of that family, not a method of its own.** The
//! name follows the two that are there, the result is their result key for
//! key, the file lands in `<data_dir>/runtime/handles/<handle>/<name>` at mode
//! 0700, `message.release_handle` releases it, the lifetime is the same ten
//! minutes, and `ANO-6` applies unchanged: a live handle pins every blob its
//! materialisation read, so a retention sweep running beside an open editor
//! cannot evict the body the file was built from. A fourth spelling would be a
//! second lifetime rule for one kind of scratch file.
//!
//! **The file is written 0444.** That is #0075's rule and it is a property of
//! the rendition rather than of the client: `$EDITOR` opens the buffer
//! read-only and says so, instead of letting someone believe an edit reaches
//! the message. The daemon owns the bytes now, so it owns the mode.
//!
//! # Addressing
//!
//! `{account, row_id}`, `{account, id}` or `{account, selector, mailbox?}`,
//! exactly as `message.get` addresses a message, and naming none or more than
//! one is `-32602`.
//!
//! `row_id` is the one the other two materialisers do not take, and it is the
//! reason this method takes all three: the TUI holds a `MessageRef`, which is
//! the synthetic row key and nothing else (#0050), and
//! `readonly_view_for_row(app, row_id)` is the call site. Making it spell
//! `"<mailbox>/<uid>"` would make it carry a second identity for every listed
//! row, which is the argument P5-U4 already made for `message.get`.
//!
//! # What this file does not assert
//!
//! The rendition's *text* is `store::read::render_markdown`'s, so the
//! expectation is that function over the same row rather than a second copy of
//! the frontmatter layout. A test that spelled the YAML out would pin the
//! rendition twice and drift from the one that ships.

mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::store::read;
use mailypoppins::store::BlobStore;

use support::parity::{socket_path, DaemonFixture};
use support::read_fixture as fixture;

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The method under test, written once so a rename is one diff.
const METHOD: &str = "message.materialise_markdown";

/// The five keys of a materialised handle, which this method shares with the
/// two materialisers that are already there.
const HANDLE_KEYS: [&str; 5] = ["bytes", "expires_at", "handle", "name", "path"];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root and a daemon serving it. The field order is the drop order.
struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory it was
    /// reading goes away. Nothing in this file runs a command.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary markdown-slice root");
        fixture::seed(tmp.path());
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
        .expect("a compatible handshake succeeds");
        conn
    }

    /// The row id of the message whose `Message-ID` key is `message_id`, read
    /// out of the fixture store the way the daemon reads it.
    fn row_id(&self, account: &str, mailbox: &str, message_id: &str) -> i64 {
        let store = fixture::store(self.root(), account);
        read::list_mailbox(&store, account, mailbox)
            .unwrap_or_else(|e| panic!("listing {account}/{mailbox}: {e:#}"))
            .into_iter()
            .find(|row| row.message_id.contains(message_id))
            .unwrap_or_else(|| panic!("the fixture holds {message_id} in {mailbox}"))
            .id
    }

    /// The rendition the store itself produces for a row: the oracle this
    /// method's bytes are measured against.
    fn rendered(&self, account: &str, row_id: i64) -> String {
        let store = fixture::store(self.root(), account);
        let row = read::find_by_id(&store, row_id)
            .expect("reading the row")
            .expect("the fixture holds the row");
        let blobs = BlobStore::new(fixture::account_dir(self.root(), account).join("blobs"));
        read::render_markdown(&store, &blobs, &row)
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

/// The keys of a JSON object, sorted, or a failure naming what came instead.
fn assert_keys(value: &Value, expected: &[&str], label: &str) {
    let map = value
        .as_object()
        .unwrap_or_else(|| panic!("{label} is a JSON object, got {value}"));
    let mut found: Vec<&str> = map.keys().map(String::as_str).collect();
    found.sort_unstable();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    assert_eq!(found, want, "{label} carries exactly these fields, got {value}");
}

// ---------------------------------------------------------------------------
// 1. The method is registered
// ---------------------------------------------------------------------------

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the name appearing there is
/// what says it is registered rather than merely documented.
///
/// **Fails at HEAD**: nothing registers the method, so it is in no capability
/// list.
#[tokio::test]
async fn the_daemon_advertises_the_markdown_rendition() {
    let slice = Slice::start();
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&socket_path(slice.root())),
    )
    .await
    .expect("connect");
    let result = within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
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

    let capabilities: Vec<&str> = result.capabilities.iter().map(String::as_str).collect();
    assert!(
        capabilities.contains(&METHOD),
        "{METHOD} must be advertised beside the two materialisers it joins, got {capabilities:?}"
    );
    for sibling in ["message.materialise_html", "message.release_handle"] {
        assert!(
            capabilities.contains(&sibling),
            "{sibling} is the family {METHOD} joins and must still be there"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The happy path
// ---------------------------------------------------------------------------

/// The rendition is the store's own, byte for byte, in a file the client can
/// open and cannot save over.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn the_rendition_is_the_store_s_own_markdown_in_a_read_only_file() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id(fixture::ACCOUNT, "inbox", fixture::BERICHT);

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": row_id}),
    )
    .await;
    assert_keys(&result, &HANDLE_KEYS, "the rendition handle");

    let path = Path::new(result["path"].as_str().expect("path is a string"));
    let name = result["name"].as_str().expect("name is a string");
    assert!(
        name.ends_with(".md"),
        "the store's own view of a message is Markdown (#0075), got {name:?}"
    );
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some(name),
        "the file keeps the name the answer gave, inside the handle's own directory"
    );
    assert!(
        path.starts_with(slice.root().join("runtime").join("handles")),
        "a rendition lands under the daemon's runtime handle directory, got {}",
        path.display()
    );

    let written = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("the client can open {}: {e}", path.display()));
    assert_eq!(
        written,
        slice.rendered(fixture::ACCOUNT, row_id),
        "the rendition is `store::read::render_markdown` over the same row, not a second layout"
    );
    assert!(
        written.starts_with("---\n"),
        "the rendition opens with YAML frontmatter, got {:?}",
        written.chars().take(40).collect::<String>()
    );
    assert!(
        written.contains("subject: Bericht über Anträge"),
        "the frontmatter carries the stored headers, got {written}"
    );
    assert_eq!(
        result["bytes"].as_u64().expect("bytes is a number"),
        written.len() as u64,
        "`bytes` is the length of the file at `path`, as it is for the html rendition"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).expect("stat the rendition").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o444,
            "#0075's rule is that $EDITOR opens the buffer read-only and says so"
        );
    }

    assert!(
        !result["expires_at"]
            .as_str()
            .expect("expires_at is a string")
            .is_empty(),
        "a handle carries the instant it dies, RFC 3339, like its two siblings"
    );
}

/// The handle is released by the family's own release method, and the
/// directory goes with it.
///
/// **Fails at HEAD**: `-32601` on the materialisation.
#[tokio::test]
async fn the_rendition_is_released_by_the_family_s_release_method() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id(fixture::ACCOUNT, "inbox", fixture::BERICHT);

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "row_id": row_id}),
    )
    .await;
    let handle = result["handle"].as_str().expect("handle is a string").to_string();
    assert!(
        !handle.is_empty() && handle.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "a handle is opaque, non-empty and usable as a directory name, got {handle:?}"
    );
    let path = result["path"].as_str().expect("path").to_string();
    assert!(Path::new(&path).exists(), "the file is there before the release");

    let released = call(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle}),
    )
    .await;
    assert_eq!(released, json!({}), "the release answers with the empty object");
    assert!(
        !Path::new(&path).exists(),
        "releasing a handle unlinks its directory, as it does for an attachment"
    );
}

/// The three addresses name one message, and they name the same one.
///
/// **Fails at HEAD**: `-32601`.
#[tokio::test]
async fn the_three_addresses_reach_the_same_message() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id(fixture::ACCOUNT, "inbox", fixture::BERICHT);
    let expected = slice.rendered(fixture::ACCOUNT, row_id);

    for params in [
        json!({"account": fixture::ACCOUNT, "row_id": row_id}),
        json!({"account": fixture::ACCOUNT, "id": "inbox/1"}),
        json!({
            "account": fixture::ACCOUNT,
            "selector": format!("mp://{}/inbox/{}", fixture::ACCOUNT, fixture::BERICHT),
        }),
    ] {
        let result = call(&mut conn, METHOD, params.clone()).await;
        let path = result["path"].as_str().expect("path is a string");
        let written = fs::read_to_string(path).expect("the client can open the rendition");
        assert_eq!(
            written, expected,
            "{params} addresses the same message as every other form"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. The refusals
// ---------------------------------------------------------------------------

/// Everything a caller can get wrong about the address is `-32602`: a row id
/// no message has, an id that is not `"<mailbox>/<uid>"`, a selector that
/// resolves to nothing, no address at all and two addresses at once.
///
/// **Fails at HEAD**: every call comes back `-32601` instead.
#[tokio::test]
async fn a_bad_address_is_invalid_params() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let row_id = slice.row_id(fixture::ACCOUNT, "inbox", fixture::BERICHT);

    for (what, params) in [
        (
            "a row id no message has",
            json!({"account": fixture::ACCOUNT, "row_id": 9_999_999}),
        ),
        (
            "an id that is not <mailbox>/<uid>",
            json!({"account": fixture::ACCOUNT, "id": "inbox"}),
        ),
        (
            "a uid the mailbox does not hold",
            json!({"account": fixture::ACCOUNT, "id": "inbox/9999"}),
        ),
        (
            "a selector that resolves to nothing",
            json!({
                "account": fixture::ACCOUNT,
                "selector": format!("mp://{}/inbox/nobody@example.com", fixture::ACCOUNT),
            }),
        ),
        ("no address at all", json!({"account": fixture::ACCOUNT})),
        (
            "two addresses at once",
            json!({"account": fixture::ACCOUNT, "row_id": row_id, "id": "inbox/1"}),
        ),
    ] {
        let error = call_err(&mut conn, METHOD, params).await;
        assert_eq!(
            error.code,
            -32602,
            "{what} is invalid params, got {error:?}"
        );
    }
}

/// The two account refusals every read method makes, with the payloads the
/// error table fixes.
///
/// **Fails at HEAD**: `-32601`.
#[tokio::test]
async fn the_account_refusals_are_the_read_family_s() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let unknown = call_err(
        &mut conn,
        METHOD,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "row_id": 1}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(
        unknown.data,
        Some(json!({"account": fixture::UNKNOWN_ACCOUNT})),
        "account_unknown names the account that was asked for"
    );

    let storeless = call_err(
        &mut conn,
        METHOD,
        json!({"account": fixture::STORELESS_ACCOUNT, "row_id": 1}),
    )
    .await;
    assert_eq!(storeless.code, ErrorCode::AccountNotReady.code());
    assert_eq!(
        storeless
            .data
            .as_ref()
            .and_then(|d| d.get("account"))
            .and_then(Value::as_str),
        Some(fixture::STORELESS_ACCOUNT),
        "account_not_ready names the account and the state account.list reports for it"
    );
}

/// A message with no stored body still renders: `render_markdown` degrades to
/// an empty body rather than failing, and the method may not turn that into a
/// refusal.
///
/// The fixture's `alpha inbox/4` is the row with no readable body, which is
/// the same row `mp show` prints its "no stored body" sentence for.
///
/// **Fails at HEAD**: `-32601`.
#[tokio::test]
async fn a_message_with_no_stored_body_still_renders() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let result = call(
        &mut conn,
        METHOD,
        json!({"account": fixture::ACCOUNT, "id": "inbox/4"}),
    )
    .await;
    let written = fs::read_to_string(result["path"].as_str().expect("path"))
        .expect("the client can open the rendition");
    assert!(
        written.starts_with("---\n"),
        "an empty body is still a rendition with frontmatter, got {written:?}"
    );
}
