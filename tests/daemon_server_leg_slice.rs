//! `LST-09` and `LST-08`: the two operations that put the TUI's server search
//! leg behind the daemon (#0126, plan unit P5-U10c).
//!
//! This is a **contract test**, written before either method exists and
//! against the shapes fixed here, in `docs/daemon-protocol.md` and in the four
//! `message.{fetch,search_server}.{request,response}.json` fixtures. It
//! compiles at HEAD and fails at HEAD with `-32601`. The implementer does not
//! edit this file.
//!
//! # The surface under test
//!
//! ```text
//! message.fetch         {account, mailbox, message_id}      -> {operation_id}
//!   settles with        {account, mailbox, uid, row_id, selector, already_present}
//!
//! message.search_server {account, query, mailboxes?, limit?, exclude_message_ids?}
//!                                                           -> {operation_id}
//!   streams             state.event kind `message.server_hit`
//!                       payload {operation_id, hit: ServerSearchHit}
//!   settles with        {account, query, hits, deduplicated, unreachable: [{mailbox, error}]}
//! ```
//!
//! # Why `message.search_server` and not `message.list_server`
//!
//! `docs/parity-matrix.md`'s `LST-08` note names the method `message.list_server`.
//! That name is taken: P4-U10 registered it as the query behind `mp fetch`,
//! `{account, mailbox, limit, criteria?}` against one mailbox, answering the
//! six header fields and the body text that listing prints. It is not the
//! search leg, and redefining a live method would be a protocol break for a
//! command that has nothing to do with this one.
//!
//! `LST-06`'s own note in the same document already names the method this
//! wants: `message.search_server`, "the twin of `message.list_server`". That is
//! what this file pins, and the matrix's `LST-08` line is corrected to match.
//!
//! # Why an operation, and what streams
//!
//! `src/tui/helpers.rs`'s `lib_do_multi_search` opens one IMAP session,
//! searches each target mailbox in turn with a per-mailbox budget, and returns
//! when the last one answers. The TUI runs it on a background thread and
//! paints the whole batch at the end. Moving it onto an operation with
//! streamed hits is what lets the overlay fill in as the mailboxes answer,
//! which is what the local-first pass (#0105) already does for the FTS half.
//!
//! **One hit per event**, kind `message.server_hit`, payload
//! `{operation_id, hit}`. The operation id is on the payload because a client
//! may have two searches in flight after a fast retype, and the overlay's own
//! generation counter is exactly that problem solved client-side today.
//!
//! **The query travels as the grammar a user types.** The overlay holds a
//! parsed `search::Query`, and the method takes a string, so the client renders
//! it back with `search::to_query_string`. One parser serves every backend,
//! which is `LST-06`'s whole point, and an engine enum on the wire would pin
//! the AST into the protocol.
//!
//! **Deduplication is by Message-ID and it is the daemon's.** The local pass
//! has already run when the server leg starts, so the client sends the
//! Message-IDs it is already showing as `exclude_message_ids` and the daemon
//! drops a hit that matches one, counting it in `deduplicated`. Dropping them
//! client-side would work too and would make the count a client-side fact that
//! no other client could reproduce.
//!
//! **A mailbox that fails does not fail the operation.** `lib_do_multi_search`
//! logs a warning per mailbox and keeps going, because a search across five
//! folders that one server refuses is still four folders of answers; the
//! settle's `unreachable` is that list, and the operation `succeeded`.
//!
//! # `message.fetch` is idempotent, not a refusal
//!
//! The overlay's `f` refuses a hit it can already see with "Already in the
//! local store", and that refusal is a client-side guard over a row the hit had
//! already resolved to. The method does not repeat it: a fetch of a message the
//! store already holds answers with that row and `already_present: true`, and
//! opens no session. Two reasons. A client that raced a sync would otherwise
//! get an error for the state it wanted, and the short-circuit is what makes
//! the method's success path reachable in a test with no server, which is the
//! only part of it that is.
//!
//! The client keeps its sentence by branching on `already_present`, which is
//! one boolean against a refusal it would have had to match on.
//!
//! # What is NOT TESTABLE offline, and the manual check
//!
//! There is no fake IMAP server in this tree, and neither method's happy path
//! can run without one: `message.fetch` has to `UID SEARCH HEADER Message-ID`
//! and fetch the bytes, and `message.search_server` has to run a `SEARCH` per
//! mailbox. Every other slice with a server leg has the same hole, which is why
//! `tests/support/sync_fixture.rs`'s `gamma` account exists: it configures a
//! server on the discard port and has no credentials, so credential resolution
//! refuses *before* anything opens a socket. That is the seam these rows use.
//!
//! So what is pinned here is the shape: the methods are registered and declared
//! the way the operation family declares one, the parameters are validated, the
//! two account refusals are the read family's, the idempotent fetch settles
//! without a server, and an account that cannot be reached settles as a failed
//! operation carrying the secret store's own sentence rather than throwing at
//! the call.
//!
//! **The owner's manual check, once the implementer is done**, against a real
//! account (`~/notes` has the TUM and Proton setups):
//!
//! 1. `mp tui`, `ff`, type a term that is on the server and not in the local
//!    index, and confirm hits appear one at a time rather than in one batch.
//! 2. Confirm a term that is in *both* shows once, and that the footer's count
//!    of server hits excludes it.
//! 3. Put the cursor on a server-only hit, press `f`, and confirm the row
//!    becomes openable (`Enter` renders its Markdown) without a sync.
//! 4. Press `f` again on the same row and confirm the overlay says "Already in
//!    the local store" rather than fetching it twice.
//! 5. Disconnect the network and repeat 1: the overlay reports the failure per
//!    mailbox and the local pass's hits stay on screen.

mod support;

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::store::read;

use support::parity::{socket_path, DaemonFixture};
use support::sync_fixture as fixture;

const DEADLINE: Duration = Duration::from_secs(20);

/// An operation settles within this, or the row fails rather than hanging.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// `LST-09`.
const FETCH: &str = "message.fetch";

/// `LST-08`, and with it `LST-06`'s unmigrated `CLI_ENGINE_RESIDUE` group.
const SEARCH_SERVER: &str = "message.search_server";

/// The keys a settled `message.fetch` answers with.
const FETCH_RESULT: [&str; 6] = [
    "account",
    "already_present",
    "mailbox",
    "row_id",
    "selector",
    "uid",
];

/// The keys a settled `message.search_server` answers with.
const SEARCH_RESULT: [&str; 5] = [
    "account",
    "deduplicated",
    "hits",
    "query",
    "unreachable",
];

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The sync fixture: the read fixture's accounts plus `gamma`, which
/// configures a server it has no credentials for.
struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory it was
    /// reading goes away.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary server-leg root");
        fixture::seed(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &std::path::Path {
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

fn operation_id(result: &Value) -> String {
    let id = result["operation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("an operation answers with an operation_id: {result}"));
    assert!(!id.is_empty(), "an operation id is never empty");
    id.to_string()
}

/// Poll `operation.status` until the operation leaves `queued`/`running`.
async fn settle(conn: &mut Connection, id: &str) -> Value {
    let start = Instant::now();
    loop {
        let status = call(conn, "operation.status", json!({"operation_id": id})).await;
        let state = status["state"]
            .as_str()
            .unwrap_or_else(|| panic!("operation.status always carries a state: {status}"))
            .to_string();
        if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
            return status;
        }
        assert!(
            start.elapsed() < SETTLE_DEADLINE,
            "operation {id} was still {state} after {SETTLE_DEADLINE:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn succeeded(status: &Value) -> &Value {
    assert_eq!(
        status["state"], "succeeded",
        "this operation must succeed, got {status}"
    );
    &status["result"]
}

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
// 1. Both methods are registered
// ---------------------------------------------------------------------------

/// The handshake derives the capability list from the dispatcher, so the two
/// names appearing there is what says they are registered.
///
/// It also pins that `message.list_server` is *still* there and is a different
/// method: `mp fetch` keeps its query, and the search leg gets its own name.
///
/// **Fails at HEAD**: neither name is registered.
#[tokio::test]
async fn the_daemon_advertises_both_server_leg_methods() {
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

    for method in [FETCH, SEARCH_SERVER] {
        assert!(
            capabilities.contains(&method),
            "{method} must be registered, got {capabilities:?}"
        );
    }
    assert!(
        capabilities.contains(&"message.list_server"),
        "`mp fetch`'s query keeps its name; the search leg does not take it"
    );
}

// ---------------------------------------------------------------------------
// 2. message.fetch: the parameters
// ---------------------------------------------------------------------------

/// Everything a caller can get wrong about a fetch is `-32602`, and the two
/// account refusals are the read family's.
///
/// The address is the one the overlay has in hand: the account, the sidebar
/// mailbox the hit came from, and the `Message-ID` the server reported. Not a
/// uid, because a server-only hit has none this store knows; not a selector,
/// because a selector names a row and the whole point is that there is not one
/// yet.
///
/// **Fails at HEAD**: every call comes back `-32601`.
#[tokio::test]
async fn a_fetch_validates_its_address() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for (what, params) in [
        ("no account", json!({"mailbox": "inbox", "message_id": "<a@b>"})),
        (
            "no mailbox",
            json!({"account": fixture::ACCOUNT, "message_id": "<a@b>"}),
        ),
        (
            "no message id",
            json!({"account": fixture::ACCOUNT, "mailbox": "inbox"}),
        ),
        (
            "a mailbox the account does not have",
            json!({
                "account": fixture::ACCOUNT,
                "mailbox": fixture::UNKNOWN_MAILBOX,
                "message_id": "<a@b>",
            }),
        ),
        (
            "a message id that is not one",
            json!({"account": fixture::ACCOUNT, "mailbox": "inbox", "message_id": ""}),
        ),
    ] {
        let error = call_err(&mut conn, FETCH, params).await;
        assert_eq!(error.code, -32602, "{what} is invalid params, got {error:?}");
    }

    let unknown = call_err(
        &mut conn,
        FETCH,
        json!({
            "account": fixture::UNKNOWN_ACCOUNT,
            "mailbox": "inbox",
            "message_id": "<a@b>",
        }),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(unknown.data, Some(json!({"account": fixture::UNKNOWN_ACCOUNT})));
}

/// A message the store already holds is answered from the store, with
/// `already_present: true` and no session at all.
///
/// This is the one success path of `message.fetch` that is reachable with no
/// server, and it is reachable *because* the short-circuit comes before the
/// backend is resolved: `alpha` configures no IMAP at all, so a method that
/// resolved credentials first would fail here.
///
/// **Fails at HEAD**: `-32601`.
#[tokio::test]
async fn fetching_a_message_the_store_already_holds_is_idempotent() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let store = support::read_fixture::store(slice.root(), fixture::ACCOUNT);
    let row = read::list_mailbox(&store, fixture::ACCOUNT, "inbox")
        .expect("the fixture store lists")
        .into_iter()
        .find(|row| !row.message_id.is_empty())
        .expect("the fixture has a message with a Message-ID");

    let started = call(
        &mut conn,
        FETCH,
        json!({
            "account": fixture::ACCOUNT,
            "mailbox": "inbox",
            "message_id": row.message_id,
        }),
    )
    .await;
    let id = operation_id(&started);
    let status = settle(&mut conn, &id).await;
    let result = succeeded(&status);

    assert_keys(result, &FETCH_RESULT, "a settled message.fetch");
    assert_eq!(
        result["already_present"],
        json!(true),
        "a message the store holds is not fetched twice: {result}"
    );
    assert_eq!(
        result["row_id"].as_i64(),
        Some(row.id),
        "the answer names the row the store already had: {result}"
    );
    assert_eq!(result["uid"].as_i64(), Some(row.uid));
    assert_eq!(result["account"], json!(fixture::ACCOUNT));
    assert_eq!(result["mailbox"], json!("inbox"));
    assert_eq!(
        result["selector"]
            .as_str()
            .expect("a stored row has a selector"),
        mailypoppins::selector::Selector::for_message(fixture::ACCOUNT, &row).to_string(),
        "the answer names the row the way every other method names it"
    );
}

/// An account whose credentials cannot be resolved fails the *operation*, not
/// the call: the id is issued, the work starts, and it settles `failed`.
///
/// `gamma` is the fixture's account with a configured server and no
/// credentials, and the refusal it produces is the secret store's own sentence,
/// the one `sync.quick` already reports for it.
///
/// **Fails at HEAD**: `-32601` at the call, so no id is ever issued.
#[tokio::test]
async fn a_fetch_from_an_unreachable_account_settles_as_a_failed_operation() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let started = call(
        &mut conn,
        FETCH,
        json!({
            "account": fixture::SERVER_ACCOUNT,
            "mailbox": fixture::INBOX,
            "message_id": "<only-on-the-server@example.com>",
        }),
    )
    .await;
    let id = operation_id(&started);

    let bootstrap = call(&mut conn, "state.bootstrap", json!({})).await;
    let live = bootstrap["snapshot"]["operations"]
        .as_array()
        .expect("the snapshot lists live operations");
    assert!(
        live.iter().all(|op| op["operation_id"] != json!(id))
            || live
                .iter()
                .any(|op| op["operation_id"] == json!(id) && op["method"] == json!(FETCH)),
        "an operation the snapshot lists names the method that started it: {live:?}"
    );

    let status = settle(&mut conn, &id).await;
    assert_eq!(
        status["state"], "failed",
        "an account with no credentials cannot be fetched from: {status}"
    );
    assert_eq!(status["method"], json!(FETCH));
    let error = &status["error"];
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.contains(fixture::SERVER_ACCOUNT)),
        "the failure names the account it is about: {error}"
    );
    assert_eq!(
        status["result"],
        json!(null),
        "a failed operation carries an error or a result, never both: {status}"
    );
}

// ---------------------------------------------------------------------------
// 3. message.search_server: the parameters
// ---------------------------------------------------------------------------

/// The search leg validates what a caller can get wrong before it issues an id.
///
/// **Fails at HEAD**: `-32601`.
#[tokio::test]
async fn the_server_search_validates_its_parameters() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for (what, params) in [
        ("no account", json!({"query": "ledger"})),
        ("no query", json!({"account": fixture::SERVER_ACCOUNT})),
        (
            "a query the grammar cannot parse",
            json!({"account": fixture::SERVER_ACCOUNT, "query": "from:\""}),
        ),
        (
            "a mailbox the account does not have",
            json!({
                "account": fixture::SERVER_ACCOUNT,
                "query": "ledger",
                "mailboxes": [fixture::UNKNOWN_MAILBOX],
            }),
        ),
        (
            "a limit that is not a number",
            json!({
                "account": fixture::SERVER_ACCOUNT,
                "query": "ledger",
                "limit": "fifty",
            }),
        ),
        (
            "exclusions that are not strings",
            json!({
                "account": fixture::SERVER_ACCOUNT,
                "query": "ledger",
                "exclude_message_ids": [7],
            }),
        ),
    ] {
        let error = call_err(&mut conn, SEARCH_SERVER, params).await;
        assert_eq!(error.code, -32602, "{what} is invalid params, got {error:?}");
    }

    let unknown = call_err(
        &mut conn,
        SEARCH_SERVER,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "query": "ledger"}),
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::AccountUnknown.code());
    assert_eq!(unknown.data, Some(json!({"account": fixture::UNKNOWN_ACCOUNT})));

    // An account that configures no server at all cannot run a server search,
    // and says so the way a sync of the same account says it.
    let local_only = call_err(
        &mut conn,
        SEARCH_SERVER,
        json!({"account": fixture::ACCOUNT, "query": "ledger"}),
    )
    .await;
    assert_eq!(
        local_only.code,
        ErrorCode::AccountNotReady.code(),
        "a local-only account is `-32006`, as it is for a sync: {local_only:?}"
    );
}

/// An account that cannot be reached settles the operation as `failed`,
/// leaving the local pass's hits alone.
///
/// The whole search failing rather than one mailbox failing is the right
/// answer here: the credential resolution happens once, before any mailbox is
/// selected, so there is nothing partial to report.
///
/// **Fails at HEAD**: `-32601` at the call.
#[tokio::test]
async fn a_server_search_over_an_unreachable_account_settles_as_a_failed_operation() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let started = call(
        &mut conn,
        SEARCH_SERVER,
        json!({
            "account": fixture::SERVER_ACCOUNT,
            "query": "from:ivana ledger",
            "mailboxes": [fixture::INBOX, fixture::EXTRA_MAILBOX],
            "limit": 50,
        }),
    )
    .await;
    let id = operation_id(&started);
    let status = settle(&mut conn, &id).await;

    assert_eq!(
        status["state"], "failed",
        "an account with no credentials cannot be searched: {status}"
    );
    assert_eq!(status["method"], json!(SEARCH_SERVER));
    assert!(
        status["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(fixture::SERVER_ACCOUNT)),
        "the failure names the account: {status}"
    );
}

/// The settle shape, stated as the assertion it will be the day there is a
/// server to run it against.
///
/// It is written against the *failed* operation above so it runs today: what it
/// pins is that `result` is `null` on a failure and that the key set below is
/// the one a success carries, which the doc and the fixture also carry. The
/// success path itself is NOT TESTABLE offline, and the manual check is in this
/// file's header.
///
/// **Fails at HEAD**: `-32601`.
#[tokio::test]
async fn the_server_search_settle_shape_is_the_documented_one() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let started = call(
        &mut conn,
        SEARCH_SERVER,
        json!({
            "account": fixture::SERVER_ACCOUNT,
            "query": "ledger",
            "exclude_message_ids": ["<already-shown@example.com>"],
        }),
    )
    .await;
    let id = operation_id(&started);
    let status = settle(&mut conn, &id).await;

    assert_keys(
        &status,
        &[
            "operation_id",
            "method",
            "state",
            "scope",
            "progress",
            "result",
            "error",
        ],
        "operation.status",
    );
    assert_eq!(
        status["scope"], "durable",
        "a search a client started must survive that client's socket: {status}"
    );
    match status["state"].as_str() {
        Some("succeeded") => assert_keys(
            &status["result"],
            &SEARCH_RESULT,
            "a settled message.search_server",
        ),
        Some("failed") => assert_eq!(
            status["result"],
            json!(null),
            "a failed operation carries an error and no result: {status}"
        ),
        other => panic!("a settled operation is succeeded or failed, got {other:?}"),
    }
}
