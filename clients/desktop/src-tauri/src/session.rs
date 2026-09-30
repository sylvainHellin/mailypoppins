//! The GUI's one daemon session, its event pump, and the door commands call
//! through.
//!
//! One `mp_client::session::Session` on its own thread, announcing
//! `ClientKind::Gui`, holding one connection and one subscription. A pump
//! thread owns the `Session` (which keeps it alive for the process) and reads
//! its [`Incoming`] stream in order:
//!
//! - an event is checked against the [`StateTracker`] watermark and, when it
//!   applies, forwarded raw as [`GuiEvent::Event`];
//! - a resync, a reconnect, a poisoned stream or an instance change is
//!   forwarded and answered with a fresh `state.bootstrap`, sent as
//!   [`GuiEvent::Rebootstrapped`] so the frontend restores presentation state
//!   by stable identifiers;
//! - after every re-bootstrap the awaited operations (only server searches in
//!   M1) are re-queried with `operation.status`, as the TUI's
//!   `requery_operations` does, and settled or dropped.
//!
//! The frontend receives all of it on one ordered tauri `Channel`, registered
//! by `subscribe_events`. The first message on it is always a
//! [`GuiEvent::Connection`] and, once connected, a `Rebootstrapped` with
//! `cause: "subscribed"` taken under the pump lock, so nothing applied before
//! the subscription is missing from what the frontend holds.
//!
//! Lock order: `pump` before `sink`, and `conn` is never held with either.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};

use mp_client::events::Incoming;
use mp_client::session::{QueryHandle, Session};
use mp_client::{Observe, StateTracker};
use mp_protocol::events::KIND_OPERATION_FINISHED;
use mp_protocol::operation::OperationStatus;
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::connector::{self, ConnectError, ConnectFailure};
use crate::error::{Addressing, GuiError};
use crate::fixture::Fixture;

/// A server search's hit, which carries the search's `operation_id`.
const KIND_SERVER_HIT: &str = "message.server_hit";

const BOOTSTRAP_BUDGET: Duration = Duration::from_secs(10);
const STATUS_BUDGET: Duration = Duration::from_secs(5);

/// How long a command waits for a session that is still connecting.
pub const CONNECT_WAIT: Duration = Duration::from_secs(45);

// ---------------------------------------------------------------------------
// The door
// ---------------------------------------------------------------------------

/// What a command calls through: the daemon session, or the fixture.
#[derive(Clone)]
pub enum Door {
    Daemon(QueryHandle),
    Fixture(Arc<Fixture>),
}

impl Door {
    /// Call one method under a budget.
    pub fn call_within(&self, method: &str, params: Value, budget: Duration) -> Result<Value> {
        match self {
            Door::Daemon(handle) => handle.call_within(method, params, budget),
            Door::Fixture(fixture) => fixture.call(method, params),
        }
    }
}

/// A [`Door`] with one budget, for the typed reads of `mp_client::queries`.
pub struct Budgeted<'a> {
    pub door: &'a Door,
    pub budget: Duration,
}

impl mp_client::queries::Queries for Budgeted<'_> {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.door.call_within(method, params, self.budget)
    }
}

// ---------------------------------------------------------------------------
// What the frontend is told
// ---------------------------------------------------------------------------

/// Why a bootstrap was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapCause {
    /// The session came up.
    Initial,
    /// The frontend subscribed.
    Subscribed,
    /// The frontend asked (`bootstrap` command).
    Requested,
    /// The daemon sent `state.resync_required`.
    Resync,
    /// The session reconnected.
    Reconnected,
    /// An event came from a daemon instance this client never bootstrapped
    /// against, or the stream was poisoned.
    InstanceChanged,
}

/// What kind of operation the GUI awaits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    ServerSearch,
}

/// Where an intercepted URL came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterceptSource {
    /// A navigation (a link click in the reader, a meta refresh, a redirect).
    Navigation,
    /// `window.open` or a `target=_blank` link.
    NewWindow,
    /// `open_external` while `MP_DESKTOP_STUB_OPENER` is set.
    OpenExternalStub,
}

/// One URL the webview was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InterceptedUrl {
    pub url: String,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub source: InterceptSource,
}

impl InterceptedUrl {
    pub fn now(url: String, source: InterceptSource) -> InterceptedUrl {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        InterceptedUrl { url, at, source }
    }
}

/// The connection as the frontend shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connecting,
    Connected {
        instance_id: String,
        daemon_version: String,
        protocol: u32,
        fixture: bool,
    },
    /// The daemon went away; the session is reconnecting on its own.
    Reconnecting {
        reason: String,
        last_error: Option<ConnectError>,
    },
    /// No session could be set up; `error.kind` decides the screen
    /// (`version_mismatch` is the blocking restart screen).
    Failed {
        error: ConnectError,
    },
}

/// One message on the event channel, in order.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuiEvent {
    /// One `state.event` envelope, verbatim, that applied above the watermark.
    Event { event: EventEnvelope },
    /// The daemon poisoned this connection's queue; a `Rebootstrapped`
    /// follows.
    Resync { instance_id: String, reason: String },
    /// The daemon went away.
    Disconnected { reason: String },
    /// A daemon answers again; a `Rebootstrapped` follows.
    Reconnected { instance_id: String },
    /// A fresh snapshot; replace the model and restore presentation state by
    /// stable identifiers.
    Rebootstrapped {
        cause: BootstrapCause,
        bootstrap: Bootstrap,
    },
    /// An awaited operation finished while the stream could not say so.
    OperationSettled {
        operation_id: String,
        kind: PendingKind,
        status: OperationStatus,
    },
    /// An awaited operation can no longer be settled (daemon restarted, or
    /// the id was forgotten).
    OperationDropped {
        operation_id: String,
        kind: PendingKind,
        reason: String,
    },
    /// The connection status changed.
    Connection { status: ConnectionStatus },
    /// The webview refused a URL; the frontend may offer to open it.
    LinkIntercepted { url: InterceptedUrl },
}

/// Where events go: the tauri channel in the app, a closure in tests.
pub type Sink = Box<dyn Fn(GuiEvent) -> bool + Send>;

// ---------------------------------------------------------------------------
// The shared state
// ---------------------------------------------------------------------------

enum ConnState {
    /// Before [`SessionHandle::start`]; reads as connecting.
    Idle,
    Connecting,
    Ready {
        door: Door,
        down: Option<String>,
    },
    Failed(ConnectError),
}

#[derive(Default)]
struct Pump {
    tracker: Option<StateTracker>,
    pending: BTreeMap<String, PendingKind>,
    last_bootstrap: Option<Bootstrap>,
}

struct Shared {
    fixture: bool,
    conn: Mutex<ConnState>,
    changed: Condvar,
    pump: Mutex<Pump>,
    sink: Mutex<Option<Sink>>,
    fixture_door: Mutex<Option<Arc<Fixture>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The session, in Tauri managed state.
#[derive(Clone)]
pub struct SessionHandle {
    shared: Arc<Shared>,
}

impl SessionHandle {
    pub fn new(fixture: bool) -> SessionHandle {
        SessionHandle {
            shared: Arc::new(Shared {
                fixture,
                conn: Mutex::new(ConnState::Idle),
                changed: Condvar::new(),
                pump: Mutex::new(Pump::default()),
                sink: Mutex::new(None),
                fixture_door: Mutex::new(None),
            }),
        }
    }

    pub fn is_fixture(&self) -> bool {
        self.shared.fixture
    }

    /// Bring the session up in the background. Idempotent while connecting or
    /// connected; after a failure it tries again.
    pub fn start(&self) {
        {
            let mut conn = lock(&self.shared.conn);
            if matches!(*conn, ConnState::Ready { .. } | ConnState::Connecting) {
                return;
            }
            *conn = ConnState::Connecting;
        }
        self.status_changed();
        let this = self.clone();
        let spawned = std::thread::Builder::new()
            .name("mp-desktop-session".into())
            .spawn(move || {
                if this.shared.fixture {
                    this.run_fixture();
                } else {
                    this.run_daemon();
                }
            });
        if let Err(e) = spawned {
            self.fail(ConnectError {
                kind: ConnectFailure::Unavailable,
                why: format!("could not start the session thread: {e}"),
                socket: crate::paths::Paths::resolve().socket,
                log: crate::paths::Paths::resolve().daemon_log,
                daemon_version: None,
            });
        }
    }

    fn run_daemon(&self) {
        let preflight = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not build a runtime: {e}"))
            .map(|rt| {
                let result = rt.block_on(async {
                    connector::connect_or_start(true)
                        .await
                        .map(|(connection, hello)| {
                            drop(connection);
                            hello
                        })
                });
                drop(rt);
                result
            });
        match preflight {
            Ok(Ok(hello)) => tracing::info!(
                "[session] daemon {} (protocol {}) instance {} answers",
                hello.app_version,
                hello.protocol,
                hello.instance_id
            ),
            Ok(Err(e)) => return self.fail(e),
            Err(why) => return self.fail(unavailable(why)),
        }

        let mut session = match Session::connect(connector::connector()) {
            Ok(session) => session,
            Err(e) => return self.fail(unavailable(format!("{e:#}"))),
        };
        if connector::take_open_gave_up() {
            // Dropping the session ends its thread, which is serving a
            // connection that is already closed.
            drop(session);
            return self.fail(unavailable(format!(
                "a daemon answered, then refused the session's handshake for {} s",
                connector::OPEN_DEADLINE.as_secs()
            )));
        }
        let Some(events) = session.events() else {
            return self.fail(unavailable("the session handed out no event stream"));
        };
        let door = Door::Daemon(session.handle());
        self.ready(door.clone());
        self.pump(&door, events);
        drop(session);
        self.fail(unavailable("the daemon session ended"));
    }

    fn run_fixture(&self) {
        let (tx, rx) = channel();
        let fixture = match Fixture::load(tx) {
            Ok(f) => Arc::new(f),
            Err(e) => return self.fail(unavailable(format!("the fixtures did not load: {e:#}"))),
        };
        *lock(&self.shared.fixture_door) = Some(Arc::clone(&fixture));
        let door = Door::Fixture(fixture);
        self.ready(door.clone());
        self.pump(&door, rx);
    }

    fn ready(&self, door: Door) {
        {
            let mut pump = lock(&self.shared.pump);
            self.rebootstrap_locked(&mut pump, &door, BootstrapCause::Initial);
        }
        *lock(&self.shared.conn) = ConnState::Ready { door, down: None };
        self.shared.changed.notify_all();
        self.status_changed();
    }

    fn fail(&self, error: ConnectError) {
        tracing::warn!("[session] no daemon session: {error}");
        *lock(&self.shared.conn) = ConnState::Failed(error);
        self.shared.changed.notify_all();
        self.status_changed();
    }

    /// The door, waiting up to `wait` for a session that is still connecting.
    pub fn door(&self, wait: Duration) -> Result<Door, GuiError> {
        let deadline = Instant::now() + wait;
        let mut conn = lock(&self.shared.conn);
        loop {
            match &*conn {
                ConnState::Ready { door, .. } => return Ok(door.clone()),
                ConnState::Failed(e) => return Err(e.clone().into()),
                ConnState::Idle | ConnState::Connecting => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(GuiError::Timeout {
                            message: "the daemon session is still connecting".into(),
                        });
                    }
                    conn = match self.shared.changed.wait_timeout(conn, left) {
                        Ok((g, _)) => g,
                        Err(poisoned) => poisoned.into_inner().0,
                    };
                }
            }
        }
    }

    /// The fixture, in fixture mode.
    pub fn fixture(&self) -> Option<Arc<Fixture>> {
        lock(&self.shared.fixture_door).clone()
    }

    pub fn status(&self) -> ConnectionStatus {
        match &*lock(&self.shared.conn) {
            ConnState::Idle | ConnState::Connecting => ConnectionStatus::Connecting,
            ConnState::Failed(error) => ConnectionStatus::Failed {
                error: error.clone(),
            },
            ConnState::Ready {
                down: Some(reason), ..
            } => ConnectionStatus::Reconnecting {
                reason: reason.clone(),
                last_error: connector::last_reopen_failure(),
            },
            ConnState::Ready { down: None, .. } => {
                if self.shared.fixture {
                    // Not from the pump: `status` runs under the pump lock.
                    return ConnectionStatus::Connected {
                        instance_id: self.fixture().map(|f| f.instance_id()).unwrap_or_default(),
                        daemon_version: format!("fixture {}", env!("CARGO_PKG_VERSION")),
                        protocol: mp_protocol::PROTOCOL_MAX,
                        fixture: true,
                    };
                }
                let hello = connector::last_hello();
                ConnectionStatus::Connected {
                    instance_id: hello
                        .as_ref()
                        .map(|h| h.instance_id.clone())
                        .unwrap_or_default(),
                    daemon_version: hello
                        .as_ref()
                        .map(|h| h.daemon_version.clone())
                        .unwrap_or_default(),
                    protocol: hello.map_or(0, |h| h.protocol),
                    fixture: false,
                }
            }
        }
    }

    fn set_down(&self, reason: Option<String>) {
        if let ConnState::Ready { down, .. } = &mut *lock(&self.shared.conn) {
            *down = reason;
        }
        self.status_changed();
    }

    fn status_changed(&self) {
        let status = self.status();
        self.emit(GuiEvent::Connection { status });
    }

    /// Send one event to the frontend, if one is subscribed.
    pub fn emit(&self, event: GuiEvent) {
        let mut sink = lock(&self.shared.sink);
        if let Some(send) = sink.as_ref() {
            if !send(event) {
                tracing::info!("[session] the event channel closed");
                *sink = None;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Subscription and bootstrap
    // -----------------------------------------------------------------------

    /// Register the frontend's channel, replacing any earlier one.
    pub fn subscribe(&self, sink: Sink) {
        let mut pump = lock(&self.shared.pump);
        *lock(&self.shared.sink) = Some(sink);
        self.status_changed();
        let door = match &*lock(&self.shared.conn) {
            ConnState::Ready { door, down: None } => Some(door.clone()),
            _ => None,
        };
        if let Some(door) = door {
            self.rebootstrap_locked(&mut pump, &door, BootstrapCause::Subscribed);
        }
    }

    /// A fresh `state.bootstrap`, which also moves the watermark and is
    /// sent on the channel.
    pub fn bootstrap(&self, door: &Door) -> Result<Bootstrap, GuiError> {
        let mut pump = lock(&self.shared.pump);
        self.rebootstrap_locked(&mut pump, door, BootstrapCause::Requested)
            .ok_or_else(|| GuiError::protocol("the bootstrap failed; see the log"))
            .and_then(|_| {
                pump.last_bootstrap
                    .clone()
                    .ok_or_else(|| GuiError::internal("no bootstrap was kept"))
            })
    }

    /// The snapshot of the latest bootstrap.
    pub fn last_bootstrap(&self) -> Option<Bootstrap> {
        lock(&self.shared.pump).last_bootstrap.clone()
    }

    /// Bootstrap, reset the watermark, send the snapshot and re-query what
    /// is awaited. `None` when the bootstrap failed, which leaves the stream
    /// poisoned so the next event tries again.
    fn rebootstrap_locked(
        &self,
        pump: &mut Pump,
        door: &Door,
        cause: BootstrapCause,
    ) -> Option<()> {
        let answer = door
            .call_within("state.bootstrap", json!({}), BOOTSTRAP_BUDGET)
            .map_err(|e| format!("{e:#}"))
            .and_then(|v| serde_json::from_value::<Bootstrap>(v).map_err(|e| e.to_string()));
        let bootstrap = match answer {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("[session] the bootstrap ({cause:?}) failed: {e}");
                if let Some(t) = pump.tracker.as_mut() {
                    t.invalidate();
                }
                return None;
            }
        };
        tracing::info!(
            "[session] bootstrapped ({cause:?}) at revision {} against {}",
            bootstrap.revision,
            bootstrap.instance_id
        );
        match pump.tracker.as_mut() {
            Some(t) => t.rebootstrap(bootstrap.revision, bootstrap.instance_id.clone()),
            None => {
                pump.tracker = Some(StateTracker::new(
                    bootstrap.revision,
                    bootstrap.instance_id.clone(),
                ))
            }
        }
        pump.last_bootstrap = Some(bootstrap.clone());
        self.emit(GuiEvent::Rebootstrapped { cause, bootstrap });
        self.requery_locked(pump, door);
        Some(())
    }

    /// The TUI's `requery_operations`: settle what finished in the gap, drop
    /// what the daemon no longer knows.
    fn requery_locked(&self, pump: &mut Pump, door: &Door) {
        let ids: Vec<(String, PendingKind)> =
            pump.pending.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (id, kind) in ids {
            let answer = door
                .call_within(
                    "operation.status",
                    json!({"operation_id": id}),
                    STATUS_BUDGET,
                )
                .map_err(|e| format!("{e:#}"))
                .and_then(|v| {
                    serde_json::from_value::<OperationStatus>(v).map_err(|e| e.to_string())
                });
            match answer {
                Ok(status) if status.state.is_terminal() => {
                    pump.pending.remove(&id);
                    self.emit(GuiEvent::OperationSettled {
                        operation_id: id,
                        kind,
                        status,
                    });
                }
                Ok(_) => {}
                Err(reason) => {
                    pump.pending.remove(&id);
                    self.emit(GuiEvent::OperationDropped {
                        operation_id: id,
                        kind,
                        reason,
                    });
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Operations
    // -----------------------------------------------------------------------

    /// Start an operation and await it, holding the pump so its finish
    /// cannot overtake the registration.
    pub fn start_operation(
        &self,
        door: &Door,
        method: &str,
        params: Value,
        kind: PendingKind,
        budget: Duration,
    ) -> Result<String, GuiError> {
        let mut pump = lock(&self.shared.pump);
        let answer = door
            .call_within(method, params, budget)
            .map_err(|e| GuiError::from_call(&e, Addressing::Params))?;
        let id = answer["operation_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| GuiError::protocol(format!("{method} answered no operation_id")))?
            .to_string();
        pump.pending.insert(id.clone(), kind);
        Ok(id)
    }

    /// Stop awaiting an operation.
    pub fn forget_operation(&self, id: &str) {
        lock(&self.shared.pump).pending.remove(id);
    }

    /// The operations awaited, for tests and diagnostics.
    pub fn pending(&self) -> Vec<String> {
        lock(&self.shared.pump).pending.keys().cloned().collect()
    }

    // -----------------------------------------------------------------------
    // The pump
    // -----------------------------------------------------------------------

    fn pump(&self, door: &Door, events: Receiver<Incoming>) {
        while let Ok(incoming) = events.recv() {
            self.handle(door, incoming);
        }
    }

    /// Apply one thing the session thread said.
    pub fn handle(&self, door: &Door, incoming: Incoming) {
        match incoming {
            Incoming::Event(event) => {
                let mut pump = lock(&self.shared.pump);
                let verdict = pump
                    .tracker
                    .as_mut()
                    .map(|t| t.observe(event.revision, &event.instance_id));
                match verdict {
                    Some(Observe::Apply) => {
                        // The TUI's `apply_finished` and `apply_server_hit`:
                        // an operation this layer no longer awaits is not
                        // this client's, or was already settled by a
                        // re-bootstrap's `operation.status`, and a hit or a
                        // finish after its `operation_settled` would reopen it.
                        let operation = matches!(
                            event.kind.as_str(),
                            KIND_OPERATION_FINISHED | KIND_SERVER_HIT
                        );
                        if operation {
                            let id = event.payload["operation_id"].as_str().unwrap_or_default();
                            if !pump.pending.contains_key(id) {
                                tracing::debug!(
                                    "[session] dropped {} for operation `{id}`, not awaited",
                                    event.kind
                                );
                                return;
                            }
                            if event.kind == KIND_OPERATION_FINISHED {
                                pump.pending.remove(id);
                            }
                        }
                        self.emit(GuiEvent::Event { event });
                    }
                    Some(Observe::Duplicate) => {}
                    Some(Observe::Gap) | Some(Observe::InstanceChanged) | None => {
                        self.rebootstrap_locked(&mut pump, door, BootstrapCause::InstanceChanged);
                    }
                }
            }
            Incoming::Resync {
                instance_id,
                reason,
            } => {
                let mut pump = lock(&self.shared.pump);
                if let Some(t) = pump.tracker.as_mut() {
                    t.invalidate();
                }
                self.emit(GuiEvent::Resync {
                    instance_id,
                    reason,
                });
                self.rebootstrap_locked(&mut pump, door, BootstrapCause::Resync);
            }
            Incoming::Disconnected { reason } => {
                self.emit(GuiEvent::Disconnected {
                    reason: reason.clone(),
                });
                self.set_down(Some(reason));
            }
            Incoming::Reconnected { instance_id } => {
                let mut pump = lock(&self.shared.pump);
                self.emit(GuiEvent::Reconnected {
                    instance_id: instance_id.clone(),
                });
                let restarted = pump
                    .tracker
                    .as_ref()
                    .is_some_and(|t| t.instance_id() != instance_id);
                if restarted {
                    // Operation ids belong to the instance that issued them.
                    let dropped = std::mem::take(&mut pump.pending);
                    for (operation_id, kind) in dropped {
                        self.emit(GuiEvent::OperationDropped {
                            operation_id,
                            kind,
                            reason: "the daemon restarted".into(),
                        });
                    }
                }
                self.rebootstrap_locked(&mut pump, door, BootstrapCause::Reconnected);
                drop(pump);
                self.set_down(None);
            }
        }
    }
}

fn unavailable(why: impl Into<String>) -> ConnectError {
    let paths = crate::paths::Paths::resolve();
    ConnectError {
        kind: ConnectFailure::Unavailable,
        why: why.into(),
        socket: paths.socket,
        log: paths.daemon_log,
        daemon_version: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Sender;

    type Seen = Arc<Mutex<Vec<Value>>>;

    /// A fixture session whose events land in a vector.
    fn harness() -> (SessionHandle, Door, Arc<Fixture>, Receiver<Incoming>, Seen) {
        let (tx, rx): (Sender<Incoming>, Receiver<Incoming>) = channel();
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        let session = SessionHandle::new(true);
        *lock(&session.shared.fixture_door) = Some(Arc::clone(&fixture));
        let door = Door::Fixture(Arc::clone(&fixture));
        session.ready(door.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        session.subscribe(Box::new(move |e| {
            lock(&sink_seen).push(serde_json::to_value(&e).unwrap_or_default());
            true
        }));
        (session, door, fixture, rx, seen)
    }

    fn types(seen: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
        lock(seen)
            .iter()
            .map(|v| {
                let t = v["type"].as_str().unwrap_or_default();
                match t {
                    "rebootstrapped" => {
                        format!("rebootstrapped:{}", v["cause"].as_str().unwrap_or_default())
                    }
                    "event" => format!("event:{}", v["event"]["kind"].as_str().unwrap_or_default()),
                    "connection" => format!(
                        "connection:{}",
                        v["status"]["state"].as_str().unwrap_or_default()
                    ),
                    other => other.to_string(),
                }
            })
            .collect()
    }

    fn drain(session: &SessionHandle, door: &Door, rx: &Receiver<Incoming>) {
        while let Ok(incoming) = rx.recv_timeout(Duration::from_millis(100)) {
            session.handle(door, incoming);
        }
    }

    #[test]
    fn subscribing_sends_the_status_and_a_snapshot_first() {
        let (_s, _d, _f, _rx, seen) = harness();
        assert_eq!(
            types(&seen),
            vec!["connection:connected", "rebootstrapped:subscribed"]
        );
    }

    #[test]
    fn events_above_the_watermark_are_forwarded_raw_and_duplicates_dropped() {
        let (session, door, fixture, rx, seen) = harness();
        fixture.simulate("new_mail").expect("new mail");
        let incoming = rx.recv_timeout(Duration::from_secs(1)).expect("an event");
        let Incoming::Event(event) = incoming else {
            panic!("an event")
        };
        session.handle(&door, Incoming::Event(event.clone()));
        session.handle(&door, Incoming::Event(event));
        let t = types(&seen);
        assert_eq!(
            t.iter().filter(|x| *x == "event:state.invalidate").count(),
            1
        );
        let last = lock(&seen).last().cloned().unwrap_or_default();
        assert_eq!(last["event"]["payload"]["resource"], "mailbox:work/inbox");
    }

    #[test]
    fn a_resync_is_forwarded_and_answered_with_a_bootstrap() {
        let (session, door, fixture, rx, seen) = harness();
        fixture.simulate("resync").expect("resync");
        drain(&session, &door, &rx);
        let t = types(&seen);
        assert_eq!(&t[t.len() - 2..], ["resync", "rebootstrapped:resync"]);
    }

    #[test]
    fn a_restart_drops_awaited_operations_and_rebootstraps() {
        let (session, door, fixture, rx, seen) = harness();
        let id = session
            .start_operation(
                &door,
                "message.search_server",
                json!({"account": "work", "query": "zzz-nothing"}),
                PendingKind::ServerSearch,
                Duration::from_secs(1),
            )
            .expect("started");
        assert_eq!(session.pending(), vec![id.clone()]);
        fixture.simulate("restart").expect("restart");
        drain(&session, &door, &rx);
        let t = types(&seen);
        assert!(t.contains(&"disconnected".to_string()), "{t:?}");
        let i = t
            .iter()
            .position(|x| x == "reconnected")
            .expect("reconnected");
        assert_eq!(t[i + 1], "operation_dropped");
        assert_eq!(t[i + 2], "rebootstrapped:reconnected");
        assert!(session.pending().is_empty());
        assert!(matches!(
            session.status(),
            ConnectionStatus::Connected { .. }
        ));
    }

    /// Start a server search on the fixture, its hits streamed without a gap.
    fn search(session: &SessionHandle, door: &Door, fixture: &Fixture, query: &str) -> String {
        fixture.set_hit_delay(Duration::ZERO);
        session
            .start_operation(
                door,
                "message.search_server",
                json!({"account": "work", "query": query}),
                PendingKind::ServerSearch,
                Duration::from_secs(1),
            )
            .expect("started")
    }

    /// Wait for the fixture to finish `id`, by its `operation.status` rather
    /// than by a guessed sleep.
    fn await_terminal(door: &Door, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = door
                .call_within(
                    "operation.status",
                    json!({"operation_id": id}),
                    Duration::from_secs(1),
                )
                .expect("status");
            if status["state"] != "running" {
                return;
            }
            assert!(Instant::now() < deadline, "{id} never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The events the fixture queued for `id`, hits up to its finish. The
    /// finish is posted just after the status turns terminal, so it is
    /// waited for.
    fn queued_for(rx: &Receiver<Incoming>, id: &str) -> Vec<EventEnvelope> {
        let mut out = Vec::new();
        loop {
            let incoming = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the operation's finish");
            if let Incoming::Event(e) = incoming {
                if e.payload["operation_id"] == id {
                    let finished = e.kind == KIND_OPERATION_FINISHED;
                    out.push(e);
                    if finished {
                        return out;
                    }
                }
            }
        }
    }

    #[test]
    fn a_finish_lost_to_a_resync_is_settled_by_the_requery() {
        let (session, door, fixture, rx, seen) = harness();
        let id = search(&session, &door, &fixture, "offsite");
        // Let the operation finish without applying its events: the queue
        // was lost, as a resync loses it.
        await_terminal(&door, &id);
        while rx.try_recv().is_ok() {}
        session.handle(
            &door,
            Incoming::Resync {
                instance_id: "fixture-instance-1".into(),
                reason: "event_queue_overflow".into(),
            },
        );
        let settled = lock(&seen)
            .iter()
            .find(|v| v["type"] == "operation_settled")
            .cloned()
            .expect("settled by the requery");
        assert_eq!(settled["operation_id"], id.as_str());
        assert_eq!(settled["status"]["state"], "succeeded");
        assert!(session.pending().is_empty());
    }

    /// The search finished between the bootstrap and the `operation.status`
    /// reply: its hits and its finish sit above the new watermark, and would
    /// reach the frontend after `operation_settled`.
    #[test]
    fn hits_and_a_finish_after_the_requery_settled_them_are_dropped() {
        let (session, door, fixture, rx, seen) = harness();
        let id = search(&session, &door, &fixture, "offsite");
        await_terminal(&door, &id);
        let queued = queued_for(&rx, &id);
        assert!(queued.len() >= 2, "hits and a finish: {queued:?}");
        session.handle(
            &door,
            Incoming::Resync {
                instance_id: fixture.instance_id(),
                reason: "event_queue_overflow".into(),
            },
        );
        let bootstrap = session.last_bootstrap().expect("bootstrapped");
        let mark = lock(&seen).len();
        // Replay them above the watermark, as the race delivers them, then
        // one unrelated event to show the stream is still applying.
        let mut revision = bootstrap.revision;
        for mut event in queued {
            revision += 1;
            event.revision = revision;
            event.instance_id = bootstrap.instance_id.clone();
            session.handle(&door, Incoming::Event(event));
        }
        session.handle(
            &door,
            Incoming::Event(EventEnvelope {
                instance_id: bootstrap.instance_id.clone(),
                revision: revision + 1,
                kind: "state.invalidate".into(),
                payload: json!({"resource": "mailbox:work/inbox"}),
            }),
        );
        let t = types(&seen);
        assert!(
            t[..mark].contains(&"operation_settled".to_string()),
            "{t:?}"
        );
        assert_eq!(t[mark..], ["event:state.invalidate"], "{t:?}");
    }

    #[test]
    fn only_awaited_operations_reach_the_frontend() {
        let (session, door, _fixture, _rx, seen) = harness();
        let bootstrap = session.last_bootstrap().expect("bootstrapped");
        lock(&session.shared.pump)
            .pending
            .insert("op-mine".into(), PendingKind::ServerSearch);
        let mark = lock(&seen).len();
        let event = |revision: u64, kind: &str, id: &str| {
            Incoming::Event(EventEnvelope {
                instance_id: bootstrap.instance_id.clone(),
                revision: bootstrap.revision + revision,
                kind: kind.into(),
                payload: json!({"operation_id": id}),
            })
        };
        session.handle(&door, event(1, KIND_SERVER_HIT, "op-mine"));
        session.handle(&door, event(2, KIND_SERVER_HIT, "op-other-window"));
        session.handle(&door, event(3, KIND_OPERATION_FINISHED, "op-other-window"));
        session.handle(&door, event(4, KIND_OPERATION_FINISHED, "op-mine"));
        session.handle(&door, event(5, KIND_SERVER_HIT, "op-mine"));
        let t = types(&seen);
        assert_eq!(
            t[mark..],
            ["event:message.server_hit", "event:operation.finished"],
            "{t:?}"
        );
        assert!(session.pending().is_empty());
    }

    /// The real path against a real daemon in a scratch data directory:
    /// on-demand start through `mp daemon start`, the handshake with every
    /// required capability, the session, the first bootstrap, a command and
    /// the reader. Run alone, since it sets the data directory for the
    /// process:
    ///
    /// `MP_DESKTOP_MP_BIN=<target>/debug/mp cargo test -- --ignored live_daemon`
    #[test]
    #[ignore = "needs an `mp` binary in MP_DESKTOP_MP_BIN"]
    fn live_daemon_round_trip() {
        let _lock = crate::test_support::env_lock();
        assert!(
            std::env::var_os(connector::MP_BIN_ENV).is_some(),
            "set MP_DESKTOP_MP_BIN"
        );
        let data = crate::test_support::scratch_dir("live-data");
        let config = crate::test_support::scratch_dir("live-config");
        std::env::set_var("MAILYPOPPINS_DATA_DIR", &data);
        std::env::set_var("MAILYPOPPINS_CONFIG_DIR", &config);

        let session = SessionHandle::new(false);
        session.start();
        let door = session.door(Duration::from_secs(40));
        let stop = || {
            let bin = std::env::var_os(connector::MP_BIN_ENV).unwrap_or_default();
            let _ = std::process::Command::new(bin)
                .args(["daemon", "stop"])
                .status();
        };
        let door = match door {
            Ok(door) => door,
            Err(e) => {
                stop();
                panic!("no session: {e:?}");
            }
        };
        let status = session.status();
        let accounts = crate::commands::list_accounts_on(&door, session.last_bootstrap().as_ref());
        let bootstrap = session.bootstrap(&door);
        let reader = crate::reader::respond("GET", "/nobody/1", Ok(door.clone()));

        // A deliberate restart: the session sees the daemon go, reconnects
        // to the new instance and re-bootstraps on its own.
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        session.subscribe(Box::new(move |e| {
            lock(&sink_seen).push(serde_json::to_value(&e).unwrap_or_default());
            true
        }));
        let restarted = connector::restart_daemon_blocking();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline
            && !types(&seen).contains(&"rebootstrapped:reconnected".to_string())
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        let after = session.status();
        stop();

        assert!(restarted.is_ok(), "{restarted:?}");
        let t = types(&seen);
        let down = t
            .iter()
            .position(|x| x == "disconnected")
            .expect("disconnected");
        let up = t
            .iter()
            .position(|x| x == "reconnected")
            .expect("reconnected");
        assert!(
            down < up && t[up + 1] == "rebootstrapped:reconnected",
            "{t:?}"
        );
        assert!(
            matches!(after, ConnectionStatus::Connected { .. }),
            "{after:?}"
        );

        assert!(
            matches!(status, ConnectionStatus::Connected { fixture: false, .. }),
            "{status:?}"
        );
        assert!(connector::last_hello().is_some_and(|h| !h.daemon_version.is_empty()));
        assert_eq!(accounts.expect("account.list"), vec![]);
        assert!(bootstrap.expect("bootstrap").snapshot.accounts.is_empty());
        assert_eq!(reader.status(), tauri::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_disconnect_is_reconnecting_until_the_reconnect() {
        let (session, door, fixture, rx, _seen) = harness();
        fixture.simulate("disconnect").expect("down");
        drain(&session, &door, &rx);
        assert!(matches!(
            session.status(),
            ConnectionStatus::Reconnecting { .. }
        ));
        let e = door
            .call_within("state.bootstrap", json!({}), Duration::from_secs(1))
            .expect_err("refused while down");
        assert!(matches!(
            GuiError::from_call(&e, Addressing::Params),
            GuiError::DaemonUnavailable { .. }
        ));
        fixture.simulate("reconnect").expect("up");
        drain(&session, &door, &rx);
        assert!(matches!(
            session.status(),
            ConnectionStatus::Connected { .. }
        ));
    }
}
