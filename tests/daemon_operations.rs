//! Long-running operations: the state machine, cancellation, progress ordering
//! and disconnect semantics (#0121, unit P3a-U7).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/operations.rs` exists, against the contract fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.4 (unit P3a-U7), the
//! error table in section 3.0, and the source plan's "Long-running operations"
//! prose ("operation state includes queued, running, succeeded, failed, and
//! cancelled states plus structured progress"; "client disconnect does not
//! cancel durable work unless the method explicitly declares client-scoped
//! cancellation"). It does not compile under `--features daemon` today, and
//! that failure *is* the proof the contract has no stub behind it. An
//! implementer (P3a-U8) does not edit this file; they make it pass.
//!
//! # The two layers, and why the split
//!
//! **(a) In-process, against `mailypoppins::daemon::operations`.** The state
//! machine is the registry's, not the wire's: "a `succeed` after a `fail` is
//! ignored", "a report on a terminal operation emits nothing", "an invalid
//! transition leaves the status exactly as it was" are properties of one
//! in-memory table, and over a socket they are unobservable as themselves,
//! because a client only ever sees the outcome the daemon chose to publish.
//! Layer (a) is also where the event *order* is pinned without a scheduler in
//! the way: [`OperationRegistry::drain_events`] hands over exactly what the
//! registry emitted, in emission order, with nothing coalesced, dropped or
//! reordered by a queue between the two.
//!
//! **(b) Over the socket, against a spawned `mp daemon run`.** Whether the
//! daemon answers `test.operation` before the work finishes, whether a
//! *different* connection can read the first one's operation, and above all
//! what a client's disconnect does to work in flight are properties of the
//! daemon process and its task layout. A disconnect in particular cannot be
//! faked in-process: the thing under test is that closing a socket reaches the
//! registry at all, and only a real daemon, a real socket and a real close can
//! be wrong about it.
//!
//! Which module: **`mailypoppins::daemon::operations`**, a sibling of
//! `daemon::dispatch` and `daemon::state`, not `daemon::state::operations`. An
//! operation is not part of the state a client mirrors: the snapshot's
//! `operations` array is a projection of the registry, the registry has its own
//! identifiers and its own lifetime, and it must be reachable from a method
//! (`operation.cancel`) and from the connection loop (a disconnect) without
//! either of them going through the canonical state's gate.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // mailypoppins::daemon::dispatch
//! pub enum CancelScope { Durable, ClientScoped }        // Copy + Eq + Hash + Debug
//! impl CancelScope {
//!     pub fn as_str(self) -> &'static str;              // "durable" | "client_scoped"
//!     pub fn from_wire(value: &str) -> Option<CancelScope>;
//! }
//! pub struct MethodSpec {                               // Copy + Eq + Debug
//!     pub name: &'static str,
//!     pub kind: MethodKind,
//!     pub since: u32,
//!     pub cancel_scope: CancelScope,                    // NEW in this unit
//! }
//! impl MethodSpec {
//!     pub const fn new(name: &'static str, kind: MethodKind, since: u32) -> MethodSpec;
//!     pub const fn client_scoped(self) -> MethodSpec;
//! }
//!
//! // mailypoppins::daemon::operations
//! pub const FAKE_OPERATIONS_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_OPERATIONS";
//! pub const KIND_OPERATION_PROGRESS: &str = "operation.progress";
//! pub const KIND_OPERATION_FINISHED: &str = "operation.finished";
//! pub const METHOD_OPERATION_STATUS: &str = "operation.status";
//! pub const METHOD_OPERATION_CANCEL: &str = "operation.cancel";
//! pub fn fake_operations() -> bool;
//!
//! pub struct OperationId(pub String);                   // Clone + Eq + Hash + Debug + Display
//! impl OperationId { pub fn new(id: impl Into<String>) -> Self; pub fn as_str(&self) -> &str; }
//!
//! pub enum OperationState { Queued, Running, Succeeded, Failed, Cancelled }  // Copy + Eq + Debug
//! impl OperationState {
//!     pub fn as_str(self) -> &'static str;              // the five wire strings
//!     pub fn from_wire(value: &str) -> Option<OperationState>;
//!     pub fn is_terminal(self) -> bool;                 // Succeeded | Failed | Cancelled
//! }
//!
//! pub struct Progress {                                 // Clone + PartialEq + Debug
//!     pub phase: String,
//!     pub done: u64,
//!     pub total: Option<u64>,
//!     pub message: Option<String>,
//! }
//! impl Progress { pub fn to_json(&self) -> serde_json::Value; }
//!
//! pub struct OperationStatus {                          // Clone + PartialEq + Debug
//!     pub id: OperationId,
//!     pub method: &'static str,
//!     pub owner: ConnectionId,
//!     pub scope: CancelScope,
//!     pub state: OperationState,
//!     pub progress: Option<Progress>,
//!     pub result: Option<serde_json::Value>,
//!     pub error: Option<DomainError>,
//! }
//! impl OperationStatus { pub fn to_json(&self) -> serde_json::Value; }
//!
//! pub struct OperationHandle { pub token: CancelToken, … }   // Clone + Debug
//! impl OperationHandle {
//!     pub fn id(&self) -> OperationId;
//!     pub fn set_running(&self);
//!     pub fn report(&self, progress: Progress);
//!     pub fn succeed(&self, result: serde_json::Value);
//!     pub fn fail(&self, error: DomainError);
//! }
//!
//! pub struct OperationRegistry { … }                    // Debug + Send + Sync
//! impl OperationRegistry {
//!     pub fn new() -> Self;
//!     pub fn start(&self, owner: ConnectionId, scope: CancelScope, method: &'static str)
//!         -> (OperationId, OperationHandle);
//!     pub fn status(&self, id: &OperationId) -> Option<OperationStatus>;
//!     pub fn cancel(&self, id: &OperationId) -> Result<(), DomainError>;
//!     pub fn on_disconnect(&self, conn: ConnectionId);
//!     pub fn drain_events(&self) -> Vec<Event>;
//! }
//! ```
//!
//! # Contract points this file pins beyond the plan text
//!
//! The plan fixes the states, the two method names and the progress payload,
//! and leaves the rest open. These are the decisions taken here; an implementer
//! who wants a different one needs a written contract amendment, not an edit to
//! this file.
//!
//! - **`CancelScope` is a field on `MethodSpec`, not a trait method.** The plan
//!   says "unless the method's `MethodSpec` declares client-scoped
//!   cancellation", and a declaration a caller has to call the method to read is
//!   not a declaration. It is the fourth field, and `MethodSpec::new` +
//!   `.client_scoped()` exist so a method that never thought about cancellation
//!   spells `Durable` by omission rather than by copying a constant.
//!   **This breaks `tests/daemon_dispatcher.rs`**, which builds four
//!   `MethodSpec { name, kind, since }` literals (lines 173, 209, 246, 280); a
//!   struct literal must name every field. The amendment is mechanical and is
//!   pre-approved here in writing, as the convention in section 3.0 requires:
//!   each of the four literals gains `cancel_scope: CancelScope::Durable`, and
//!   `CancelScope` joins that file's `use mailypoppins::daemon::dispatch::{…}`
//!   list. Nothing else in that file moves, and none of its assertions change
//!   meaning.
//! - **An operation id is an opaque non-empty string, unique for the life of
//!   the process.** Not a ULID, not `op-<n>`: a client echoes it and never
//!   parses it, and pinning a spelling would forbid the implementer the
//!   generator the rest of the daemon already has. Uniqueness and non-emptiness
//!   are asserted; the format is not.
//! - **Transitions run forward only, and an invalid one is a silent no-op.**
//!   `Queued -> Running -> {Succeeded, Failed, Cancelled}`, plus
//!   `Queued -> {Succeeded, Failed, Cancelled}` for work that finishes before it
//!   reports anything. Anything else - a `succeed` after a `fail`, a
//!   `set_running` after a cancel, a `report` on a terminal operation - leaves
//!   the status byte-identical and emits **no** event. Not a panic: the loser of
//!   the race is a worker thread that was already told to stop and cannot be
//!   expected to have noticed yet, and taking the daemon down for it would turn
//!   every cancellation into a crash. Not an error either: nothing is on the
//!   other end of a `report` to receive one.
//! - **`report` on a `Queued` operation moves it to `Running`.** A report is
//!   evidence of running, this is still a forward transition, and the
//!   alternative - dropping the first progress event of every method that
//!   forgot `set_running` - loses information to punish a caller.
//! - **Only two things emit events: `report` and the terminal transition.**
//!   `start` and `set_running` emit nothing. A client learns that an operation
//!   exists from the answer that carried its id, and learns it is running from
//!   the first progress event or from `operation.status`; an event per state
//!   change would put two frames on the wire for every operation that has
//!   nothing to say yet.
//! - **`cancel` is synchronous and terminal at once.** It shuts the token, sets
//!   `Cancelled`, and emits the finished event before it returns, so the
//!   `operation.status` a client calls immediately afterwards already says
//!   `cancelled`. The worker observes its token whenever it next looks, and its
//!   later `succeed`/`fail` is the ignored invalid transition above. The
//!   alternative - cancelled-when-the-worker-agrees - makes the wire answer to
//!   `operation.cancel` a promise instead of a fact, and gives every test a race
//!   to poll.
//! - **Cancelling a terminal operation is `-32602`** with
//!   `data = {"operation_id", "state"}`, and **cancelling an unknown id is
//!   `-32602`** with `data = {"operation_id"}`. No daemon code fits: `-32008`
//!   `operation_cancelled` is the *operation's own answer* to its caller, as
//!   `docs/daemon-protocol.md` says ("cancelling is the method's own answer,
//!   never a cancellation imposed on it from outside"), so reusing it for
//!   "you cannot cancel that" would make one code mean two things on one
//!   connection. The caller asked for something that does not exist, or asked
//!   for something impossible about something that does: that is what `-32602`
//!   is for. `-32009` is taken by `shutting_down`, and inventing `-32010` for a
//!   condition JSON-RPC already covers would widen the daemon range for nothing.
//!   Same rule for `operation.status` on an unknown id: `-32602` with
//!   `{"operation_id"}`.
//! - **The finished event carries `result` xor `error`.** Payload is
//!   `{"operation_id", "state", "result"}` for a success and
//!   `{"operation_id", "state", "error"}` for a failure or a cancellation, where
//!   `error` is the JSON-RPC `{code, message, data}` object. A cancelled
//!   operation's error is the table's own `-32008` with `{"operation_id"}`, so a
//!   client that missed the `operation.cancel` response learns the same fact
//!   from the event. The absent member is absent, not `null`: `data` is already
//!   omitted rather than nulled everywhere else in this protocol.
//! - **Operation events are lifecycle events and reach every bootstrapped
//!   connection**, not only the owner's. `Event::Lifecycle` survives an overflow
//!   and a poison precisely because no snapshot brings it back, and a second
//!   client that may read a first client's operation status (below) must be able
//!   to watch it too. A connection that has not bootstrapped is not a subscriber
//!   and receives nothing, exactly as for every other event.
//! - **An operation is daemon-wide, not connection-private.** `owner` decides
//!   what a disconnect cancels and nothing else: any initialized connection may
//!   call `operation.status` on any live id. The daemon is one process serving
//!   one user's data directory, and a GUI that started a sync must be able to
//!   watch it from the CLI window beside it.
//! - **`on_disconnect` cancels the connection's `ClientScoped` operations, in
//!   start order, and leaves everything else alone**: its own `Durable` ones,
//!   another connection's of either scope, and its own terminal ones. A
//!   cancellation from a disconnect is indistinguishable from an explicit
//!   `operation.cancel`: same state, same finished event, same `-32008` error
//!   inside it. A client that reconnects and asks is told what happened, and a
//!   second code for "your socket went away" would be a distinction only the
//!   daemon can see.
//! - **`drain_events` is a hand-off, not a log.** It returns what has been
//!   emitted since the last call, oldest first, and empties the queue. The
//!   revision each event travels at is stamped by whatever fans it out, not by
//!   the registry: revisions belong to the canonical state, and an operation
//!   that took one per progress report would burn thousands of them.
//! - **The fake operation method is an env hook, never a flag.** On the
//!   [`FAKE_EVENT_BURST_ENV`](mailypoppins::daemon::state::events::FAKE_EVENT_BURST_ENV)
//!   precedent: `MAILYPOPPINS_DAEMON_FAKE_OPERATIONS=1` registers
//!   `test.operation` on the dispatcher, nothing exposes it, and `mp --help`
//!   never moves. Phase 3a has no real long-running method - sync, auth and the
//!   rebuilds all arrive in Phase 5 - so without a hook the whole wire half of
//!   this contract would be untestable until then.
//!
//! # The fake operation, pinned
//!
//! With `MAILYPOPPINS_DAEMON_FAKE_OPERATIONS=1` the daemon registers one extra
//! method, `test.operation`, kind [`MethodKind::Operation`], `since` 1, whose
//! `MethodSpec` declares `CancelScope::Durable`. Params:
//!
//! ```json
//! {"steps": u64, "step_ms": u64, "scope": "durable"|"client_scoped", "fail_at": u64|null}
//! ```
//!
//! It answers **immediately** with `{"operation_id": str}` and runs the work in
//! the background: `steps` reports of `{"phase": "step", "done": i,
//! "total": steps, "message": null}` for `i` in `1..=steps`, each after a
//! `step_ms` pause, then `succeed({"steps": steps})`. A non-null `fail_at` of
//! `k` fails with `-32603` and `data = {"failed_at": k}` after the `k`-th
//! report instead of continuing. A shut token stops the loop before the next
//! report, and the worker then does nothing: the registry has already recorded
//! the cancellation.
//!
//! `scope` is the one thing the fake takes that a real method would not: it
//! passes the params' scope to [`OperationRegistry::start`] rather than its own
//! `spec().cancel_scope`, because one test method has to cover both halves of
//! the disconnect contract. A real method passes its own spec's scope, which is
//! what makes the spec the declaration.
//!
//! # Process hygiene
//!
//! Every daemon this file starts is killed before the test returns, including
//! on panic: the child goes into a [`Proc`] whose `Drop` kills and reaps it, and
//! [`Sandbox`]'s `Drop` kills whatever `daemon.pid` names. Every wait is a
//! bounded poll or a `tokio::time::timeout`, and no test sleeps for a fixed
//! duration to make an assertion true. Tests never touch the test process's
//! environment: each passes `HOME`, `MAILYPOPPINS_DATA_DIR` and
//! `MAILYPOPPINS_CONFIG_DIR` to the child through `Command::env`, so they are
//! safe to run in parallel and under `--test-threads=1` alike.
//!
//! The harness is a trimmed copy of `tests/daemon_events.rs`'s rather than a
//! shared `tests/common/` module: each daemon test file needs a different half
//! of it, and a shared module would have to be built into every explicit
//! `[[test]]` target.

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::{ErrorCode, EventEnvelope, JSONRPC_VERSION, METHOD_STATE_EVENT};

use mailypoppins::daemon::dispatch::{CancelScope, DomainError, MethodKind, MethodSpec};
use mailypoppins::daemon::operations::{
    fake_operations, OperationHandle, OperationId, OperationRegistry, OperationState,
    OperationStatus, Progress, FAKE_OPERATIONS_ENV, KIND_OPERATION_FINISHED,
    KIND_OPERATION_PROGRESS, METHOD_OPERATION_CANCEL, METHOD_OPERATION_STATUS,
};
use mailypoppins::daemon::state::events::Event;
use mailypoppins::daemon::state::ConnectionId;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Upper bound on any single wait: the socket appearing, a frame arriving, an
/// operation reaching a state. Generous, because it is a ceiling and never a
/// sleep.
const DEADLINE: Duration = Duration::from_secs(20);

/// Poll interval for every bounded wait.
const TICK: Duration = Duration::from_millis(25);

/// JSON-RPC's own "invalid params", the code both refusals of
/// `operation.cancel` carry.
const INVALID_PARAMS: i64 = -32602;

/// The wire name of the fake method the env hook registers.
const TEST_OPERATION: &str = "test.operation";

/// The connection ids layer (a) uses. Two, because half the contract is about
/// what one connection's disconnect does *not* do to the other's work.
const OWNER: ConnectionId = ConnectionId(1);
const OTHER: ConnectionId = ConnectionId(2);

// ===========================================================================
// Layer (a) - in-process, against the registry
// ===========================================================================

/// A registry with one operation started on it, for the tests that do not care
/// how it got there.
fn started(scope: CancelScope) -> (OperationRegistry, OperationId, OperationHandle) {
    let registry = OperationRegistry::new();
    let (id, handle) = registry.start(OWNER, scope, TEST_OPERATION);
    (registry, id, handle)
}

/// One progress report, spelled out so every test that asserts on a payload
/// asserts on the same one.
fn progress(done: u64) -> Progress {
    Progress {
        phase: "step".to_string(),
        done,
        total: Some(3),
        message: None,
    }
}

/// The state a registry reports for `id`, or a failure naming the id.
fn state_of(registry: &OperationRegistry, id: &OperationId) -> OperationState {
    registry
        .status(id)
        .unwrap_or_else(|| panic!("the registry still knows {id}"))
        .state
}

/// The five states spell exactly the wire strings the plan lists, and the
/// terminal predicate splits them the way every transition rule reads.
#[test]
fn the_five_operation_states_spell_the_documented_wire_strings() {
    let all = [
        (OperationState::Queued, "queued", false),
        (OperationState::Running, "running", false),
        (OperationState::Succeeded, "succeeded", true),
        (OperationState::Failed, "failed", true),
        (OperationState::Cancelled, "cancelled", true),
    ];
    for (state, wire, terminal) in all {
        assert_eq!(state.as_str(), wire, "{state:?} travels as {wire}");
        assert_eq!(
            OperationState::from_wire(wire),
            Some(state),
            "{wire} parses back into {state:?}"
        );
        assert_eq!(
            state.is_terminal(),
            terminal,
            "{state:?} is {} terminal",
            if terminal { "" } else { "not" }
        );
    }
    assert_eq!(
        OperationState::from_wire("stopped"),
        None,
        "a sixth state is not one this build serves"
    );
}

/// The two scopes spell the two strings the fake method's `scope` param takes,
/// and nothing else parses.
#[test]
fn the_two_cancel_scopes_spell_durable_and_client_scoped() {
    assert_eq!(CancelScope::Durable.as_str(), "durable");
    assert_eq!(CancelScope::ClientScoped.as_str(), "client_scoped");
    assert_eq!(
        CancelScope::from_wire("durable"),
        Some(CancelScope::Durable)
    );
    assert_eq!(
        CancelScope::from_wire("client_scoped"),
        Some(CancelScope::ClientScoped)
    );
    assert_eq!(
        CancelScope::from_wire("client-scoped"),
        None,
        "the wire spelling is snake_case like every other enum in this protocol"
    );
}

/// A method that never thought about cancellation declares `Durable`, and the
/// builder is the only way to say otherwise. This is the plan's "unless the
/// method's `MethodSpec` declares client-scoped cancellation": the declaration
/// is a field a caller reads, not a behaviour it discovers.
#[test]
fn a_method_spec_is_durable_unless_it_says_otherwise() {
    let plain = MethodSpec::new(TEST_OPERATION, MethodKind::Operation, 1);
    assert_eq!(plain.name, TEST_OPERATION);
    assert_eq!(plain.kind, MethodKind::Operation);
    assert_eq!(plain.since, 1);
    assert_eq!(
        plain.cancel_scope,
        CancelScope::Durable,
        "work outlives the client that asked for it unless the method says it must not"
    );

    let scoped = plain.client_scoped();
    assert_eq!(scoped.cancel_scope, CancelScope::ClientScoped);
    assert_eq!(
        MethodSpec {
            name: TEST_OPERATION,
            kind: MethodKind::Operation,
            since: 1,
            cancel_scope: CancelScope::ClientScoped,
        },
        scoped,
        "the builder is sugar over the fourth field and adds nothing to it"
    );
}

/// A started operation is queued, remembers who started it under which scope,
/// has reported nothing, and has produced neither a result nor an error.
#[test]
fn a_started_operation_is_queued_and_remembers_its_owner_and_scope() {
    let (registry, id, handle) = started(CancelScope::ClientScoped);
    let status = registry
        .status(&id)
        .expect("a started operation has status");

    assert_eq!(status.id, id);
    assert_eq!(handle.id(), id, "the handle names the operation it drives");
    assert_eq!(status.method, TEST_OPERATION);
    assert_eq!(status.owner, OWNER);
    assert_eq!(status.scope, CancelScope::ClientScoped);
    assert_eq!(status.state, OperationState::Queued);
    assert_eq!(status.progress, None, "nothing has been reported yet");
    assert_eq!(status.result, None);
    assert_eq!(status.error, None);
    assert!(
        !handle.token.is_cancelled(),
        "a fresh operation's token is open"
    );
    assert!(
        registry.drain_events().is_empty(),
        "starting an operation emits no event: the answer that carried the id is the announcement"
    );
}

/// Ids are opaque, non-empty and distinct. The format is deliberately not
/// pinned; a client echoes an id and never parses one.
#[test]
fn every_operation_gets_its_own_non_empty_id() {
    let registry = OperationRegistry::new();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let (id, _handle) = registry.start(OWNER, CancelScope::Durable, TEST_OPERATION);
        assert!(!id.as_str().is_empty(), "an id is a non-empty string");
        assert_eq!(
            id.to_string(),
            id.as_str(),
            "an id displays as the string it is"
        );
        assert!(seen.insert(id.as_str().to_string()), "ids do not repeat");
    }
    assert_eq!(seen.len(), 64);
}

/// The happy path, one transition at a time: queued, running, one report, one
/// result. This is the machine every other test in this file deviates from.
#[test]
fn the_state_machine_runs_forward_from_queued_to_succeeded() {
    let (registry, id, handle) = started(CancelScope::Durable);
    assert_eq!(state_of(&registry, &id), OperationState::Queued);

    handle.set_running();
    assert_eq!(state_of(&registry, &id), OperationState::Running);
    assert!(
        registry.drain_events().is_empty(),
        "set_running publishes nothing: an operation with nothing to say sends no frame"
    );

    handle.report(progress(1));
    let status = registry.status(&id).expect("status");
    assert_eq!(status.state, OperationState::Running);
    assert_eq!(
        status.progress,
        Some(progress(1)),
        "the status carries the newest report whole"
    );

    handle.succeed(json!({"steps": 3}));
    let status = registry
        .status(&id)
        .expect("a terminal operation keeps its status");
    assert_eq!(status.state, OperationState::Succeeded);
    assert_eq!(status.result, Some(json!({"steps": 3})));
    assert_eq!(status.error, None);
    assert_eq!(
        status.progress,
        Some(progress(1)),
        "the last report survives the finish, so a client that missed it can still read it"
    );
}

/// A report is evidence of running: a method that never called `set_running`
/// does not lose its first progress event to a state it forgot to announce.
#[test]
fn a_report_on_a_queued_operation_moves_it_to_running() {
    let (registry, id, handle) = started(CancelScope::Durable);
    handle.report(progress(1));

    assert_eq!(state_of(&registry, &id), OperationState::Running);
    let events = registry.drain_events();
    assert_eq!(events.len(), 1, "one report, one event: got {events:?}");
}

/// A failure is terminal and carries the error whole, which is what the
/// finished event and `operation.status` both render.
#[test]
fn a_failed_operation_carries_its_error_and_no_result() {
    let (registry, id, handle) = started(CancelScope::Durable);
    let error = DomainError::internal("the fake operation was told to fail");
    handle.set_running();
    handle.fail(error.clone());

    let status = registry.status(&id).expect("status");
    assert_eq!(status.state, OperationState::Failed);
    assert_eq!(status.result, None);
    assert_eq!(status.error, Some(error));
}

/// Every transition out of a terminal state is a silent no-op: the status does
/// not move and nothing is published. The loser of the race is a worker that was
/// already told to stop, and neither a panic nor a second finished event is an
/// answer to that.
#[test]
fn a_terminal_operation_ignores_every_later_transition() {
    let (registry, id, handle) = started(CancelScope::Durable);
    handle.succeed(json!({"steps": 0}));
    let settled = registry.status(&id).expect("status");
    assert_eq!(
        registry.drain_events().len(),
        1,
        "the finish published exactly one event"
    );

    handle.set_running();
    handle.report(progress(2));
    handle.fail(DomainError::internal("too late"));
    handle.succeed(json!({"steps": 99}));

    assert_eq!(
        registry.status(&id).expect("status"),
        settled,
        "an invalid transition leaves the status byte-identical"
    );
    assert!(
        registry.drain_events().is_empty(),
        "and publishes nothing at all"
    );
}

/// Reports reach the fan-out in the order they were made, with nothing
/// coalesced away, and the terminal event closes the sequence: after it, that
/// operation never speaks again.
#[test]
fn progress_events_arrive_in_report_order_and_the_finish_closes_them() {
    let (registry, id, handle) = started(CancelScope::Durable);
    handle.set_running();
    for done in 1..=3 {
        handle.report(progress(done));
    }
    handle.succeed(json!({"steps": 3}));

    let events = registry.drain_events();
    assert_eq!(
        events.len(),
        4,
        "three reports and one finish, none merged: got {events:?}"
    );
    for (index, event) in events.iter().take(3).enumerate() {
        let done = index as u64 + 1;
        assert_eq!(
            event,
            &Event::Lifecycle {
                kind: KIND_OPERATION_PROGRESS,
                payload: json!({
                    "operation_id": id.as_str(),
                    "phase": "step",
                    "done": done,
                    "total": 3,
                    "message": null,
                }),
            },
            "report {done} travels as its own lifecycle event"
        );
    }
    assert_eq!(
        events[3],
        Event::Lifecycle {
            kind: KIND_OPERATION_FINISHED,
            payload: json!({
                "operation_id": id.as_str(),
                "state": "succeeded",
                "result": {"steps": 3},
            }),
        },
        "a success names its result and carries no error member"
    );
    assert!(
        registry.drain_events().is_empty(),
        "the hand-off empties: a drained event is not delivered twice"
    );
}

/// The progress payload is the plan's five members exactly, including the two
/// that are `null` rather than absent, because a client reads `total` to draw a
/// bar and has to be able to tell "unknown" from "missing field".
#[test]
fn the_progress_payload_is_the_five_documented_members() {
    let (registry, id, handle) = started(CancelScope::Durable);
    handle.report(Progress {
        phase: "fetching".to_string(),
        done: 7,
        total: None,
        message: Some("mailbox 2 of many".to_string()),
    });

    let events = registry.drain_events();
    assert_eq!(
        events,
        vec![Event::Lifecycle {
            kind: KIND_OPERATION_PROGRESS,
            payload: json!({
                "operation_id": id.as_str(),
                "phase": "fetching",
                "done": 7,
                "total": null,
                "message": "mailbox 2 of many",
            }),
        }],
        "a report travels as one lifecycle event carrying the five members whole"
    );
}

/// A failure's finished event carries the JSON-RPC error object and no result,
/// which is the mirror image of the success above.
#[test]
fn a_failures_finished_event_carries_the_error_object() {
    let (registry, id, handle) = started(CancelScope::Durable);
    handle.fail(DomainError::invalid_params("steps must be below 1000"));

    let events = registry.drain_events();
    assert_eq!(events.len(), 1, "one finish, one event: got {events:?}");
    assert_eq!(
        events[0],
        Event::Lifecycle {
            kind: KIND_OPERATION_FINISHED,
            payload: json!({
                "operation_id": id.as_str(),
                "state": "failed",
                "error": {
                    "code": INVALID_PARAMS,
                    "message": "steps must be below 1000",
                },
            }),
        },
        "an error with no data omits `data` rather than nulling it"
    );
}

/// Cancelling shuts the token, settles the state and publishes the finish
/// before it returns: the answer to `operation.cancel` is a fact, not a promise
/// that a worker will agree later.
#[test]
fn cancelling_a_running_operation_shuts_its_token_and_finishes_it_at_once() {
    let (registry, id, handle) = started(CancelScope::Durable);
    handle.set_running();
    handle.report(progress(1));
    let _ = registry.drain_events();

    registry.cancel(&id).expect("a running operation cancels");

    assert!(
        handle.token.is_cancelled(),
        "the worker's token is shut, which is how it learns to stop"
    );
    let status = registry.status(&id).expect("status");
    assert_eq!(status.state, OperationState::Cancelled);
    assert_eq!(status.result, None);
    let error = status
        .error
        .expect("a cancelled operation carries the -32008 error");
    assert_eq!(
        error.code(),
        i64::from(ErrorCode::OperationCancelled.code()),
        "the table's own operation_cancelled, so a client that missed the response reads the same fact"
    );
    assert_eq!(error.data(), Some(json!({"operation_id": id.as_str()})));

    let events = registry.drain_events();
    assert_eq!(
        events,
        vec![Event::Lifecycle {
            kind: KIND_OPERATION_FINISHED,
            payload: json!({
                "operation_id": id.as_str(),
                "state": "cancelled",
                "error": {
                    "code": i64::from(ErrorCode::OperationCancelled.code()),
                    "message": error.message(),
                    "data": {"operation_id": id.as_str()},
                },
            }),
        }],
        "one finish, cancelled, carrying the same error the status shows"
    );
}

/// A queued operation cancels without ever having run, which is the case a
/// client hits when it changes its mind faster than the daemon schedules.
#[test]
fn cancelling_a_queued_operation_settles_it_without_running_it() {
    let (registry, id, _handle) = started(CancelScope::Durable);
    registry.cancel(&id).expect("a queued operation cancels");
    assert_eq!(state_of(&registry, &id), OperationState::Cancelled);
}

/// A second cancel is refused rather than being a no-op: the caller asked for
/// something impossible about something that exists, and the state it is in is
/// the one fact that explains the refusal.
#[test]
fn cancelling_a_terminal_operation_is_invalid_params_naming_its_state() {
    for (settle, state) in [
        (
            Box::new(|handle: &OperationHandle| handle.succeed(json!({})))
                as Box<dyn Fn(&OperationHandle)>,
            OperationState::Succeeded,
        ),
        (
            Box::new(|handle: &OperationHandle| handle.fail(DomainError::internal("failed"))),
            OperationState::Failed,
        ),
    ] {
        let (registry, id, handle) = started(CancelScope::Durable);
        settle(&handle);
        let error = registry
            .cancel(&id)
            .expect_err("a finished operation cannot be cancelled");
        assert_eq!(error.code(), INVALID_PARAMS, "got {error:?}");
        assert_eq!(
            error.data(),
            Some(json!({"operation_id": id.as_str(), "state": state.as_str()})),
            "the refusal names the operation and the state that made it impossible"
        );
    }

    // And cancelling twice is the same refusal, with `cancelled` as the state.
    let (registry, id, _handle) = started(CancelScope::Durable);
    registry.cancel(&id).expect("the first cancel takes");
    let error = registry.cancel(&id).expect_err("the second does not");
    assert_eq!(error.code(), INVALID_PARAMS);
    assert_eq!(
        error.data(),
        Some(json!({"operation_id": id.as_str(), "state": "cancelled"}))
    );
}

/// An id the registry never issued is `-32602` naming it, and `status` on it is
/// `None` rather than an invented entry.
#[test]
fn an_unknown_operation_id_is_invalid_params_naming_the_id() {
    let registry = OperationRegistry::new();
    let ghost = OperationId::new("op-does-not-exist");

    assert_eq!(registry.status(&ghost), None);
    let error = registry
        .cancel(&ghost)
        .expect_err("an unknown id cannot be cancelled");
    assert_eq!(error.code(), INVALID_PARAMS, "got {error:?}");
    assert_eq!(
        error.data(),
        Some(json!({"operation_id": "op-does-not-exist"})),
        "no `state` member: there is no state to name"
    );
}

/// The disconnect contract, both halves at once: the departing connection's
/// client-scoped work is cancelled, its durable work keeps running, and the
/// other connection's work of either scope is untouched.
#[test]
fn a_disconnect_cancels_only_that_connections_client_scoped_operations() {
    let registry = OperationRegistry::new();
    let (scoped, scoped_handle) = registry.start(OWNER, CancelScope::ClientScoped, TEST_OPERATION);
    let (durable, durable_handle) = registry.start(OWNER, CancelScope::Durable, TEST_OPERATION);
    let (other_scoped, _) = registry.start(OTHER, CancelScope::ClientScoped, TEST_OPERATION);
    let (other_durable, _) = registry.start(OTHER, CancelScope::Durable, TEST_OPERATION);
    for handle in [&scoped_handle, &durable_handle] {
        handle.set_running();
    }
    let _ = registry.drain_events();

    registry.on_disconnect(OWNER);

    assert_eq!(
        state_of(&registry, &scoped),
        OperationState::Cancelled,
        "work declared client-scoped dies with the client that asked for it"
    );
    assert!(
        scoped_handle.token.is_cancelled(),
        "and its worker is told, through the same token an explicit cancel shuts"
    );
    assert_eq!(
        state_of(&registry, &durable),
        OperationState::Running,
        "durable work survives its client: the plan's whole point"
    );
    assert!(
        !durable_handle.token.is_cancelled(),
        "and its token stays open"
    );
    assert_eq!(state_of(&registry, &other_scoped), OperationState::Queued);
    assert_eq!(state_of(&registry, &other_durable), OperationState::Queued);

    let events = registry.drain_events();
    assert_eq!(
        events,
        vec![Event::Lifecycle {
            kind: KIND_OPERATION_FINISHED,
            payload: json!({
                "operation_id": scoped.as_str(),
                "state": "cancelled",
                "error": {
                    "code": i64::from(ErrorCode::OperationCancelled.code()),
                    "message": registry
                        .status(&scoped)
                        .expect("status")
                        .error
                        .expect("error")
                        .message(),
                    "data": {"operation_id": scoped.as_str()},
                },
            }),
        }],
        "a disconnect cancellation is indistinguishable from an explicit one"
    );
}

/// A disconnect never disturbs work that has already finished, and never
/// publishes a second finished event for it.
#[test]
fn a_disconnect_leaves_that_connections_finished_operations_alone() {
    let registry = OperationRegistry::new();
    let (id, handle) = registry.start(OWNER, CancelScope::ClientScoped, TEST_OPERATION);
    handle.succeed(json!({"steps": 0}));
    let _ = registry.drain_events();

    registry.on_disconnect(OWNER);

    assert_eq!(state_of(&registry, &id), OperationState::Succeeded);
    assert!(
        registry.drain_events().is_empty(),
        "nothing changed, so nothing is published"
    );
}

/// A disconnect of a connection that started nothing is a no-op, which is the
/// common case: most connections never start an operation at all.
#[test]
fn a_disconnect_of_a_connection_with_no_operations_does_nothing() {
    let registry = OperationRegistry::new();
    let (id, handle) = registry.start(OWNER, CancelScope::ClientScoped, TEST_OPERATION);
    handle.set_running();

    registry.on_disconnect(ConnectionId(99));

    assert_eq!(state_of(&registry, &id), OperationState::Running);
    assert!(registry.drain_events().is_empty());
}

/// `OperationStatus::to_json` is what `operation.status` answers, and the wire
/// shape is fixed here rather than inside the method: two callers render it
/// (the method and the bootstrap snapshot's `operations` array) and they may
/// not disagree.
#[test]
fn the_status_json_is_the_documented_shape() {
    let (registry, id, handle) = started(CancelScope::ClientScoped);
    handle.report(progress(2));

    let status: OperationStatus = registry.status(&id).expect("status");
    assert_eq!(
        status.to_json(),
        json!({
            "operation_id": id.as_str(),
            "method": TEST_OPERATION,
            "state": "running",
            "scope": "client_scoped",
            "progress": {"phase": "step", "done": 2, "total": 3, "message": null},
            "result": null,
            "error": null,
        }),
        "every member is present, and the three that have nothing to say are null"
    );
    assert_eq!(
        status.progress.as_ref().expect("progress").to_json(),
        json!({"phase": "step", "done": 2, "total": 3, "message": null}),
        "the progress object renders the same four members it travels with in an event"
    );

    handle.succeed(json!({"steps": 3}));
    let finished = registry.status(&id).expect("status").to_json();
    assert_eq!(finished["state"], json!("succeeded"));
    assert_eq!(finished["result"], json!({"steps": 3}));
    assert_eq!(finished["error"], Value::Null);
}

/// The two method names and the env hook are the strings the wire half of this
/// file and `docs/daemon-protocol.md` both use.
#[test]
fn the_names_this_unit_puts_on_the_wire_are_the_documented_strings() {
    assert_eq!(METHOD_OPERATION_STATUS, "operation.status");
    assert_eq!(METHOD_OPERATION_CANCEL, "operation.cancel");
    assert_eq!(KIND_OPERATION_PROGRESS, "operation.progress");
    assert_eq!(KIND_OPERATION_FINISHED, "operation.finished");
    assert_eq!(FAKE_OPERATIONS_ENV, "MAILYPOPPINS_DAEMON_FAKE_OPERATIONS");
    assert!(
        !fake_operations(),
        "the hook is off in a process nobody set it in, so no real daemon serves test.operation"
    );
}

// ===========================================================================
// Layer (b) - over the socket, against a spawned daemon
// ===========================================================================

/// A private `HOME`, config directory and data directory.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives the
/// test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// One configured account, which is all a daemon needs to serve a bootstrap.
    fn single_account() -> Self {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Self { root };
        fs::write(
            sandbox.config_dir().join("config.toml"),
            r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#,
        )
        .expect("write config.toml");
        sandbox
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn socket(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.pid")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Spawn `mp daemon run`, with or without the fake operation method,
    /// killed on drop, and wait until its socket accepts.
    async fn start_daemon(&self, fake_operations: bool) -> Proc {
        let mut command = Command::new(MP);
        command
            .env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST")
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if fake_operations {
            command.env(FAKE_OPERATIONS_ENV, "1");
        } else {
            command.env_remove(FAKE_OPERATIONS_ENV);
        }
        let proc = Proc(Some(command.spawn().expect("spawn mp daemon run")));
        self.wait_socket_live().await;
        proc
    }

    /// Block until the socket accepts a connection, or fail the test.
    async fn wait_socket_live(&self) {
        let start = Instant::now();
        loop {
            if UnixStream::connect(self.socket()).await.is_ok() {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                self.socket().display()
            );
            tokio::time::sleep(TICK).await;
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    // Safety: a pid read from a pid file we own.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
}

/// A spawned `mp` process, killed and reaped on drop, so a panicking assertion
/// never leaves a daemon behind.
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Await `fut` under [`DEADLINE`], failing the test rather than the suite's
/// patience if the daemon never answers.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

/// The client identification every test sends.
fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Gui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Connect, handshake, and bootstrap, which is what makes the connection a
/// subscriber: an operation event reaches a connection that bootstrapped and no
/// other, exactly like every other event.
async fn connect_subscribed(sandbox: &Sandbox) -> Connection {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    within("state.bootstrap", conn.call("state.bootstrap", json!({})))
        .await
        .expect("state.bootstrap answers an initialized connection");
    conn
}

/// Start one fake operation and return the id the daemon answered with,
/// asserting that the answer came back before the work did.
async fn start_operation(
    conn: &mut Connection,
    steps: u64,
    step_ms: u64,
    scope: CancelScope,
    fail_at: Option<u64>,
) -> String {
    let params = json!({
        "steps": steps,
        "step_ms": step_ms,
        "scope": scope.as_str(),
        "fail_at": fail_at,
    });
    let result = within(TEST_OPERATION, conn.call(TEST_OPERATION, params))
        .await
        .expect("the fake operation method answers");
    let mut keys: Vec<&str> = result
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["operation_id"],
        "an operation method answers with the id and nothing else, got {result}"
    );
    result["operation_id"]
        .as_str()
        .expect("the operation id is a string")
        .to_string()
}

/// `operation.status` for one id, or a failure naming the refusal.
async fn status(conn: &mut Connection, id: &str) -> Value {
    within(
        METHOD_OPERATION_STATUS,
        conn.call(METHOD_OPERATION_STATUS, json!({"operation_id": id})),
    )
    .await
    .unwrap_or_else(|e| panic!("operation.status for {id} answers: {e}"))
}

/// The `state` member of `operation.status`, as a string.
async fn state_over_wire(conn: &mut Connection, id: &str) -> String {
    let status = status(conn, id).await;
    status["state"]
        .as_str()
        .unwrap_or_else(|| panic!("operation.status carries a string state, got {status}"))
        .to_string()
}

/// Poll `operation.status` until it reports `want`, bounded by [`DEADLINE`].
/// Returns the last status object, so a caller can assert on the rest of it.
async fn wait_for_state(conn: &mut Connection, id: &str, want: OperationState) -> Value {
    let start = Instant::now();
    loop {
        let status = status(conn, id).await;
        let state = status["state"]
            .as_str()
            .expect("a string state")
            .to_string();
        if state == want.as_str() {
            return status;
        }
        assert!(
            !OperationState::from_wire(&state)
                .unwrap_or_else(|| panic!("{state} is one of the five states"))
                .is_terminal(),
            "operation {id} settled on {state} while waiting for {}: {status}",
            want.as_str()
        );
        assert!(
            start.elapsed() < DEADLINE,
            "operation {id} never reached {} within {DEADLINE:?}, last status {status}",
            want.as_str()
        );
        tokio::time::sleep(TICK).await;
    }
}

/// The next `state.event` whose kind is one of this unit's two, failing on
/// anything else: this sandbox has no readiness hook and no burst, so an
/// operation event is the only event a daemon here can produce.
async fn next_operation_event(conn: &mut Connection) -> EventEnvelope {
    let notification = within("an operation event", conn.next_notification())
        .await
        .expect("the daemon delivers a notification rather than closing");
    assert_eq!(notification.jsonrpc, JSONRPC_VERSION);
    assert_eq!(
        notification.method, METHOD_STATE_EVENT,
        "operation progress travels as an ordinary state.event notification"
    );
    let event: EventEnvelope = serde_json::from_value(notification.params.clone())
        .unwrap_or_else(|e| panic!("the params are an event envelope: {e}; got {notification:?}"));
    assert!(
        event.kind == KIND_OPERATION_PROGRESS || event.kind == KIND_OPERATION_FINISHED,
        "nothing but this unit's two kinds can arrive in this sandbox, got {event:?}"
    );
    event
}

/// The JSON-RPC error behind a refused call, or a failure naming what came
/// instead.
fn rpc_error(label: &str, outcome: Result<Value, ClientError>) -> mp_protocol::RpcError {
    match outcome {
        Err(ClientError::Rpc(error)) => error,
        Ok(value) => panic!("{label} was answered with {value} instead of being refused"),
        Err(other) => panic!("{label} failed with {other} rather than a JSON-RPC error"),
    }
}

/// The two operation methods are served, and therefore advertised, on a daemon
/// nobody armed the hook on; the fake method is not.
#[tokio::test]
async fn the_capabilities_name_the_two_operation_methods_and_not_the_fake_one() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(false).await;

    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connect");
    let hello = within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("handshake");

    for method in [METHOD_OPERATION_STATUS, METHOD_OPERATION_CANCEL] {
        assert!(
            hello.capabilities.iter().any(|c| c == method),
            "{method} is served, so it is advertised: {:?}",
            hello.capabilities
        );
    }
    assert!(
        !hello.capabilities.iter().any(|c| c == TEST_OPERATION),
        "the fake method exists only under its env hook: {:?}",
        hello.capabilities
    );
}

/// The whole state machine over the wire: the call returns before the work
/// does, the operation is queued or running while it runs, and it ends
/// succeeded with the result the method produced.
#[tokio::test]
async fn an_operation_runs_from_queued_through_running_to_succeeded() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut conn = connect_subscribed(&sandbox).await;

    let started = Instant::now();
    let id = start_operation(&mut conn, 4, 120, CancelScope::Durable, None).await;
    let answered = started.elapsed();
    assert!(
        answered < Duration::from_millis(4 * 120),
        "an operation method answers with an id instead of waiting for the work: took {answered:?}"
    );

    let first = state_over_wire(&mut conn, &id).await;
    assert!(
        first == "queued" || first == "running",
        "an operation in flight is queued or running, got {first}"
    );

    let finished = wait_for_state(&mut conn, &id, OperationState::Succeeded).await;
    assert_eq!(finished["operation_id"], json!(id));
    assert_eq!(finished["method"], json!(TEST_OPERATION));
    assert_eq!(finished["scope"], json!("durable"));
    assert_eq!(finished["result"], json!({"steps": 4}));
    assert_eq!(finished["error"], Value::Null);
    assert_eq!(
        finished["progress"],
        json!({"phase": "step", "done": 4, "total": 4, "message": null}),
        "the last report survives the finish"
    );
}

/// Progress notifications arrive in order, with a strictly increasing `done`,
/// and the finished event ends the stream: nothing about that operation follows
/// it.
#[tokio::test]
async fn progress_notifications_arrive_in_order_and_stop_after_the_finish() {
    const STEPS: u64 = 5;
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut conn = connect_subscribed(&sandbox).await;

    let id = start_operation(&mut conn, STEPS, 20, CancelScope::Durable, None).await;

    let mut done_seen = 0_u64;
    let mut revision = 0_u64;
    for _ in 0..STEPS {
        let event = next_operation_event(&mut conn).await;
        assert_eq!(event.kind, KIND_OPERATION_PROGRESS);
        assert_eq!(event.payload["operation_id"], json!(id));
        assert_eq!(event.payload["phase"], json!("step"));
        assert_eq!(event.payload["total"], json!(STEPS));
        assert_eq!(event.payload["message"], Value::Null);
        let done = event.payload["done"].as_u64().expect("a u64 done");
        assert_eq!(
            done,
            done_seen + 1,
            "progress is strictly increasing and loses no step"
        );
        done_seen = done;
        assert!(
            event.revision > revision,
            "an operation event takes a revision above the last one, {} after {revision}",
            event.revision
        );
        revision = event.revision;
    }

    let finish = next_operation_event(&mut conn).await;
    assert_eq!(finish.kind, KIND_OPERATION_FINISHED);
    assert_eq!(
        finish.payload,
        json!({
            "operation_id": id,
            "state": "succeeded",
            "result": {"steps": STEPS},
        })
    );

    // Bounded by construction: it waits exactly this long and never longer.
    let quiet =
        tokio::time::timeout(Duration::from_millis(400), next_operation_event(&mut conn)).await;
    assert!(
        quiet.is_err(),
        "a finished operation says nothing more, got {quiet:?}"
    );
}

/// A cancel mid-run settles the operation at once and the finished event says
/// cancelled, carrying the table's `-32008`.
#[tokio::test]
async fn cancelling_mid_run_finishes_the_operation_cancelled() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut conn = connect_subscribed(&sandbox).await;

    let id = start_operation(&mut conn, 200, 25, CancelScope::Durable, None).await;
    // Wait for evidence of work rather than for a clock: the first progress
    // event proves the operation is running before the cancel lands.
    let first = next_operation_event(&mut conn).await;
    assert_eq!(first.kind, KIND_OPERATION_PROGRESS);

    let cancelled = within(
        METHOD_OPERATION_CANCEL,
        conn.call(METHOD_OPERATION_CANCEL, json!({"operation_id": id})),
    )
    .await
    .expect("cancelling a running operation succeeds");
    assert_eq!(
        cancelled,
        json!({"operation_id": id, "state": "cancelled"}),
        "the answer is the fact, not a promise"
    );
    assert_eq!(
        state_over_wire(&mut conn, &id).await,
        "cancelled",
        "the status a client reads straight afterwards already agrees"
    );

    // The finished event follows, possibly behind progress reports that were
    // already queued when the cancel landed.
    loop {
        let event = next_operation_event(&mut conn).await;
        if event.kind == KIND_OPERATION_FINISHED {
            assert_eq!(event.payload["operation_id"], json!(id));
            assert_eq!(event.payload["state"], json!("cancelled"));
            assert_eq!(
                event.payload["error"]["code"],
                json!(ErrorCode::OperationCancelled.code()),
                "a cancellation reports the table's own operation_cancelled"
            );
            assert_eq!(
                event.payload["error"]["data"],
                json!({"operation_id": id}),
                "which carries the operation id, as the error table fixes"
            );
            assert_eq!(event.payload["result"], Value::Null);
            break;
        }
        assert_eq!(
            event.payload["operation_id"],
            json!(id),
            "no other operation is running in this sandbox"
        );
    }
}

/// `fail_at` ends the operation `failed`, with the error on the status and on
/// the finished event, and no result on either.
#[tokio::test]
async fn a_failing_operation_finishes_failed_and_carries_its_error() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut conn = connect_subscribed(&sandbox).await;

    let id = start_operation(&mut conn, 6, 10, CancelScope::Durable, Some(2)).await;
    let failed = wait_for_state(&mut conn, &id, OperationState::Failed).await;

    assert_eq!(failed["result"], Value::Null);
    assert_eq!(
        failed["error"]["code"],
        json!(-32603),
        "the fake failure is the daemon's own fault, so it is an internal error: {failed}"
    );
    assert_eq!(failed["error"]["data"], json!({"failed_at": 2}));
    assert_eq!(
        failed["progress"]["done"],
        json!(2),
        "it failed after the second report and made no third"
    );
}

/// An operation is daemon-wide: a second connection, which did not start it and
/// does not own it, reads its status and watches it finish.
#[tokio::test]
async fn a_second_connection_reads_and_watches_the_first_ones_operation() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut owner = connect_subscribed(&sandbox).await;
    let mut observer = connect_subscribed(&sandbox).await;

    let id = start_operation(&mut owner, 3, 40, CancelScope::Durable, None).await;

    let seen = status(&mut observer, &id).await;
    assert_eq!(seen["operation_id"], json!(id));
    assert_eq!(seen["method"], json!(TEST_OPERATION));

    let finish = loop {
        let event = next_operation_event(&mut observer).await;
        assert_eq!(event.payload["operation_id"], json!(id));
        if event.kind == KIND_OPERATION_FINISHED {
            break event;
        }
    };
    assert_eq!(
        finish.payload["state"],
        json!("succeeded"),
        "an operation event reaches every bootstrapped connection, not only its owner"
    );
}

/// The plan's headline: "client disconnect does not cancel durable work". The
/// owner drops its socket mid-run and a second connection watches the operation
/// finish anyway.
#[tokio::test]
async fn disconnecting_the_owner_leaves_a_durable_operation_running() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut observer = connect_subscribed(&sandbox).await;

    let id = {
        let mut owner = connect_subscribed(&sandbox).await;
        let id = start_operation(&mut owner, 40, 50, CancelScope::Durable, None).await;
        // Evidence of work before the socket goes, so the disconnect really
        // lands mid-run rather than before the first step.
        let first = next_operation_event(&mut observer).await;
        assert_eq!(first.kind, KIND_OPERATION_PROGRESS);
        assert_eq!(first.payload["operation_id"], json!(id));
        id
        // `owner` drops here: the socket closes and the daemon observes it.
    };

    let finished = wait_for_state(&mut observer, &id, OperationState::Succeeded).await;
    assert_eq!(
        finished["result"],
        json!({"steps": 40}),
        "the whole operation ran, every step of it, after its client went away"
    );
}

/// The other half: work the call declared client-scoped dies with the client,
/// with the same state and the same error an explicit cancel produces.
#[tokio::test]
async fn disconnecting_the_owner_cancels_a_client_scoped_operation() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut observer = connect_subscribed(&sandbox).await;

    let id = {
        let mut owner = connect_subscribed(&sandbox).await;
        // Twenty seconds of work rather than five: a loaded machine can take a
        // while to notice the closed socket, and an operation that finished
        // first would settle on `succeeded` and fail this assertion for the
        // wrong reason. The wait below is still bounded by `DEADLINE`, so a
        // cancellation that never comes fails the test rather than the suite's
        // patience.
        let id = start_operation(&mut owner, 400, 50, CancelScope::ClientScoped, None).await;
        let first = next_operation_event(&mut observer).await;
        assert_eq!(first.kind, KIND_OPERATION_PROGRESS);
        assert_eq!(first.payload["operation_id"], json!(id));
        id
    };

    let cancelled = wait_for_state(&mut observer, &id, OperationState::Cancelled).await;
    assert_eq!(cancelled["scope"], json!("client_scoped"));
    assert_eq!(cancelled["result"], Value::Null);
    assert_eq!(
        cancelled["error"]["code"],
        json!(ErrorCode::OperationCancelled.code()),
        "a disconnect cancellation is indistinguishable from an explicit one: {cancelled}"
    );
    assert_eq!(cancelled["error"]["data"], json!({"operation_id": id}));
}

/// Both refusals of `operation.cancel`, over the wire, with the `data` each
/// carries.
#[tokio::test]
async fn cancelling_a_finished_or_unknown_operation_is_invalid_params() {
    let sandbox = Sandbox::single_account();
    let _daemon = sandbox.start_daemon(true).await;
    let mut conn = connect_subscribed(&sandbox).await;

    let id = start_operation(&mut conn, 0, 0, CancelScope::Durable, None).await;
    wait_for_state(&mut conn, &id, OperationState::Succeeded).await;

    let finished = rpc_error(
        "cancelling a finished operation",
        conn.call(METHOD_OPERATION_CANCEL, json!({"operation_id": id.clone()}))
            .await,
    );
    assert_eq!(finished.code, INVALID_PARAMS as i32);
    assert_eq!(
        finished.data,
        Some(json!({"operation_id": id, "state": "succeeded"})),
        "the refusal names the state that made the cancel impossible"
    );

    let unknown = rpc_error(
        "cancelling an unknown operation",
        conn.call(
            METHOD_OPERATION_CANCEL,
            json!({"operation_id": "op-no-such-thing"}),
        )
        .await,
    );
    assert_eq!(unknown.code, INVALID_PARAMS as i32);
    assert_eq!(
        unknown.data,
        Some(json!({"operation_id": "op-no-such-thing"})),
        "an id that names nothing has no state to report"
    );

    let missing = rpc_error(
        "operation.status for an unknown operation",
        conn.call(
            METHOD_OPERATION_STATUS,
            json!({"operation_id": "op-no-such-thing"}),
        )
        .await,
    );
    assert_eq!(missing.code, INVALID_PARAMS as i32);
    assert_eq!(
        missing.data,
        Some(json!({"operation_id": "op-no-such-thing"})),
        "status and cancel refuse an unknown id the same way"
    );
}
