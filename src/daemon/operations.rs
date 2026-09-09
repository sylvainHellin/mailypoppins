//! Long-running operations: the registry, its state machine, and the two
//! methods a client watches one with (P3a-U8).
//!
//! An operation is deliberately **not** part of the state a client mirrors. The
//! snapshot's `operations` array is a projection of this registry, the registry
//! has its own identifiers and its own lifetime, and it is reached both from a
//! method (`operation.cancel`) and from the connection loop (a disconnect)
//! without either going through the canonical state's gate.
//!
//! # The machine
//!
//! `Queued -> Running -> {Succeeded, Failed, Cancelled}`, plus
//! `Queued -> {Succeeded, Failed, Cancelled}` for work that finishes before it
//! reports anything. Every other transition is a silent no-op: the loser of the
//! race is a worker that was already told to stop, and neither a panic nor a
//! second finished event is an answer to that. A [`OperationHandle::report`] on
//! a queued operation moves it to `Running`, because a report is evidence of
//! running.
//!
//! Only two things publish: a report and the terminal transition.
//! [`OperationRegistry::start`] and [`OperationHandle::set_running`] publish
//! nothing, because a client learns that an operation exists from the answer
//! that carried its id.
//!
//! [`OperationRegistry::cancel`] is synchronous and terminal at once: it shuts
//! the token, sets `Cancelled` and publishes the finished event before it
//! returns, so the `operation.status` a client calls straight afterwards
//! already agrees. The worker's later `succeed` is the ignored invalid
//! transition above.
//!
//! # The fan-out
//!
//! Operation events are [`Event::Lifecycle`] events and reach every
//! bootstrapped connection, not only the owner's: an operation is daemon-wide,
//! and a GUI that started a sync must be watchable from the CLI window beside
//! it. The registry never stamps a revision - revisions belong to the canonical
//! state - so it queues its events and calls the fan-out installed by
//! [`OperationRegistry::set_fanout`], which drains them with
//! [`OperationRegistry::drain_events`] and publishes each one at the next
//! canonical revision. With no fan-out installed the events simply accumulate
//! for the next drain, which is what makes the machine testable in process.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{json, Value};

use mp_protocol::ErrorCode;

use super::dispatch::{
    CancelScope, CancelToken, ClientCtx, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::state::events::Event;
use super::state::{CanonicalState, ConnectionId};

/// Test-only hook: register the fake `test.operation` method, so the wire half
/// of the operation contract is reachable while Phase 3a still has no real
/// long-running method (sync, auth and the rebuilds all arrive in Phase 5).
///
/// On the [`FAKE_EVENT_BURST_ENV`](super::state::events::FAKE_EVENT_BURST_ENV)
/// precedent: no flag exposes it and `mp --help` never moves. Documented in
/// `docs/daemon-operations.md` beside the other hooks.
pub const FAKE_OPERATIONS_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_OPERATIONS";

/// The kind one progress report travels as.
pub const KIND_OPERATION_PROGRESS: &str = "operation.progress";

/// The kind a terminal transition travels as.
pub const KIND_OPERATION_FINISHED: &str = "operation.finished";

/// The method that reports one operation.
pub const METHOD_OPERATION_STATUS: &str = "operation.status";

/// The method that stops one operation.
pub const METHOD_OPERATION_CANCEL: &str = "operation.cancel";

/// The wire name of the fake method [`FAKE_OPERATIONS_ENV`] registers.
const TEST_OPERATION: &str = "test.operation";

/// Whether this process serves the fake operation method.
pub fn fake_operations() -> bool {
    super::lifecycle::env_flag(FAKE_OPERATIONS_ENV)
}

// ---------------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------------

/// One operation's identifier: opaque, non-empty, and unique for the life of
/// the process. A client echoes it and never parses it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationId(pub String);

impl OperationId {
    /// Build one from anything string-like.
    pub fn new(id: impl Into<String>) -> Self {
        OperationId(id.into())
    }

    /// The identifier as it travels.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The five states of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    /// Accepted, not started.
    Queued,
    /// Working.
    Running,
    /// Finished with a result.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// Stopped, by a client or by its owner's disconnect.
    Cancelled,
}

impl OperationState {
    /// The string this state travels as.
    pub fn as_str(self) -> &'static str {
        match self {
            OperationState::Queued => "queued",
            OperationState::Running => "running",
            OperationState::Succeeded => "succeeded",
            OperationState::Failed => "failed",
            OperationState::Cancelled => "cancelled",
        }
    }

    /// The state a wire string names, or `None` for anything else.
    pub fn from_wire(value: &str) -> Option<OperationState> {
        match value {
            "queued" => Some(OperationState::Queued),
            "running" => Some(OperationState::Running),
            "succeeded" => Some(OperationState::Succeeded),
            "failed" => Some(OperationState::Failed),
            "cancelled" => Some(OperationState::Cancelled),
            _ => None,
        }
    }

    /// Whether nothing can move this operation any further.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
        )
    }
}

/// One progress report, as the plan fixes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    /// What the operation is doing now.
    pub phase: String,
    /// How much of it is done.
    pub done: u64,
    /// How much there is, `None` when the total is not known yet.
    pub total: Option<u64>,
    /// One line for the user, when there is one.
    pub message: Option<String>,
}

impl Progress {
    /// The four members, with `null` rather than an absent key for the two that
    /// may have nothing to say: a client reads `total` to draw a bar and has to
    /// tell "unknown" from "missing field".
    pub fn to_json(&self) -> Value {
        json!({
            "phase": self.phase,
            "done": self.done,
            "total": self.total,
            "message": self.message,
        })
    }
}

/// Everything the registry knows about one operation.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationStatus {
    /// Its identifier.
    pub id: OperationId,
    /// The method that started it.
    pub method: &'static str,
    /// The connection that started it, which decides what a disconnect
    /// cancels and nothing else.
    pub owner: ConnectionId,
    /// What a disconnect of that connection does to it.
    pub scope: CancelScope,
    /// Where it is in the machine.
    pub state: OperationState,
    /// The newest report, kept after the finish so a client that missed it can
    /// still read it.
    pub progress: Option<Progress>,
    /// What a succeeded operation produced.
    pub result: Option<Value>,
    /// Why a failed or cancelled operation stopped.
    pub error: Option<DomainError>,
}

impl OperationStatus {
    /// The `result` of `operation.status`, and one entry of the bootstrap
    /// snapshot's `operations` array. Fixed here rather than in either caller,
    /// because two callers render it and they may not disagree.
    ///
    /// `owner` is not on the wire: it is a connection id inside this daemon,
    /// which no client can address and none may branch on.
    pub fn to_json(&self) -> Value {
        json!({
            "operation_id": self.id.as_str(),
            "method": self.method,
            "state": self.state.as_str(),
            "scope": self.scope.as_str(),
            "progress": self.progress.as_ref().map(Progress::to_json),
            "result": self.result,
            "error": self.error.as_ref().map(error_json),
        })
    }
}

/// One error as the JSON-RPC object an event and a status both carry. `data` is
/// omitted rather than nulled, as everywhere else in this protocol.
fn error_json(error: &DomainError) -> Value {
    match error.data() {
        Some(data) => json!({"code": error.code(), "message": error.message(), "data": data}),
        None => json!({"code": error.code(), "message": error.message()}),
    }
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// The worker's end of one operation: the token it observes and the four
/// transitions it may ask for.
#[derive(Clone, Debug)]
pub struct OperationHandle {
    /// Shut when the operation is cancelled, which is how a worker learns to
    /// stop.
    pub token: CancelToken,
    id: OperationId,
    shared: Arc<Shared>,
}

impl OperationHandle {
    /// The operation this handle drives.
    pub fn id(&self) -> OperationId {
        self.id.clone()
    }

    /// Announce that the work has started. Publishes nothing.
    pub fn set_running(&self) {
        self.shared.mutate(&self.id, |status| {
            if status.state == OperationState::Queued {
                status.state = OperationState::Running;
            }
            None
        });
    }

    /// Report progress, which also moves a queued operation to running.
    pub fn report(&self, progress: Progress) {
        self.shared.mutate(&self.id, |status| {
            if status.state.is_terminal() {
                return None;
            }
            status.state = OperationState::Running;
            let event = Event::Lifecycle {
                kind: KIND_OPERATION_PROGRESS,
                payload: progress_payload(&status.id, &progress),
            };
            status.progress = Some(progress);
            Some(event)
        });
    }

    /// Finish with a result.
    pub fn succeed(&self, result: Value) {
        self.shared.mutate(&self.id, |status| {
            if status.state.is_terminal() {
                return None;
            }
            status.state = OperationState::Succeeded;
            status.result = Some(result);
            Some(finished_event(status))
        });
    }

    /// Finish with an error.
    pub fn fail(&self, error: DomainError) {
        self.shared.mutate(&self.id, |status| {
            if status.state.is_terminal() {
                return None;
            }
            status.state = OperationState::Failed;
            status.error = Some(error);
            Some(finished_event(status))
        });
    }
}

/// One progress event's payload: the operation and the report's four members.
fn progress_payload(id: &OperationId, progress: &Progress) -> Value {
    let mut payload = progress.to_json();
    payload["operation_id"] = json!(id.as_str());
    payload
}

/// One terminal event's payload, which carries `result` exclusive-or `error`.
fn finished_event(status: &OperationStatus) -> Event {
    let mut payload = json!({
        "operation_id": status.id.as_str(),
        "state": status.state.as_str(),
    });
    match (&status.result, &status.error) {
        (Some(result), _) => payload["result"] = result.clone(),
        (None, Some(error)) => payload["error"] = error_json(error),
        (None, None) => {}
    }
    Event::Lifecycle {
        kind: KIND_OPERATION_FINISHED,
        payload,
    }
}

/// The table of live operations, shared by the registry and every handle it
/// has issued.
#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    fanout: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Ids in start order, which is the order a disconnect cancels in.
    order: Vec<OperationId>,
    entries: HashMap<OperationId, Entry>,
    pending: VecDeque<Event>,
}

#[derive(Debug)]
struct Entry {
    status: OperationStatus,
    token: CancelToken,
}

impl Shared {
    /// Apply one transition and publish whatever it produced.
    ///
    /// The event is queued under the lock, in emission order, and the fan-out
    /// runs after the lock is released, so nothing it calls can re-enter this
    /// table.
    fn mutate(&self, id: &OperationId, change: impl FnOnce(&mut OperationStatus) -> Option<Event>) {
        {
            let mut inner = lock(&self.inner);
            let Some(entry) = inner.entries.get_mut(id) else {
                return;
            };
            match change(&mut entry.status) {
                Some(event) => inner.pending.push_back(event),
                None => return,
            }
        }
        self.fan_out();
    }

    /// Hand the queued events to whatever publishes them.
    fn fan_out(&self) {
        let fanout = lock(&self.fanout).clone();
        if let Some(fanout) = fanout {
            fanout();
        }
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("operations", &lock(&self.inner).order.len())
            .finish_non_exhaustive()
    }
}

/// Every operation this daemon process has started, live or finished.
#[derive(Debug, Default)]
pub struct OperationRegistry {
    shared: Arc<Shared>,
}

impl OperationRegistry {
    /// An empty registry with no fan-out installed.
    pub fn new() -> Self {
        OperationRegistry::default()
    }

    /// Install the fan-out, once, at daemon startup.
    ///
    /// It is called on the thread that emitted the events, after they were
    /// queued, and is expected to drain them with
    /// [`OperationRegistry::drain_events`] and publish each one.
    pub fn set_fanout(&self, fanout: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.shared.fanout) = Some(fanout);
    }

    /// Register one operation, queued, and hand back the worker's end of it.
    ///
    /// Publishes nothing: the answer that carries the id is the announcement.
    pub fn start(
        &self,
        owner: ConnectionId,
        scope: CancelScope,
        method: &'static str,
    ) -> (OperationId, OperationHandle) {
        // 128 random bits as hex, the generator `daemon.json`'s instance id
        // already uses: opaque, collision-free in practice, and carrying
        // nothing about the daemon that issued it.
        let id = OperationId::new(format!("{:032x}", rand::random::<u128>()));
        let token = CancelToken::new();
        let status = OperationStatus {
            id: id.clone(),
            method,
            owner,
            scope,
            state: OperationState::Queued,
            progress: None,
            result: None,
            error: None,
        };
        {
            let mut inner = lock(&self.shared.inner);
            inner.order.push(id.clone());
            inner.entries.insert(
                id.clone(),
                Entry {
                    status,
                    token: token.clone(),
                },
            );
        }
        (
            id.clone(),
            OperationHandle {
                token,
                id,
                shared: Arc::clone(&self.shared),
            },
        )
    }

    /// What the registry knows about one operation, or `None` for an id it
    /// never issued.
    pub fn status(&self, id: &OperationId) -> Option<OperationStatus> {
        lock(&self.shared.inner)
            .entries
            .get(id)
            .map(|entry| entry.status.clone())
    }

    /// Every operation that has not settled, in start order: the bootstrap
    /// snapshot's `operations` array.
    pub fn live(&self) -> Vec<OperationStatus> {
        let inner = lock(&self.shared.inner);
        inner
            .order
            .iter()
            .filter_map(|id| inner.entries.get(id))
            .filter(|entry| !entry.status.state.is_terminal())
            .map(|entry| entry.status.clone())
            .collect()
    }

    /// Stop one operation, now.
    ///
    /// A terminal operation is `-32602` naming the state that made the cancel
    /// impossible, and an unknown id is `-32602` naming the id: the caller
    /// asked for something impossible about something that exists, or for
    /// something that does not. `-32008` is the operation's own answer to its
    /// caller and may not also mean "you cannot cancel that".
    pub fn cancel(&self, id: &OperationId) -> Result<(), DomainError> {
        let cancelled = {
            let mut inner = lock(&self.shared.inner);
            let Some(entry) = inner.entries.get_mut(id) else {
                return Err(DomainError::invalid_params(format!(
                    "no operation {id} is known to this daemon"
                ))
                .with_data(json!({"operation_id": id.as_str()})));
            };
            if entry.status.state.is_terminal() {
                let state = entry.status.state.as_str();
                return Err(DomainError::invalid_params(format!(
                    "operation {id} has already {state}"
                ))
                .with_data(json!({"operation_id": id.as_str(), "state": state})));
            }
            settle_cancelled(entry)
        };
        lock(&self.shared.inner).pending.push_back(cancelled);
        self.shared.fan_out();
        Ok(())
    }

    /// Cancel every non-terminal client-scoped operation this connection
    /// started, in start order, and leave everything else alone: its own
    /// durable work, another connection's of either scope, and its own
    /// finished operations.
    pub fn on_disconnect(&self, conn: ConnectionId) {
        let cancelled = {
            let mut inner = lock(&self.shared.inner);
            let doomed: Vec<OperationId> = inner
                .order
                .iter()
                .filter(|id| {
                    inner.entries.get(*id).is_some_and(|entry| {
                        entry.status.owner == conn
                            && entry.status.scope == CancelScope::ClientScoped
                            && !entry.status.state.is_terminal()
                    })
                })
                .cloned()
                .collect();
            let mut events = Vec::new();
            for id in doomed {
                if let Some(entry) = inner.entries.get_mut(&id) {
                    events.push(settle_cancelled(entry));
                }
            }
            events
        };
        if cancelled.is_empty() {
            return;
        }
        lock(&self.shared.inner).pending.extend(cancelled);
        self.shared.fan_out();
    }

    /// Everything emitted since the last call, oldest first, leaving the queue
    /// empty. A hand-off, not a log: a drained event is not delivered twice.
    pub fn drain_events(&self) -> Vec<Event> {
        lock(&self.shared.inner).pending.drain(..).collect()
    }
}

/// Shut one operation's token, settle it, and produce its finished event. A
/// cancellation from a disconnect is indistinguishable from an explicit one.
fn settle_cancelled(entry: &mut Entry) -> Event {
    entry.token.cancel();
    entry.status.state = OperationState::Cancelled;
    entry.status.error = Some(DomainError::new(
        ErrorCode::OperationCancelled,
        format!("operation {} was cancelled", entry.status.id),
        Some(json!({"operation_id": entry.status.id.as_str()})),
    ));
    finished_event(&entry.status)
}

/// Lock, recovering from a poisoned mutex: every section between a lock and its
/// release is a handful of infallible field writes, so a panic elsewhere may
/// not wedge the whole operation table.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// The methods
// ---------------------------------------------------------------------------

/// The required `operation_id`, or `-32602` naming what was asked for.
fn operation_id(params: &Value) -> Result<OperationId, DomainError> {
    params
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(OperationId::new)
        .ok_or_else(|| {
            DomainError::invalid_params("operation_id is a required non-empty string parameter")
        })
}

/// `operation.status`: what the registry knows about one operation.
///
/// A [`MethodKind::Query`], and daemon-wide: any initialized connection may ask
/// about any live id, because the daemon serves one user's data directory and a
/// GUI that started a sync must be watchable from the CLI window beside it.
pub struct OperationStatusMethod {
    /// The one registry of this daemon process.
    pub registry: Arc<OperationRegistry>,
}

impl Method for OperationStatusMethod {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new(METHOD_OPERATION_STATUS, MethodKind::Query, 1)
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let id = operation_id(&params)?;
            let status = self.registry.status(&id).ok_or_else(|| {
                DomainError::invalid_params(format!("no operation {id} is known to this daemon"))
                    .with_data(json!({"operation_id": id.as_str()}))
            })?;
            Ok(Outcome::query(status.to_json()))
        })
    }
}

/// `operation.cancel`: stop one operation and say so.
///
/// A [`MethodKind::Command`], because it settles an operation the whole daemon
/// can see. Its revision is the canonical state's after the finished event was
/// published, which the registry's fan-out does synchronously inside
/// [`OperationRegistry::cancel`], so the number this outcome reports is the one
/// that event travelled at.
pub struct OperationCancelMethod {
    /// The one registry of this daemon process.
    pub registry: Arc<OperationRegistry>,
    /// The state whose revisions the fan-out stamps events with.
    pub canonical: Arc<CanonicalState>,
}

impl Method for OperationCancelMethod {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new(METHOD_OPERATION_CANCEL, MethodKind::Command, 1)
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let id = operation_id(&params)?;
            self.registry.cancel(&id)?;
            Ok(Outcome::command(
                json!({"operation_id": id.as_str(), "state": OperationState::Cancelled.as_str()}),
                self.canonical.revision().get(),
                vec![ResourceId::new(format!("operation:{id}"))],
            ))
        })
    }
}

/// The fake long-running method [`FAKE_OPERATIONS_ENV`] registers.
///
/// It answers immediately with the id and runs the work on a spawned task,
/// which is what an operation method does. `scope` is the one parameter a real
/// method would not take: a real one passes its own `spec().cancel_scope`,
/// which is what makes the spec the declaration, and one test method has to
/// cover both halves of the disconnect contract.
pub struct TestOperation {
    /// The one registry of this daemon process.
    pub registry: Arc<OperationRegistry>,
}

impl Method for TestOperation {
    fn spec(&self) -> MethodSpec {
        MethodSpec::new(TEST_OPERATION, MethodKind::Operation, 1)
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let steps = u64_param(&params, "steps")?.unwrap_or(0);
            let step_ms = u64_param(&params, "step_ms")?.unwrap_or(0);
            let fail_at = u64_param(&params, "fail_at")?;
            let scope = match params.get("scope").and_then(Value::as_str) {
                None => CancelScope::Durable,
                Some(value) => CancelScope::from_wire(value).ok_or_else(|| {
                    DomainError::invalid_params("scope is `durable` or `client_scoped`")
                })?,
            };
            let (id, handle) =
                self.registry
                    .start(ConnectionId(ctx.connection_id), scope, TEST_OPERATION);
            tokio::spawn(fake_work(handle, steps, step_ms, fail_at));
            Ok(Outcome::query(json!({"operation_id": id.as_str()})))
        })
    }
}

/// The fake operation's work: `steps` reports, then a result, or the failure
/// `fail_at` asked for. A shut token stops the loop before the next report and
/// the worker then does nothing: the registry has already recorded the
/// cancellation.
async fn fake_work(handle: OperationHandle, steps: u64, step_ms: u64, fail_at: Option<u64>) {
    handle.set_running();
    for done in 1..=steps {
        if handle.token.is_cancelled() {
            return;
        }
        if step_ms > 0 {
            tokio::time::sleep(Duration::from_millis(step_ms)).await;
        }
        if handle.token.is_cancelled() {
            return;
        }
        handle.report(Progress {
            phase: "step".to_string(),
            done,
            total: Some(steps),
            message: None,
        });
        if fail_at == Some(done) {
            handle.fail(
                DomainError::internal(format!("the fake operation was told to fail at {done}"))
                    .with_data(json!({"failed_at": done})),
            );
            return;
        }
    }
    handle.succeed(json!({"steps": steps}));
}

/// An optional unsigned parameter; `null` and absent are both `None`.
fn u64_param(params: &Value, name: &str) -> Result<Option<u64>, DomainError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            DomainError::invalid_params(format!("{name} is a non-negative integer"))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hook is off in a process nobody set it in, so no real daemon serves
    /// the fake method.
    #[test]
    fn the_fake_operations_hook_is_off_unless_it_is_set() {
        assert_eq!(FAKE_OPERATIONS_ENV, "MAILYPOPPINS_DAEMON_FAKE_OPERATIONS");
        assert!(!fake_operations());
    }

    /// The snapshot projection is the non-terminal operations in start order,
    /// which is the one thing `live` promises beyond `status`.
    #[test]
    fn live_lists_the_unsettled_operations_in_start_order() {
        let registry = OperationRegistry::new();
        let (first, first_handle) =
            registry.start(ConnectionId(1), CancelScope::Durable, TEST_OPERATION);
        let (second, _) = registry.start(ConnectionId(1), CancelScope::Durable, TEST_OPERATION);
        assert_eq!(
            registry
                .live()
                .iter()
                .map(|status| status.id.clone())
                .collect::<Vec<_>>(),
            vec![first.clone(), second.clone()]
        );
        first_handle.succeed(json!({}));
        assert_eq!(
            registry
                .live()
                .iter()
                .map(|status| status.id.clone())
                .collect::<Vec<_>>(),
            vec![second],
            "a settled operation leaves the snapshot's array"
        );
        assert!(registry.status(&first).is_some(), "and keeps its status");
    }

    /// The fan-out runs on the emitting thread, right after the event is
    /// queued, so what it drains is in emission order.
    #[test]
    fn the_fanout_sees_every_event_in_emission_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let registry = Arc::new(OperationRegistry::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        registry.set_fanout(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let (_, handle) = registry.start(ConnectionId(1), CancelScope::Durable, TEST_OPERATION);
        handle.set_running();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "set_running publishes none"
        );
        handle.report(Progress {
            phase: "step".to_string(),
            done: 1,
            total: None,
            message: None,
        });
        handle.succeed(json!({}));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(registry.drain_events().len(), 2);
    }
}
