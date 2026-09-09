//! The transport-independent method dispatcher (P3a-U2).
//!
//! Everything above this module speaks a wire protocol; everything below it
//! speaks the domain. A [`Method`] receives the caller's [`ClientCtx`], its
//! parameters as JSON and a [`CancelToken`], and answers with an [`Outcome`] or
//! a [`DomainError`]. It renders nothing and asks the user nothing: the same
//! call has to serve a one-shot command line, a long-lived terminal interface
//! and a native shell, and only the caller knows how to show an answer.
//!
//! The four [`MethodKind`]s are the contract a client can rely on without
//! reading the method's code:
//!
//! - [`MethodKind::Query`] reads and changes nothing, so its outcome carries no
//!   revision and no affected resource.
//! - [`MethodKind::Command`] changes state at once, so its outcome carries the
//!   revision the change moved the daemon to and every resource it touched.
//! - [`MethodKind::Operation`] is long-running and observes its
//!   [`CancelToken`]; a cancelled one answers
//!   [`ErrorCode::OperationCancelled`] rather than a partial result.
//! - [`MethodKind::ClientIntegration`] is work only the client's own process
//!   can do (opening a browser, revealing a file), so the daemon answers with
//!   the instruction and the client carries it out.
//!
//! Cancellation is the method's answer, never the dispatcher's: a method that
//! has already committed a write must be allowed to report the write instead of
//! being reported as cancelled behind its own back.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::Notify;

use mp_protocol::{ErrorCode, Request, RpcError};

/// JSON-RPC's own "method not found".
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's own "invalid params".
const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC's own "internal error", which is also what a code too wide for the
/// wire narrows onto.
const INTERNAL_ERROR: i64 = -32603;

// ---------------------------------------------------------------------------
// The caller
// ---------------------------------------------------------------------------

/// Which kind of client is calling.
///
/// The daemon's own copy of [`mp_client::ClientKind`]: the two must agree on
/// the wire spelling, which `tests/daemon_dispatcher.rs` checks, but the daemon
/// does not depend on its client crate to name its callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClientKind {
    /// A one-shot command-line invocation.
    Cli,
    /// The terminal user interface.
    Tui,
    /// A native desktop shell.
    Gui,
}

impl ClientKind {
    /// The `client.type` string this kind travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Cli => "cli",
            ClientKind::Tui => "tui",
            ClientKind::Gui => "gui",
        }
    }

    /// The kind a `client.type` string names, or `None` for anything else.
    pub fn from_wire(value: &str) -> Option<ClientKind> {
        match value {
            "cli" => Some(ClientKind::Cli),
            "tui" => Some(ClientKind::Tui),
            "gui" => Some(ClientKind::Gui),
            _ => None,
        }
    }
}

/// Everything a method may know about the connection it is answering. Built
/// once per connection by the handshake and handed to every call on it, so a
/// method never reaches back into transport state.
#[derive(Clone, Debug)]
pub struct ClientCtx {
    /// Identifies the connection within this daemon process; also the
    /// subscriber id the event fan-out will address.
    pub connection_id: u64,
    /// What kind of client is on the other end.
    pub kind: ClientKind,
    /// The protocol version the handshake settled on.
    pub protocol: u32,
    /// The capabilities in effect on this connection.
    pub capabilities: Vec<String>,
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// What a method does to the daemon's state, and therefore what its outcome
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MethodKind {
    /// Reads state; changes nothing.
    Query,
    /// Changes state at once and reports the revision it moved to.
    Command,
    /// Runs long enough to be worth cancelling and observes its token.
    Operation,
    /// Work the client's own process performs on the daemon's instruction.
    ClientIntegration,
}

impl MethodKind {
    /// The stable identifier for logs and for the protocol documentation.
    pub fn as_str(self) -> &'static str {
        match self {
            MethodKind::Query => "query",
            MethodKind::Command => "command",
            MethodKind::Operation => "operation",
            MethodKind::ClientIntegration => "client_integration",
        }
    }
}

/// What a client's disconnect does to work that client started (P3a-U8).
///
/// The declaration the plan means by "a client disconnect does not cancel
/// durable work unless the method's `MethodSpec` declares client-scoped
/// cancellation": a field a caller reads, not a behaviour it discovers by
/// calling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelScope {
    /// The work outlives the connection that asked for it.
    Durable,
    /// The work is cancelled when its connection closes.
    ClientScoped,
}

impl CancelScope {
    /// The string this scope travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            CancelScope::Durable => "durable",
            CancelScope::ClientScoped => "client_scoped",
        }
    }

    /// The scope a wire string names, or `None` for anything else.
    pub fn from_wire(value: &str) -> Option<CancelScope> {
        match value {
            "durable" => Some(CancelScope::Durable),
            "client_scoped" => Some(CancelScope::ClientScoped),
            _ => None,
        }
    }
}

/// A method's declaration: its name, its kind, the first protocol version that
/// served it, and what a disconnect does to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodSpec {
    /// The wire name, `family.verb`.
    pub name: &'static str,
    /// What the method does to the daemon's state.
    pub kind: MethodKind,
    /// The first protocol version offering it; never below 1.
    pub since: u32,
    /// What a client's disconnect does to work this method started.
    pub cancel_scope: CancelScope,
}

impl MethodSpec {
    /// A declaration whose work outlives its client, which is what a method
    /// that never thought about cancellation means.
    pub const fn new(name: &'static str, kind: MethodKind, since: u32) -> MethodSpec {
        MethodSpec {
            name,
            kind,
            since,
            cancel_scope: CancelScope::Durable,
        }
    }

    /// The same declaration, with work that dies with the client that asked
    /// for it.
    pub const fn client_scoped(self) -> MethodSpec {
        MethodSpec {
            name: self.name,
            kind: self.kind,
            since: self.since,
            cancel_scope: CancelScope::ClientScoped,
        }
    }
}

/// One served method.
///
/// `Send + Sync` because a [`Dispatcher`] is shared by every connection task,
/// and `&'a self` in [`Method::call`] because the returned future borrows the
/// method for as long as it runs.
pub trait Method: Send + Sync {
    /// What this method declares itself to be.
    fn spec(&self) -> MethodSpec;

    /// Answer one call.
    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>>;
}

/// A resource a call touched, as `family:path`, for example `account:work` or
/// `message:work/inbox/41`. A newtype over a string rather than an enum: the
/// resources are open-ended and clients match them as prefixes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub String);

impl ResourceId {
    /// Build one from anything string-like.
    pub fn new(id: impl Into<String>) -> Self {
        ResourceId(id.into())
    }

    /// The identifier as it travels.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a successful call produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The method's `result` on the wire.
    pub result: Value,
    /// The revision the daemon moved to, `None` for a call that changed
    /// nothing.
    pub revision: Option<u64>,
    /// Every resource the call invalidated, empty for a call that changed
    /// nothing.
    pub affected: Vec<ResourceId>,
}

impl Outcome {
    /// The outcome of a read: a result, no revision, nothing invalidated.
    pub fn query(result: Value) -> Self {
        Outcome {
            result,
            revision: None,
            affected: Vec::new(),
        }
    }

    /// The outcome of a write: the revision it moved to, and the resources
    /// whose cached copies are now stale.
    pub fn command(result: Value, revision: u64, affected: Vec<ResourceId>) -> Self {
        Outcome {
            result,
            revision: Some(revision),
            affected,
        }
    }
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// A latch a caller shuts to ask a running [`MethodKind::Operation`] to stop.
///
/// Cloning gives another view of the same latch, so a session can cancel a call
/// the dispatcher is already awaiting. Shutting it is one-way, and
/// [`CancelToken::cancelled`] on an already-shut token returns at once.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    latch: Arc<Latch>,
}

#[derive(Debug, Default)]
struct Latch {
    shut: AtomicBool,
    changed: Notify,
}

impl CancelToken {
    /// A fresh, open token.
    pub fn new() -> Self {
        CancelToken::default()
    }

    /// Shut the latch and wake every waiter. Idempotent.
    pub fn cancel(&self) {
        if !self.latch.shut.swap(true, Ordering::SeqCst) {
            self.latch.changed.notify_waiters();
        }
    }

    /// Whether the latch is shut.
    pub fn is_cancelled(&self) -> bool {
        self.latch.shut.load(Ordering::SeqCst)
    }

    /// Resolve once the latch is shut, immediately if it already is.
    pub async fn cancelled(&self) {
        loop {
            // Registered before the check, so a `cancel` racing this loop
            // wakes the waiter instead of being missed between the two.
            let waiting = self.latch.changed.notified();
            if self.is_cancelled() {
                return;
            }
            waiting.await;
            if self.is_cancelled() {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A method's failure, in the domain's terms: what a JSON-RPC error carries,
/// as a value a method returns without knowing it is on a wire at all. The code
/// is `i64` here and `i32` on the wire, and [`RpcError::from`] is the only
/// place the width changes.
#[derive(Clone, Debug, PartialEq)]
pub struct DomainError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl DomainError {
    /// One error from the daemon's own table, with the payload its code fixes.
    pub fn new(code: ErrorCode, message: impl Into<String>, data: Option<Value>) -> Self {
        DomainError {
            code: i64::from(code.code()),
            message: message.into(),
            data,
        }
    }

    /// `-32602`: the parameters are missing, of the wrong type, or contradict
    /// each other. The message names what is wrong.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        DomainError::raw(INVALID_PARAMS, message, None)
    }

    /// `-32601`: no method of that name is registered.
    pub fn method_not_found(method: &str) -> Self {
        DomainError::raw(METHOD_NOT_FOUND, format!("unknown method {method}"), None)
    }

    /// `-32603`: the daemon failed at something that is its own fault.
    pub fn internal(message: impl Into<String>) -> Self {
        DomainError::raw(INTERNAL_ERROR, message, None)
    }

    /// `-32008`: the operation stopped because its token was shut.
    pub fn cancelled(operation_id: impl Into<String>) -> Self {
        let operation_id = operation_id.into();
        DomainError::new(
            ErrorCode::OperationCancelled,
            format!("operation {operation_id} was cancelled"),
            Some(json!({ "operation_id": operation_id })),
        )
    }

    /// The wire code.
    pub fn code(&self) -> i64 {
        self.code
    }

    /// The one-line human-readable summary.
    pub fn message(&self) -> String {
        self.message.clone()
    }

    /// The structured payload, when the code fixes one.
    pub fn data(&self) -> Option<Value> {
        self.data.clone()
    }

    /// The same error carrying `data`.
    ///
    /// The JSON-RPC standard codes above take no payload from their
    /// constructors, because most callers have none; the operation family has
    /// one for every refusal it makes (`{operation_id}`, `{operation_id,
    /// state}`, `{failed_at}`) and attaches it here rather than through four
    /// more constructors.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// A JSON-RPC standard code, which [`ErrorCode`] deliberately does not
    /// cover.
    fn raw(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        DomainError {
            code,
            message: message.into(),
            data,
        }
    }
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for DomainError {}

impl From<DomainError> for RpcError {
    fn from(error: DomainError) -> RpcError {
        RpcError {
            // Every code in the table fits in an `i32`; a value that does not
            // is a daemon bug, and reporting it as an internal error beats
            // truncating it into some other error's number.
            code: i32::try_from(error.code).unwrap_or(INTERNAL_ERROR as i32),
            message: error.message,
            data: error.data,
        }
    }
}

impl From<RpcError> for DomainError {
    fn from(error: RpcError) -> DomainError {
        DomainError {
            code: i64::from(error.code),
            message: error.message,
            data: error.data,
        }
    }
}

// ---------------------------------------------------------------------------
// The dispatcher
// ---------------------------------------------------------------------------

/// The table of served methods, shared by every connection.
///
/// Built once at startup and read-only afterwards, so it needs no lock.
/// Registering a name twice replaces the earlier method: the table is the
/// daemon's own, and the alternative to last-registration-wins would be a panic
/// inside a startup table.
#[derive(Default)]
pub struct Dispatcher {
    methods: BTreeMap<&'static str, Arc<dyn Method>>,
}

impl Dispatcher {
    /// An empty table.
    pub fn new() -> Self {
        Dispatcher::default()
    }

    /// Register a method under the name its own [`MethodSpec`] declares.
    pub fn register(&mut self, method: Arc<dyn Method>) {
        let name = method.spec().name;
        self.methods.insert(name, method);
    }

    /// The declaration of one method, or `None` when nothing serves that exact
    /// name.
    pub fn spec(&self, name: &str) -> Option<MethodSpec> {
        self.methods.get(name).map(|method| method.spec())
    }

    /// Every registered method's declaration, in name order.
    pub fn specs(&self) -> Vec<MethodSpec> {
        self.methods.values().map(|method| method.spec()).collect()
    }

    /// Route one request to its method, with a token nobody else holds.
    pub async fn dispatch(
        &self,
        ctx: &ClientCtx,
        request: Request,
    ) -> Result<Outcome, DomainError> {
        self.dispatch_cancellable(ctx, request, CancelToken::new())
            .await
    }

    /// Route one request to its method, under a token the caller keeps a view
    /// of, which is how a session cancels a call already in flight.
    pub async fn dispatch_cancellable(
        &self,
        ctx: &ClientCtx,
        request: Request,
        cancel: CancelToken,
    ) -> Result<Outcome, DomainError> {
        let method = match self.methods.get(request.method.as_str()) {
            Some(method) => method,
            None => return Err(DomainError::method_not_found(&request.method)),
        };
        // The token is never raced here: a method decides for itself whether a
        // cancellation beats the work it has already committed.
        method.call(ctx, request.params, cancel).await
    }
}

impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("methods", &self.methods.keys().collect::<Vec<_>>())
            .finish()
    }
}
