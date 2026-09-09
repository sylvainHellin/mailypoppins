//! The transport-independent method dispatcher (#0121, unit P3a-U1).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/dispatch.rs` exists, against the surface fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.4 (unit P3a-U1), the
//! error table in section 3.0, and the "Dispatcher" section of the source plan.
//! It does not compile against today's tree, which has no `dispatch` module at
//! all; that failure *is* the proof the contract has no stub behind it. The
//! implementer (P3a-U2) does not edit this file, they make it pass.
//!
//! # Surface under test
//!
//! ```text
//! mailypoppins::daemon::dispatch::{
//!     ClientCtx { connection_id: u64, kind: ClientKind, protocol: u32, capabilities: Vec<String> },
//!     ClientKind { Cli, Tui, Gui },
//!     MethodKind { Query, Command, Operation, ClientIntegration },
//!     MethodSpec { name: &'static str, kind: MethodKind, since: u32 },
//!     Method { fn spec(&self) -> MethodSpec;
//!              fn call<'a>(&'a self, ctx: &'a ClientCtx, params: Value, cancel: CancelToken)
//!                  -> BoxFuture<'a, Result<Outcome, DomainError>>; },
//!     Outcome { result: Value, revision: Option<u64>, affected: Vec<ResourceId> },
//!     ResourceId(String),
//!     CancelToken { new, cancel, is_cancelled, async cancelled },
//!     DomainError { code() -> i64, message() -> String, data() -> Option<Value>,
//!                   invalid_params(..), cancelled(..), Into<mp_protocol::RpcError> },
//!     Dispatcher { new, register, spec, specs, dispatch },
//! }
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! The plan fixes the signatures and leaves the semantics open. These are the
//! decisions taken here; an implementer who wants a different one needs a
//! written contract amendment, not an edit to this file.
//!
//! - **`BoxFuture` is `futures::future::BoxFuture`**, that is
//!   `Pin<Box<dyn Future<Output = T> + Send + 'a>>`. `futures` is already a
//!   direct dependency of the root crate, so the dispatcher does not define its
//!   own alias, and a `Box::pin(async move { … })` written in a client crate
//!   coerces into the trait's return type without a helper.
//! - **The daemon defines `CancelToken` itself.** `tokio-util` is not a
//!   dependency of this workspace, and adding one for a latch with four methods
//!   is not worth a new crate in the tree. The token is a shared handle:
//!   `Clone` gives another view of the same latch, so cancelling any clone
//!   cancels every clone, which is what lets a session cancel a call the
//!   dispatcher is already awaiting. `cancel()` latches, it never un-cancels,
//!   and `cancelled()` on an already-cancelled token completes immediately
//!   rather than waiting for a second `cancel()`.
//! - **`ResourceId` is a newtype over `String`**, not an enum. The resources a
//!   command touches are open-ended (`account:work`, `mailbox:work/inbox`,
//!   `message:work/inbox/41`) and P3a-U5 carries the same type in
//!   `Event::Invalidate` alongside a free-form `scope`, so an enum would have
//!   to grow a variant per phase for no gain in what a client can check.
//! - **`DomainError::code()` returns `i64`** while `mp_protocol::RpcError.code`
//!   stays `i32`. The conversion narrows, and the two must agree on the number:
//!   `i64::from(RpcError::from(e).code) == e.code()`.
//! - **Cancellation is the method's answer, not the dispatcher's.** A method
//!   that observes its token returns `DomainError::cancelled(operation_id)`,
//!   and the dispatcher propagates it unchanged: code `-32008`, `data` of
//!   `{"operation_id": …}`, exactly the error table's row. The dispatcher does
//!   not race the token itself, because a method that has already committed a
//!   store write must be allowed to report the write rather than be reported as
//!   cancelled behind its own back.
//! - **A `Query` outcome carries `revision: None` and an empty `affected`.** A
//!   query changes nothing, so it cannot move the revision and it invalidates
//!   nothing.
//! - **A `Command` outcome carries `Some(revision)` and a non-empty
//!   `affected`.** A command that changed nothing observable is a query, and a
//!   command whose `affected` is empty would leave every client's cache stale
//!   with no event to fix it.
//! - **Registering a name twice replaces the earlier method**, last
//!   registration wins, and the name appears once in `specs()`. The plan's
//!   `register` returns nothing, so rejecting a duplicate could only be a
//!   panic, and a panic inside a registration table is a worse failure mode
//!   than a shadowed test double. Rejection would need `register` to return a
//!   `Result`, which is a contract change.
//! - **`specs()` is the accessor for "every method declares a kind"**: it
//!   returns one [`MethodSpec`] per registered method. Its order is not pinned;
//!   the names in it are unique.
//! - **An unknown method is `-32601` and its message names the method.** Its
//!   `data` is not pinned: JSON-RPC fixes no payload for that code.
//! - **"The dispatcher never writes to stdout/stderr" is checked on the
//!   source**, the way `tests/architecture_boundaries.rs` checks the TUI's
//!   imports. Capturing a child process's fds would prove it for one code path
//!   on one run, and `std::io::set_output_capture` is unstable; scanning
//!   `src/daemon/dispatch.rs` for `println!`, `eprintln!`, `std::io::stdout`
//!   and friends proves it for every path, including the ones no test reaches.
//!   The same scan covers "without drawing or prompting" by rejecting `ratatui`,
//!   `crossterm` and `dialoguer`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mailypoppins::daemon::dispatch::{
    CancelScope, CancelToken, ClientCtx, ClientKind, Dispatcher, DomainError, Method, MethodKind,
    MethodSpec, Outcome, ResourceId,
};
use mp_protocol::{ErrorCode, Request, RequestId, RpcError, JSONRPC_VERSION};

/// Upper bound on any single wait in this file. Generous, because it is a
/// ceiling and never a sleep: a dispatcher that answers takes microseconds.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long the cancellable operation waits for its token before giving up and
/// answering successfully, which fails the assertion loudly instead of hanging
/// the suite on a token that never fires.
const OPERATION_PATIENCE: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the operation to publish its token.
const TICK: Duration = Duration::from_millis(5);

/// The revision the command double reports having moved the state to.
const COMMAND_REVISION: u64 = 42;

/// The resource the command double reports having touched.
const COMMAND_AFFECTED: &str = "mailbox:work/inbox";

/// The operation id the cancellable double reports in its `-32008` payload.
const OPERATION_ID: &str = "op-7f3a";

/// JSON-RPC's own codes, spelled out rather than imported: `mp_protocol`'s
/// [`ErrorCode`] deliberately covers only the daemon's own `-32009..=-32000`.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// What a method double saw when the dispatcher called it.
#[derive(Clone, Debug, PartialEq)]
struct CallRecord {
    method: &'static str,
    marker: &'static str,
    connection_id: u64,
    kind: ClientKind,
    protocol: u32,
    capabilities: Vec<String>,
    params: Value,
}

/// Shared call log, so a test can assert a method was reached, once, with the
/// context and params the client sent.
type Log = Arc<Mutex<Vec<CallRecord>>>;

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn records(log: &Log) -> Vec<CallRecord> {
    log.lock().expect("call log not poisoned").clone()
}

/// A [`MethodKind::Query`] that records its call and validates its params.
///
/// `marker` travels back in the result, which is how the duplicate-registration
/// test tells the two registrations apart.
struct RecordingQuery {
    name: &'static str,
    marker: &'static str,
    log: Log,
}

impl Method for RecordingQuery {
    fn spec(&self) -> MethodSpec {
        MethodSpec {
            name: self.name,
            kind: MethodKind::Query,
            since: 1,
            cancel_scope: CancelScope::Durable,
        }
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            record(&self.log, self.name, self.marker, ctx, &params);
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| DomainError::invalid_params("name is a required string parameter"))?
                .to_string();
            Ok(Outcome {
                result: json!({"name": name, "marker": self.marker}),
                revision: None,
                affected: Vec::new(),
            })
        })
    }
}

/// A [`MethodKind::Command`] that reports a revision and one affected resource.
struct TouchCommand {
    log: Log,
}

impl Method for TouchCommand {
    fn spec(&self) -> MethodSpec {
        MethodSpec {
            name: "test.command",
            kind: MethodKind::Command,
            since: 1,
            cancel_scope: CancelScope::Durable,
        }
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            record(&self.log, "test.command", "command", ctx, &params);
            Ok(Outcome {
                result: json!({"touched": true}),
                revision: Some(COMMAND_REVISION),
                affected: vec![ResourceId::new(COMMAND_AFFECTED)],
            })
        })
    }
}

/// A [`MethodKind::Operation`] that publishes the token the dispatcher handed
/// it, waits on it, and answers `-32008` once it fires.
///
/// If the token never fires it answers successfully after
/// [`OPERATION_PATIENCE`], so a dispatcher that hands out a dead token fails the
/// assertion instead of hanging the suite.
struct CancellableOperation {
    published: Arc<Mutex<Option<CancelToken>>>,
    log: Log,
}

impl Method for CancellableOperation {
    fn spec(&self) -> MethodSpec {
        MethodSpec {
            name: "test.operation",
            kind: MethodKind::Operation,
            since: 1,
            cancel_scope: CancelScope::Durable,
        }
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            record(&self.log, "test.operation", "operation", ctx, &params);
            *self.published.lock().expect("token slot not poisoned") = Some(cancel.clone());
            match tokio::time::timeout(OPERATION_PATIENCE, cancel.cancelled()).await {
                Ok(_) => Err(DomainError::cancelled(OPERATION_ID)),
                Err(_) => Ok(Outcome {
                    result: json!({"cancelled": false}),
                    revision: Some(COMMAND_REVISION),
                    affected: Vec::new(),
                }),
            }
        })
    }
}

/// A [`MethodKind::ClientIntegration`] method, present so that every variant of
/// [`MethodKind`] is exercised by a registration.
struct OpenInBrowser;

impl Method for OpenInBrowser {
    fn spec(&self) -> MethodSpec {
        MethodSpec {
            name: "test.integration",
            kind: MethodKind::ClientIntegration,
            since: 1,
            cancel_scope: CancelScope::Durable,
        }
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        _params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            Ok(Outcome {
                result: json!({"handled_by": "client"}),
                revision: None,
                affected: Vec::new(),
            })
        })
    }
}

fn record(log: &Log, method: &'static str, marker: &'static str, ctx: &ClientCtx, params: &Value) {
    log.lock().expect("call log not poisoned").push(CallRecord {
        method,
        marker,
        connection_id: ctx.connection_id,
        kind: ctx.kind,
        protocol: ctx.protocol,
        capabilities: ctx.capabilities.clone(),
        params: params.clone(),
    });
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The client context every test dispatches under: connection 7, a TUI, on the
/// only protocol version this build speaks.
fn ctx() -> ClientCtx {
    ClientCtx {
        connection_id: 7,
        kind: ClientKind::Tui,
        protocol: 1,
        capabilities: vec!["account.list".to_string(), "message.list".to_string()],
    }
}

fn request(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: Some(RequestId::Num(1)),
        method: method.to_string(),
        params,
    }
}

/// A dispatcher with one method of every kind registered, plus the call log
/// they all write to.
fn populated() -> (Dispatcher, Log) {
    let log = log();
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(Arc::new(RecordingQuery {
        name: "test.query",
        marker: "query",
        log: log.clone(),
    }));
    dispatcher.register(Arc::new(TouchCommand { log: log.clone() }));
    dispatcher.register(Arc::new(CancellableOperation {
        published: Arc::new(Mutex::new(None)),
        log: log.clone(),
    }));
    dispatcher.register(Arc::new(OpenInBrowser));
    (dispatcher, log)
}

/// `specs()` keyed by name, which also proves the names are unique.
fn specs_by_name(dispatcher: &Dispatcher) -> BTreeMap<&'static str, MethodSpec> {
    let specs = dispatcher.specs();
    let by_name: BTreeMap<&'static str, MethodSpec> =
        specs.iter().map(|spec| (spec.name, *spec)).collect();
    assert_eq!(
        by_name.len(),
        specs.len(),
        "Dispatcher::specs() lists a name twice: {:?}",
        specs.iter().map(|s| s.name).collect::<Vec<_>>()
    );
    by_name
}

// ---------------------------------------------------------------------------
// Registration and lookup
// ---------------------------------------------------------------------------

#[test]
fn register_then_spec_finds_every_registered_method() {
    let (dispatcher, _log) = populated();

    let query = dispatcher
        .spec("test.query")
        .expect("test.query registered");
    assert_eq!(query.name, "test.query");
    assert_eq!(query.kind, MethodKind::Query);
    assert_eq!(query.since, 1);

    assert_eq!(
        dispatcher.spec("test.command").expect("registered").kind,
        MethodKind::Command
    );
    assert_eq!(
        dispatcher.spec("test.operation").expect("registered").kind,
        MethodKind::Operation
    );
    assert_eq!(
        dispatcher
            .spec("test.integration")
            .expect("registered")
            .kind,
        MethodKind::ClientIntegration
    );
}

#[test]
fn spec_of_an_unregistered_name_is_none() {
    let (dispatcher, _log) = populated();
    assert!(dispatcher.spec("test.unknown").is_none());
    assert!(dispatcher.spec("").is_none());
    assert!(
        dispatcher.spec("TEST.QUERY").is_none(),
        "method names are matched exactly, not case-insensitively"
    );
}

#[test]
fn an_empty_dispatcher_knows_nothing() {
    let dispatcher = Dispatcher::new();
    assert!(dispatcher.specs().is_empty());
    assert!(dispatcher.spec("test.query").is_none());
}

#[test]
fn every_registered_method_declares_a_kind() {
    let (dispatcher, _log) = populated();
    let by_name = specs_by_name(&dispatcher);

    assert_eq!(by_name.len(), 4, "four methods were registered");
    let kinds: Vec<MethodKind> = by_name.values().map(|spec| spec.kind).collect();
    for kind in [
        MethodKind::Query,
        MethodKind::Command,
        MethodKind::Operation,
        MethodKind::ClientIntegration,
    ] {
        assert!(
            kinds.contains(&kind),
            "{kind:?} is missing from Dispatcher::specs(): {kinds:?}"
        );
    }

    // `spec(name)` and `specs()` are two views of one table.
    for (name, spec) in &by_name {
        let looked_up = dispatcher.spec(name).expect("specs() named it");
        assert_eq!(looked_up.name, spec.name);
        assert_eq!(looked_up.kind, spec.kind);
        assert_eq!(looked_up.since, spec.since);
        assert!(
            spec.since >= 1,
            "{name} declares since {}, below the first protocol version",
            spec.since
        );
    }
}

#[tokio::test]
async fn registering_a_name_twice_replaces_the_earlier_method() {
    let log = log();
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(Arc::new(RecordingQuery {
        name: "test.query",
        marker: "first",
        log: log.clone(),
    }));
    dispatcher.register(Arc::new(RecordingQuery {
        name: "test.query",
        marker: "second",
        log: log.clone(),
    }));

    assert_eq!(
        dispatcher.specs().len(),
        1,
        "a replaced method leaves one entry, not two"
    );

    let outcome = dispatch(&dispatcher, "test.query", json!({"name": "ada"}))
        .await
        .expect("the surviving method answers");
    assert_eq!(
        outcome.result["marker"], "second",
        "the last registration wins"
    );

    let seen = records(&log);
    assert_eq!(seen.len(), 1, "only one method was called: {seen:?}");
    assert_eq!(seen[0].marker, "second");
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Run one dispatch under [`DEADLINE`], so a wedged implementation fails
/// instead of hanging the suite.
async fn dispatch(
    dispatcher: &Dispatcher,
    method: &str,
    params: Value,
) -> Result<Outcome, DomainError> {
    let ctx = ctx();
    tokio::time::timeout(DEADLINE, dispatcher.dispatch(&ctx, request(method, params)))
        .await
        .unwrap_or_else(|_| panic!("{method} did not answer within {DEADLINE:?}"))
}

#[tokio::test]
async fn dispatch_hands_the_method_the_client_context_and_params() {
    let (dispatcher, log) = populated();

    let outcome = dispatch(&dispatcher, "test.query", json!({"name": "ada"}))
        .await
        .expect("test.query answers");
    assert_eq!(outcome.result, json!({"name": "ada", "marker": "query"}));

    let seen = records(&log);
    assert_eq!(seen.len(), 1, "exactly one method ran: {seen:?}");
    assert_eq!(
        seen[0],
        CallRecord {
            method: "test.query",
            marker: "query",
            connection_id: 7,
            kind: ClientKind::Tui,
            protocol: 1,
            capabilities: vec!["account.list".to_string(), "message.list".to_string()],
            params: json!({"name": "ada"}),
        },
        "the dispatcher passes the context and the params through unchanged"
    );
}

#[tokio::test]
async fn an_unknown_method_is_method_not_found() {
    let (dispatcher, log) = populated();

    let error = dispatch(&dispatcher, "test.unknown", json!({}))
        .await
        .expect_err("an unregistered method is an error");

    assert_eq!(error.code(), METHOD_NOT_FOUND);
    assert!(
        error.message().contains("test.unknown"),
        "the message names the method that was not found: {}",
        error.message()
    );
    assert!(
        records(&log).is_empty(),
        "no registered method runs for an unknown name"
    );
}

#[tokio::test]
async fn bad_params_are_invalid_params() {
    let (dispatcher, log) = populated();

    let error = dispatch(&dispatcher, "test.query", json!({}))
        .await
        .expect_err("the required parameter is missing");

    assert_eq!(error.code(), INVALID_PARAMS);
    assert!(
        error.message().contains("name"),
        "the message names the parameter: {}",
        error.message()
    );
    assert_eq!(
        records(&log).len(),
        1,
        "the dispatcher routes first and lets the method validate"
    );
}

#[tokio::test]
async fn a_query_outcome_carries_no_revision() {
    let (dispatcher, _log) = populated();

    let outcome = dispatch(&dispatcher, "test.query", json!({"name": "ada"}))
        .await
        .expect("test.query answers");

    assert_eq!(
        outcome.revision, None,
        "a query changes nothing, so it moves no revision"
    );
    assert!(
        outcome.affected.is_empty(),
        "a query invalidates nothing: {:?}",
        outcome.affected
    );
}

#[tokio::test]
async fn a_command_outcome_carries_a_revision_and_the_resources_it_touched() {
    let (dispatcher, _log) = populated();

    let outcome = dispatch(&dispatcher, "test.command", json!({}))
        .await
        .expect("test.command answers");

    assert_eq!(outcome.revision, Some(COMMAND_REVISION));
    assert!(
        !outcome.affected.is_empty(),
        "a command names at least one affected resource"
    );
    assert_eq!(
        outcome.affected,
        vec![ResourceId::new(COMMAND_AFFECTED)],
        "the affected list reaches the caller unchanged"
    );
    assert_eq!(outcome.affected[0].as_str(), COMMAND_AFFECTED);
    assert_eq!(outcome.result, json!({"touched": true}));
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn a_cancel_token_starts_open_and_latches_shut() {
    let token = CancelToken::new();
    assert!(!token.is_cancelled());

    let clone = token.clone();
    assert!(!clone.is_cancelled());

    clone.cancel();
    assert!(
        token.is_cancelled(),
        "a clone is another view of the same latch, not a copy of its state"
    );

    clone.cancel();
    assert!(
        token.is_cancelled(),
        "cancelling twice is not un-cancelling"
    );
}

#[tokio::test]
async fn cancelled_completes_immediately_on_an_already_cancelled_token() {
    let token = CancelToken::new();
    token.cancel();
    tokio::time::timeout(DEADLINE, token.cancelled())
        .await
        .expect("cancelled() on a cancelled token does not wait for a second cancel()");
}

#[tokio::test]
async fn a_cancelled_operation_is_operation_cancelled() {
    let published: Arc<Mutex<Option<CancelToken>>> = Arc::new(Mutex::new(None));
    let log = log();
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(Arc::new(CancellableOperation {
        published: published.clone(),
        log: log.clone(),
    }));

    let ctx = ctx();
    let call = dispatcher.dispatch(&ctx, request("test.operation", json!({})));
    let canceller = async {
        loop {
            let token = published.lock().expect("token slot not poisoned").clone();
            match token {
                Some(token) => {
                    token.cancel();
                    return;
                }
                None => tokio::time::sleep(TICK).await,
            }
        }
    };

    let (result, ()) = tokio::time::timeout(DEADLINE, futures::future::join(call, canceller))
        .await
        .expect("the operation answered once its token fired");

    let error = result.expect_err("a cancelled operation does not succeed");
    assert_eq!(
        error.code(),
        i64::from(ErrorCode::OperationCancelled.code())
    );
    assert_eq!(error.code(), -32008);
    assert_eq!(
        error.data(),
        Some(json!({"operation_id": OPERATION_ID})),
        "the -32008 payload is the error table's {{operation_id}}"
    );
    assert_eq!(records(&log).len(), 1, "the operation ran once");
}

// ---------------------------------------------------------------------------
// Errors on the wire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_domain_error_narrows_onto_the_rpc_error_table() {
    let (dispatcher, _log) = populated();
    let error = dispatch(&dispatcher, "test.unknown", json!({}))
        .await
        .expect_err("an unregistered method is an error");

    // `code()` is `i64` while `RpcError.code` is `i32`; the two agree on the
    // number and the conversion is the only place the width changes.
    let code: i64 = error.code();
    let rpc: RpcError = RpcError::from(error.clone());
    assert_eq!(i64::from(rpc.code), code);
    assert_eq!(rpc.message, error.message());
    assert_eq!(rpc.data, error.data());
}

#[test]
fn the_error_constructors_carry_the_table_codes() {
    let invalid = DomainError::invalid_params("name is a required string parameter");
    assert_eq!(invalid.code(), INVALID_PARAMS);
    assert_eq!(invalid.message(), "name is a required string parameter");

    let cancelled = DomainError::cancelled(OPERATION_ID);
    assert_eq!(
        cancelled.code(),
        i64::from(ErrorCode::OperationCancelled.code())
    );
    assert_eq!(
        cancelled.data(),
        Some(json!({"operation_id": OPERATION_ID}))
    );

    let rpc: RpcError = cancelled.into();
    assert_eq!(rpc.code, ErrorCode::OperationCancelled.code());
    assert!(
        !rpc.message.is_empty(),
        "every error carries a human-readable line for the log and the CLI"
    );
}

#[test]
fn the_daemon_and_the_client_spell_a_client_kind_the_same_way() {
    for (daemon, client) in [
        (ClientKind::Cli, mp_client::ClientKind::Cli),
        (ClientKind::Tui, mp_client::ClientKind::Tui),
        (ClientKind::Gui, mp_client::ClientKind::Gui),
    ] {
        assert_eq!(
            daemon.as_str(),
            client.as_str(),
            "the kind the handshake sends and the kind the dispatcher sees are one string"
        );
    }
}

// ---------------------------------------------------------------------------
// The dispatcher is silent
// ---------------------------------------------------------------------------

/// Macros and paths that write to the process's own streams. A dispatcher that
/// contains one of these has a code path that prints, which a GUI client cannot
/// see and a socket client would receive as corruption if the daemon ever ran
/// its protocol over stdio.
const WRITES_TO_TERMINAL: [&str; 8] = [
    "println!",
    "eprintln!",
    "print!",
    "eprint!",
    "dbg!",
    "io::stdout",
    "io::stderr",
    "io::stdin",
];

/// Crates that draw, prompt, or colour. The dispatcher returns typed results;
/// presentation is the client's business.
const DRAWS_OR_PROMPTS: [&str; 4] = ["ratatui", "crossterm", "dialoguer", "colored"];

#[test]
fn the_dispatcher_source_never_writes_to_stdout_or_stderr() {
    let source = dispatch_source();
    for needle in WRITES_TO_TERMINAL {
        assert!(
            !source.contains(needle),
            "src/daemon/dispatch.rs contains `{needle}`: the dispatcher returns typed \
             results and domain errors, it never writes to the process streams"
        );
    }
}

#[test]
fn the_dispatcher_source_never_draws_or_prompts() {
    let source = dispatch_source();
    for needle in DRAWS_OR_PROMPTS {
        assert!(
            !source.contains(needle),
            "src/daemon/dispatch.rs mentions `{needle}`: the dispatcher neither draws \
             nor prompts, so a GUI and a CLI can share it"
        );
    }
}

/// `src/daemon/dispatch.rs` with its `//` comments removed, so that a doc
/// comment saying "never `println!`" does not fail the scan it describes.
/// Block comments are left alone, as in `tests/architecture_boundaries.rs`.
fn dispatch_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/dispatch.rs");
    let source = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("the dispatcher lives at src/daemon/dispatch.rs (plan unit P3a-U2): {path:?}: {e}")
    });
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
