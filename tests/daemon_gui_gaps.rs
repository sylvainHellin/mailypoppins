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
use mp_protocol::draft::DraftCreated;
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

#[allow(dead_code)]
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
