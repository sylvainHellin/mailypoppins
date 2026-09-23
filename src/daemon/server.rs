//! The socket server: one tokio task per connection, NDJSON JSON-RPC in and
//! out (P2-U7).
//!
//! `initialize`, `daemon.status` and `daemon.stop` are answered here and are
//! not registered on the dispatcher: they are lifecycle surface rather than
//! domain surface, they run *before* a handshake (`mp daemon status` must
//! describe a daemon whose protocol range it cannot negotiate, and
//! `mp daemon stop` must be able to end one), and two of them need the
//! connection's own state, which no domain method may reach. Every other method
//! goes to [`DaemonState::dispatcher`] under the
//! [`ClientCtx`](super::dispatch::ClientCtx) the handshake
//! settled, and an unregistered name comes back as `-32601` from there. The
//! handshake itself lives in [`super::session`], which owns the per-connection
//! state [`dispatch_request`] gates on.
//!
//! Shutdown is a [`watch`] channel rather than a flag: the accept loop selects
//! on it, and [`super::shutdown`] sends on it once it has run the eight steps
//! of a graceful stop, whether a `daemon.stop` or a signal started them. From
//! the instant the first of those steps runs, [`dispatch_request`] refuses
//! every method but `daemon.status` and `daemon.stop` with `-32009`, ahead of
//! the handshake gate: the gate exempts `initialize` by construction, so a
//! refusal placed behind it would let a client negotiate its way into a daemon
//! that is leaving.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use log::{debug, info, warn};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::watch;

use mp_protocol::frame::{self, Decoder, FrameError};
use mp_protocol::{
    ErrorCode, ErrorResponse, EventEnvelope, Notification, Request, RequestId, RpcError,
    JSONRPC_VERSION, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, METHOD_STATE_EVENT,
    METHOD_STATE_RESYNC_REQUIRED,
};

use super::config::ConfigStore;
use super::dispatch::Dispatcher;
use super::operations::OperationRegistry;
use super::runtime::account::{AccountRuntime, Readiness, TickKind, TickOutcome};
use super::runtime::InstanceMeta;
use super::session::Session;
use super::state::events::{Event, Outbound, Outgoing, Subscriber};
use super::state::{
    seeds_from_config, CanonicalState, Change, ConnectionId, EventQueue, InstanceId, Revision,
};

/// JSON-RPC standard codes this unit emits. The daemon range lives in
/// [`mp_protocol::ErrorCode`] and is not reachable until P2-U9.
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// Hands out one id per accepted connection, which is what a
/// [`ClientCtx`](super::dispatch::ClientCtx) carries and what the event fan-out
/// will address subscribers by.
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

/// How much one read may pull off a connection at a time. The frame cap is
/// enforced by the [`Decoder`], not by this buffer.
const READ_CHUNK: usize = 8 * 1024;

/// How many answered requests may wait for the wire before a connection stops
/// reading.
///
/// The reader's own backpressure: a client that pipelines requests without
/// reading the answers waits on its own socket instead of making the daemon
/// hold an unbounded pile of them. Events are bounded separately, by the
/// [`Outbound`] queue's two caps.
const MAX_PENDING_REPLIES: usize = 32;

/// What a connection does once everything it has queued has been written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterFlush {
    /// Close this connection and nothing else.
    Close,
    /// Write out whatever this connection is still owed, then close: step 7 of
    /// a shutdown, for a connection that asked for nothing.
    Drain,
    /// Write `daemon.stopped` and close: step 7 for the connection that asked,
    /// which is the one the report is owed to.
    Report,
}

/// The live per-account runtimes, keyed by account name (P3b-U4).
///
/// Empty for an account whose `start` is still in flight:
/// `daemon.status` reports `opening`
/// for exactly the accounts this table has nothing for, which is why an absent
/// entry is a state rather than a missing one.
///
/// It owns the runtimes, so it owns the engine locks they hold: the table lives
/// as long as the [`DaemonState`] does, and the locks come free when the daemon
/// exits.
#[derive(Debug, Default)]
pub struct RuntimeTable {
    entries: Mutex<BTreeMap<String, RuntimeEntry>>,
}

/// How an account's runtime is doing, as the diagnostics read it.
///
/// [`RuntimeTable::state_of`] answers `daemon.status`'s three words and cannot
/// tell a runtime that came up blocked from one that never came up at all: both
/// are `blocked` to a client that only paints a loading marker. A health report
/// has to tell them apart, because a second `mp` holding the engine lock is a
/// normal state of this machine and an account with no store is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeHealth {
    /// No start has come back yet.
    Opening,
    /// The runtime holds the lock and is serving.
    Ready,
    /// The runtime came up, and another engine holds the lock.
    Blocked(String),
    /// The runtime never came up at all.
    Failed(String),
}

impl RuntimeHealth {
    /// The word `daemon.status` and a health report's `accounts` both use.
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeHealth::Opening => "opening",
            RuntimeHealth::Ready => "ready",
            RuntimeHealth::Blocked(_) | RuntimeHealth::Failed(_) => "blocked",
        }
    }
}

/// What a start attempt left behind.
#[derive(Debug)]
enum RuntimeEntry {
    /// The runtime came up. Whether it holds the engine lock is its own
    /// [`Readiness`], since a contended lock is a successful start.
    Live(Arc<AccountRuntime>),
    /// The runtime could not be started at all: the account directory or the
    /// store is unusable. Reported as `blocked` with the reason on the event,
    /// because nothing about that account can be served.
    Failed(String),
}

/// Take a lock whose holder may have panicked; the map behind it is whole
/// either way.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RuntimeTable {
    /// Record a runtime that came up, ready or blocked.
    pub fn insert(&self, runtime: Arc<AccountRuntime>) {
        lock(&self.entries).insert(runtime.account().to_string(), RuntimeEntry::Live(runtime));
    }

    /// Record a start that failed outright.
    pub fn insert_failure(&self, account: &str, reason: String) {
        lock(&self.entries).insert(account.to_string(), RuntimeEntry::Failed(reason));
    }

    /// The state `daemon.status` reports for `account`, or `None` when no start
    /// has come back yet, which reads as `opening`.
    pub fn state_of(&self, account: &str) -> Option<&'static str> {
        Some(match lock(&self.entries).get(account)? {
            RuntimeEntry::Live(runtime) => match runtime.readiness() {
                Readiness::Opening => "opening",
                Readiness::Ready => "ready",
                Readiness::Blocked { .. } => "blocked",
            },
            RuntimeEntry::Failed(_) => "blocked",
        })
    }

    /// How `account` is doing, with a blocked runtime told apart from one that
    /// never came up (P6-U8).
    pub fn health_of(&self, account: &str) -> RuntimeHealth {
        match lock(&self.entries).get(account) {
            None => RuntimeHealth::Opening,
            Some(RuntimeEntry::Live(runtime)) => match runtime.readiness() {
                Readiness::Opening => RuntimeHealth::Opening,
                Readiness::Ready => RuntimeHealth::Ready,
                Readiness::Blocked { reason } => RuntimeHealth::Blocked(reason),
            },
            Some(RuntimeEntry::Failed(reason)) => RuntimeHealth::Failed(reason.clone()),
        }
    }

    /// Why `account` is not serving, whether its runtime came up blocked or
    /// never came up at all. `None` for an account that is ready or still
    /// opening. `daemon.status` carries only the state; the reason travels on
    /// the `account.state_changed` event and is kept here for the diagnostics
    /// that will ask for it.
    pub fn blocked_reason(&self, account: &str) -> Option<String> {
        match lock(&self.entries).get(account)? {
            RuntimeEntry::Live(runtime) => match runtime.readiness() {
                Readiness::Blocked { reason } => Some(reason),
                Readiness::Opening | Readiness::Ready => None,
            },
            RuntimeEntry::Failed(reason) => Some(reason.clone()),
        }
    }

    /// Take `account` out of the table, so dropping what comes back releases
    /// its engine lock and closes its store (P3b-U8).
    ///
    /// The drop is the caller's, on `spawn_blocking`: it closes SQLite, and a
    /// reload promises the lock is free by the time it answers. A start that
    /// only ever failed has nothing to hand back and is simply forgotten.
    pub fn remove(&self, account: &str) -> Option<Arc<AccountRuntime>> {
        match lock(&self.entries).remove(account)? {
            RuntimeEntry::Live(runtime) => Some(runtime),
            RuntimeEntry::Failed(_) => None,
        }
    }

    /// Empty the table, handing back every runtime that came up (P6-U4).
    ///
    /// Step 6 of the shutdown: dropping what comes back closes the stores and
    /// releases the engine locks, so the caller drops it on `spawn_blocking`
    /// exactly as [`RuntimeTable::remove`]'s caller does. A start that only
    /// ever failed holds nothing and is simply forgotten.
    pub fn take_all(&self) -> Vec<Arc<AccountRuntime>> {
        std::mem::take(&mut *lock(&self.entries))
            .into_values()
            .filter_map(|entry| match entry {
                RuntimeEntry::Live(runtime) => Some(runtime),
                RuntimeEntry::Failed(_) => None,
            })
            .collect()
    }

    /// The runtime serving `account`, for a caller that wants to tick it or
    /// read through its pool.
    pub fn get(&self, account: &str) -> Option<Arc<AccountRuntime>> {
        match lock(&self.entries).get(account)? {
            RuntimeEntry::Live(runtime) => Some(Arc::clone(runtime)),
            RuntimeEntry::Failed(_) => None,
        }
    }
}

/// Everything a served method may read. Immutable for the daemon's lifetime in
/// Phase 2, so it needs no lock; account runtimes will make it interior-mutable.
#[derive(Debug)]
pub struct DaemonState {
    /// What this daemon published in `daemon.json`.
    pub meta: InstanceMeta,
    /// The configuration this daemon owns (P3b-U8): the live snapshot, its
    /// revision, and the accounts every read method resolves against. Shared
    /// with the methods registered on [`DaemonState::dispatcher`], which hold
    /// the same store rather than a reference back to the state that owns
    /// them, so a swap is visible to all of them at once.
    pub config: Arc<ConfigStore>,
    /// Every domain method this build serves, registered once at startup and
    /// read-only afterwards.
    pub dispatcher: Dispatcher,
    /// The one canonical state of this daemon process: what `state.bootstrap`
    /// captures and what every connection's event queue is fed from. Held here
    /// so the connection loop can subscribe a socket, and held by the
    /// `state.bootstrap` method through its own `Arc` rather than a reference
    /// back to the state that owns the dispatcher.
    pub canonical: Arc<CanonicalState>,
    /// Every long-running operation this daemon has started (P3a-U8). Held
    /// here because two things outside the dispatcher reach it: the connection
    /// loop, which tells it about a disconnect, and the bootstrap snapshot,
    /// which projects its live entries.
    pub operations: Arc<OperationRegistry>,
    /// The account runtimes, filled in as each `start` comes back (P3b-U4,
    /// started by default since P5-U8). Behind an
    /// `Arc` because the `config.*` family reconciles it against a new
    /// configuration and must reach it without reaching back into the state
    /// that owns the dispatcher.
    pub runtimes: Arc<RuntimeTable>,
    /// The draft and signature watcher (P3b-U10). Held here because three
    /// things reach it: the poll loop the daemon spawns, `draft.approve`,
    /// which resolves an id through its settled inventory, and a
    /// configuration swap, which re-derives its roots.
    pub watch: Arc<super::watch::DraftWatch>,
    /// Every live materialised handle (P3b-U12). Held here because the
    /// retention sweep reads its pins from outside the dispatcher, and the
    /// three `message.*` handle methods hold the same table rather than a
    /// reference back to the state that owns them.
    pub handles: Arc<super::handles::HandleTable>,
    /// Every undo-send hold this daemon is carrying (P6-U2). Held here for the
    /// reason the operation registry is: the connection loop reaches it from
    /// outside the dispatcher, to apply the last-client rule.
    pub holds: Arc<super::hold::HoldScheduler>,
    /// This daemon's one graceful shutdown (P6-U4). Marked by the first
    /// `daemon.stop` or by a signal, read by every dispatch afterwards, and
    /// carrying the report the asking connection writes as its last frame.
    pub shutdown: Arc<super::shutdown::Shutdown>,
    /// This daemon's health checks, its log reader and its bundle writer
    /// (P6-U8). Held here because three things reach it: the `diagnostic.*`
    /// family, the canonical state, which projects the non-`ok` checks into
    /// every bootstrap snapshot, and the periodic sweep that re-evaluates them.
    pub diagnostics: Arc<super::diagnostics::Diagnostics>,
}

impl DaemonState {
    /// Assemble the state and register the domain methods on it.
    pub fn new(meta: InstanceMeta, config: Arc<ConfigStore>) -> Self {
        let configured = config.accounts();
        // Seeded from the configuration alone: no store is opened and no
        // runtime started here, so every account is `opening` with zeroed
        // counts until something reports otherwise.
        let canonical = Arc::new(CanonicalState::new(
            InstanceId::new(meta.instance_id.clone()),
            seeds_from_config(&configured),
        ));
        let runtimes = Arc::new(RuntimeTable::default());
        // Rooted at the configured accounts and unconditional: the watcher
        // opens no store and takes no engine lock, so it runs whether or not
        // this daemon starts account runtimes.
        let watch = Arc::new(super::watch::DraftWatch::new(
            super::watch::roots_from(&configured),
            super::watch::WatchConfig::from_env(),
        ));
        let operations = Arc::new(OperationRegistry::new());
        canonical.attach_operations(Arc::clone(&operations));
        // The registry emits events and stamps none: a revision belongs to the
        // canonical state. This is the fan-out that gives each one the next
        // revision and queues it for every bootstrapped connection. Both ends
        // are held weakly, so the closure the registry owns does not keep the
        // state and the registry alive through each other.
        let fanout_state = Arc::downgrade(&canonical);
        let fanout_operations = Arc::downgrade(&operations);
        operations.set_fanout(Arc::new(move || {
            let (Some(canonical), Some(operations)) =
                (fanout_state.upgrade(), fanout_operations.upgrade())
            else {
                return;
            };
            for event in operations.drain_events() {
                canonical.publish(event);
            }
        }));
        // One table per daemon process, with its lifetime read once at startup:
        // a handle minted under one lifetime and released under another would be
        // a promise this daemon changed its mind about.
        let handles = Arc::new(super::handles::HandleTable::from_env());
        let holds = Arc::new(super::hold::HoldScheduler::new());
        // Built here because it needs the instance metadata, the configuration
        // and the runtime table, and held by the canonical state so a bootstrap
        // can project the checks that are not `ok`. The state holds it back
        // weakly, so the two do not keep each other alive.
        let diagnostics = Arc::new(super::diagnostics::Diagnostics::new(
            meta.clone(),
            Arc::clone(&config),
            Arc::clone(&runtimes),
            Arc::clone(&operations),
            Arc::clone(&holds),
            &canonical,
        ));
        canonical.attach_diagnostics(Arc::clone(&diagnostics));
        // The snapshot's `holds` array is the scheduler's own listing: a client
        // that joins mid-window has no other way to learn about a countdown it
        // may cancel.
        canonical.attach_holds(Arc::clone(&holds));
        let mut dispatcher = Dispatcher::new();
        super::methods::register(
            &mut dispatcher,
            super::methods::Shared {
                config: Arc::clone(&config),
                runtimes: Arc::clone(&runtimes),
                canonical: Arc::clone(&canonical),
                operations: Arc::clone(&operations),
                watch: Arc::clone(&watch),
                handles: Arc::clone(&handles),
                holds: Arc::clone(&holds),
                diagnostics: Arc::clone(&diagnostics),
            },
        );
        DaemonState {
            meta,
            config,
            dispatcher,
            canonical,
            operations,
            runtimes,
            watch,
            handles,
            holds,
            shutdown: Arc::new(super::shutdown::Shutdown::new()),
            diagnostics,
        }
    }

    /// Run one tick on `account`'s runtime and commit what it did.
    ///
    /// The one place a real `sync.completed` is published. A tick that ran
    /// carries a [`SyncCompleted`](mp_protocol::events::SyncCompleted) built
    /// from the engine's own `SyncResult` and the mutation drains' failure
    /// count, and it is committed through
    /// [`CanonicalState::apply`](super::state::CanonicalState::apply), which
    /// stamps it with a fresh revision and fans it out to every bootstrapped
    /// connection without reducing anything into the snapshot.
    ///
    /// `None` for an account with no live runtime. A blocked tick and a joined
    /// one carry no outcome and commit nothing: the first ran no engine, and
    /// the second is reporting a tick whose runner commits it once.
    ///
    /// Nothing calls this on a schedule yet: the periodic tick is the Phase 5/6
    /// scheduler's. Since P4-U10 `sync.quick` and `sync.full` reach the same
    /// mechanism through [`tick_and_commit`], which is this method's body, held
    /// apart so a dispatched method can call it without reaching back into the
    /// state that owns the dispatcher.
    pub async fn tick_account(&self, account: &str, kind: TickKind) -> Option<TickOutcome> {
        tick_and_commit(&self.runtimes, &self.canonical, account, kind).await
    }

    /// The `result` of `daemon.status`.
    ///
    /// The CLI adds `"running": true` and prints the rest verbatim, so this
    /// object is the contract in `tests/daemon_lifecycle.rs` minus that key.
    ///
    /// Assembled by [`super::diagnostics::Diagnostics`], which holds the same
    /// three things it is built from and writes the same object into a support
    /// bundle's `daemon-status.json`: two renderers would eventually be two
    /// answers.
    pub fn status_result(&self) -> Value {
        self.diagnostics.status_result()
    }
}

/// Run one tick on `account`'s runtime and commit what it did.
///
/// The one place a real `sync.completed` is published. A tick that ran carries a
/// [`SyncCompleted`](mp_protocol::events::SyncCompleted) built from the engine's
/// own `SyncResult` and the mutation drains' failure count, and it is committed
/// through [`CanonicalState::apply`](super::state::CanonicalState::apply), which
/// stamps it with a fresh revision and fans it out to every bootstrapped
/// connection without reducing anything into the snapshot.
///
/// `None` for an account with no live runtime. A blocked tick and a joined one
/// carry no outcome and commit nothing: the first ran no engine, and the second
/// is reporting a tick whose runner commits it once.
pub async fn tick_and_commit(
    runtimes: &RuntimeTable,
    canonical: &CanonicalState,
    account: &str,
    kind: TickKind,
) -> Option<TickOutcome> {
    let runtime = runtimes.get(account)?;
    Some(commit_tick(&runtime, canonical, kind).await)
}

/// [`tick_and_commit`] on a runtime the caller already holds.
///
/// The account watcher's entry point: it ticks the runtime it was spawned for,
/// never whichever runtime the account's name resolves to now.
pub async fn commit_tick(
    runtime: &AccountRuntime,
    canonical: &CanonicalState,
    kind: TickKind,
) -> TickOutcome {
    let outcome = runtime.tick(kind).await;
    if let Some(sync) = outcome.sync.clone() {
        canonical.apply(Change::SyncCompleted(sync));
    }
    outcome
}

/// Accept connections until `shutdown` fires, then return.
///
/// `shutdown` fires at the end of the eight-step sequence [`super::shutdown`]
/// owns, not at the start of it: by the time this loop breaks the holds are
/// cancelled, the grace is over, the watchers and the account runtimes are
/// stopped, and the connection that asked has been handed its `daemon.stopped`
/// report. What is left for the caller is step 8, unlinking its own three
/// runtime files.
///
/// The connections themselves are closed by the process exiting rather than
/// one by one: a client that is owed a frame has already been given it, and
/// everything else on a socket at that point is an event about a daemon that
/// has already announced it is going.
pub async fn serve(
    listener: UnixListener,
    state: Arc<DaemonState>,
    shutdown: watch::Sender<bool>,
) -> Result<()> {
    let mut stop = shutdown.subscribe();
    info!(
        "[daemon] serving instance {} on {}",
        state.meta.instance_id,
        listener
            .local_addr()
            .ok()
            .and_then(|a| a.as_pathname().map(|p| p.display().to_string()))
            .unwrap_or_else(|| "<unnamed>".to_string())
    );

    loop {
        tokio::select! {
            changed = stop.changed() => {
                // A closed channel means every sender is gone, which cannot
                // happen while the caller holds one; either way, stop serving.
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    let state = Arc::clone(&state);
                    let shutdown = shutdown.clone();
                    // Wraps after 2^64 connections, which no daemon reaches.
                    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_connection(stream, connection_id, state, shutdown).await
                        {
                            debug!("[daemon] connection ended: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    // A failed accept is per-connection (EMFILE, ECONNABORTED)
                    // and never a reason to take the daemon down.
                    warn!("[daemon] accept failed: {e}");
                }
            },
        }
    }

    info!("[daemon] stopped accepting connections");
    Ok(())
}

/// Read frames off one connection until it closes or breaks the framing rules.
///
/// A framing error closes **this** connection and nothing else: the daemon has
/// no way to resynchronise a stream whose boundaries it has lost, but every
/// other client is unaffected.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    connection_id: u64,
    state: Arc<DaemonState>,
    shutdown: watch::Sender<bool>,
) -> Result<()> {
    // Subscribed on accept and attached to the fan-out by `state.bootstrap`:
    // the queue endpoint has to exist before the bootstrap that registers it,
    // and a connection that never bootstraps is fed nothing.
    let conn = ConnectionId(connection_id);
    let queue = state.canonical.subscribe(conn);
    // The queue owns the subscription and unsubscribes when its last handle
    // drops, so the connection task cannot leak an endpoint however it ends.
    // This one is kept past `serve_connection` on purpose: the cancellations a
    // disconnect causes are published while this connection is still a
    // subscriber, exactly as before, and therefore reach every other one in the
    // same fan-out.
    let result = serve_connection(stream, connection_id, &state, shutdown, queue.clone()).await;
    state.operations.on_disconnect(conn);
    drop(queue);
    // The last client leaving cancels every hold and leaves the drafts
    // approved, which is the plan's own rule and what killing the TUI does
    // today. After the unsubscribe, so the count is what it will be: a hold
    // that fired into an empty daemon would be a send nobody could still undo.
    // A shutdown reaches the same holds at its step 2, so a daemon that is
    // already going finds nothing here and says nothing.
    if state.canonical.subscriber_count() == 0 {
        let cancelled = state.holds.cancel_all(&state.canonical, &state.operations);
        if cancelled > 0 {
            info!(
                "[daemon] the last client left mid-hold: cancelled {cancelled} held send(s), \
                 their drafts are still approved"
            );
        }
    }
    result
}

/// The read/write loop of one connection, with its subscription already made
/// (P3a-U6).
///
/// Three things share this task and no other connection's: the reader, the
/// drain of this connection's [`EventQueue`] into its bounded [`Outbound`], and
/// the writer. They are three arms of one `select!` rather than three tasks
/// because they share the socket and the queue, and every arm is
/// cancellation-safe: `read` and `write` return only what they actually moved,
/// and [`EventQueue::ready`] re-checks the queue before it waits, so a change
/// committed while another arm ran is never missed.
///
/// The order of the three is what makes a stalled reader harmless. A blocked
/// write leaves its arm pending, so the drain keeps running and keeps the
/// unbounded [`EventQueue`] empty; what grows instead is the `Outbound`, which
/// coalesces, then overflows, then asks the client to bootstrap again. The
/// daemon's memory for a client that never reads is therefore
/// [`Subscriber::max_bytes`] and no more, and every other connection has its
/// own task and is untouched.
async fn serve_connection(
    stream: tokio::net::UnixStream,
    connection_id: u64,
    state: &Arc<DaemonState>,
    shutdown: watch::Sender<bool>,
    mut queue: EventQueue,
) -> Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut decoder = Decoder::new(MAX_REQUEST_BYTES);
    let mut buf = vec![0u8; READ_CHUNK];
    // One handshake per connection, so the session dies with the connection and
    // nothing has to expire it.
    let mut session = Session::new(connection_id);

    // The one buffer between the fan-out and this socket, at the caps the plan
    // fixes.
    let mut outbound = Outbound::new(Subscriber::default());
    // Answered requests waiting for the wire, encoded, in the order they were
    // answered.
    let mut replies: VecDeque<Vec<u8>> = VecDeque::new();
    // The frame being written, and how much of it the kernel has taken.
    let mut frame_out: Vec<u8> = Vec::new();
    let mut written = 0usize;
    let mut after_flush: Option<AfterFlush> = None;
    // Whether this connection asked the daemon to stop and is therefore owed
    // the `daemon.stopped` report. It keeps serving `daemon.status` and a
    // repeated `daemon.stop` while it waits: the report does not exist until
    // the grace is over, and a connection that stopped answering in the
    // meantime would be a `mp daemon stop` with nothing to watch.
    //
    // Owned as a guard rather than a flag: whichever way this task ends - the
    // report written, the peer gone, a write failed - the drop releases the
    // shutdown driver, once.
    let mut report_owed: Option<super::shutdown::ReportOwed> = None;
    let mut settled = state.shutdown.settled();

    loop {
        if written == frame_out.len() {
            if !frame_out.is_empty() {
                writer.flush().await.context("flushing a frame")?;
                frame_out.clear();
                written = 0;
            }
            // One whole frame at a time, an answer before an event: a response
            // frame and an event frame may not interleave on the wire.
            if let Some(reply) = replies.pop_front() {
                frame_out = reply;
            } else {
                match after_flush {
                    // Everything this connection was owed is on the wire and
                    // the shutdown has settled: the report is the last frame,
                    // and the socket closes behind it.
                    Some(AfterFlush::Report) => {
                        if let Some(frame) =
                            state.shutdown.report_frame(&state.meta.instance_id).await
                        {
                            writer
                                .write_all(&frame)
                                .await
                                .context("writing the shutdown report")?;
                            writer.flush().await.context("flushing the report")?;
                        }
                        drop(report_owed.take());
                        return Ok(());
                    }
                    Some(AfterFlush::Close) => return Ok(()),
                    // The events this connection was queued before the
                    // shutdown reached it, `daemon.shutting_down` among them,
                    // and then the EOF. Dropping them would make an orderly
                    // stop indistinguishable from a crash for every client
                    // that did not ask for it.
                    Some(AfterFlush::Drain) => match outbound.pop() {
                        Some(item) => {
                            frame_out = encode_outgoing(&item, &state.meta.instance_id)?;
                        }
                        None => return Ok(()),
                    },
                    None => {
                        if let Some(item) = outbound.pop() {
                            frame_out = encode_outgoing(&item, &state.meta.instance_id)?;
                        }
                    }
                }
            }
        }

        let writing = written < frame_out.len();
        let reading = after_flush.is_none() && replies.len() < MAX_PENDING_REPLIES;
        let closing = after_flush.is_none();
        tokio::select! {
            written_now = writer.write(&frame_out[written..]), if writing => {
                let n = written_now.context("writing a frame")?;
                if n == 0 {
                    // The peer will take nothing more, so neither the event nor
                    // the `state.resync_required` behind it can be delivered:
                    // this connection is over.
                    return Ok(());
                }
                written += n;
            }
            _ = queue.ready() => {
                for (revision, event) in drain_queue(&mut queue) {
                    outbound.push(revision, event);
                }
            }
            changed = settled.changed(), if closing => {
                // Step 7, on every connection: the one that asked writes the
                // report and the rest write out what they were queued, and
                // then all of them close. `changed` is cancellation-safe, so
                // losing this arm to another one never loses the signal, and a
                // closed channel means the daemon is gone from under us, which
                // ends the connection the same way.
                if changed.is_err() || *settled.borrow() {
                    // What the fan-out queued before the settle, the
                    // `daemon.shutting_down` among them, moves to the outbound
                    // buffer now: the drain below writes only that buffer, and
                    // this arm can win the race against the queue's own.
                    for (revision, event) in drain_queue(&mut queue) {
                        outbound.push(revision, event);
                    }
                    after_flush = Some(if report_owed.is_some() {
                        AfterFlush::Report
                    } else {
                        AfterFlush::Drain
                    });
                }
            }
            read = reader.read(&mut buf), if reading => {
                let read = read.context("reading a frame")?;
                if read == 0 {
                    return Ok(());
                }
                let frames = match decoder.push(&buf[..read]) {
                    Ok(frames) => frames,
                    Err(e) => {
                        let response = ErrorResponse {
                            jsonrpc: JSONRPC_VERSION.to_string(),
                            id: None,
                            error: frame_error(&e),
                        };
                        replies.push_back(frame::encode(&response)?);
                        after_flush = Some(AfterFlush::Close);
                        continue;
                    }
                };
                for value in frames {
                    let method = value
                        .get("method")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let owes_report = report_owed.is_some();
                    let (reply, stop) =
                        dispatch_request(value, state, &mut session, &shutdown, owes_report)
                            .await;
                    // Taken before anything below can fail: the dispatch just
                    // registered this connection as a reporter.
                    if stop && !owes_report {
                        report_owed = Some(state.shutdown.report_owed());
                    }
                    if let Some(reply) = reply {
                        if method.as_deref() == Some("state.bootstrap") {
                            rebootstrap(&mut outbound, &mut queue, &reply);
                        }
                        replies.push_back(encode_capped(&reply, MAX_RESPONSE_BYTES)?);
                    }
                }
            }
        }
    }
}

/// Start this connection's event stream again behind the snapshot it has just
/// been given.
///
/// Everything queued at this moment predates that snapshot, so it goes: the
/// poison clears with it, and a client that was told to resync is served again.
/// Called between the dispatch and the response frame, where no drain can run,
/// so the only changes that survive are those the fan-out committed after the
/// capture, which are exactly the ones the snapshot does not carry.
fn rebootstrap(outbound: &mut Outbound, queue: &mut EventQueue, reply: &Value) {
    let Some(captured) = reply.pointer("/result/revision").and_then(Value::as_u64) else {
        return;
    };
    outbound.rebootstrap();
    for (revision, event) in drain_queue(queue) {
        if revision.get() > captured {
            outbound.push(revision, event);
        }
    }
}

/// Everything this connection is owed, as events, in revision order.
///
/// Two queues feed it - committed changes and lifecycle events - and
/// [`Outbound::push`] is offered its items in non-decreasing revision order, so
/// [`EventQueue::drain_all`] empties both under both locks at once and merges
/// them by revision. Two separate drains would not do: a change committed
/// between the two acquisitions arrives behind a lifecycle event published
/// after it, and the client drops it as a duplicate.
fn drain_queue(queue: &mut EventQueue) -> Vec<(Revision, Event)> {
    queue.drain_all()
}

/// One outbound item as the frame it travels in: a `state.event` notification
/// carrying the [`EventEnvelope`] `docs/daemon-protocol.md` fixes, or the
/// `state.resync_required` control notification.
fn encode_outgoing(item: &Outgoing, instance_id: &str) -> Result<Vec<u8>> {
    let notification = match item {
        Outgoing::Event(revision, event) => Notification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: METHOD_STATE_EVENT.to_string(),
            params: serde_json::to_value(EventEnvelope {
                instance_id: instance_id.to_string(),
                revision: revision.get(),
                kind: event.kind().to_string(),
                payload: event.payload(),
            })
            .context("serialising an event envelope")?,
        },
        Outgoing::ResyncRequired { reason } => Notification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: METHOD_STATE_RESYNC_REQUIRED.to_string(),
            params: json!({"instance_id": instance_id, "reason": reason}),
        },
    };
    Ok(frame::encode(&notification)?)
}

/// Answer one frame.
///
/// Returns the message to write back (`None` for a notification, which by
/// JSON-RPC gets no answer) and whether this connection now owes its peer a
/// `daemon.stopped` report, which is true of every frame that asked the daemon
/// to stop.
///
/// `owes_report` is whether it already did. A connection that stops a daemon
/// twice is one reporter and not two, or the driver would sit out its ceiling
/// waiting for a second frame nobody is going to write.
async fn dispatch_request(
    value: Value,
    state: &Arc<DaemonState>,
    session: &mut Session,
    exit: &watch::Sender<bool>,
    owes_report: bool,
) -> (Option<Value>, bool) {
    let request: Request = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(e) => {
            return (
                Some(error_value(
                    None,
                    INVALID_REQUEST,
                    format!("not a JSON-RPC request: {e}"),
                    None,
                )),
                false,
            )
        }
    };

    if request.jsonrpc != JSONRPC_VERSION {
        return (
            Some(error_value(
                request.id.clone(),
                INVALID_REQUEST,
                format!("jsonrpc must be \"{JSONRPC_VERSION}\""),
                None,
            )),
            false,
        );
    }

    // The shutdown gate, ahead of the handshake gate on purpose: the handshake
    // gate exempts `initialize` by construction, and a client that negotiated
    // its way into a daemon that is leaving would be admitted to a session
    // whose socket is about to close. The two lifecycle methods keep
    // answering, because `daemon.status` is how `mp daemon stop` watches the
    // shutdown it asked for and a second `daemon.stop` is not an error.
    if state.shutdown.is_stopping() && !super::session::is_lifecycle_method(&request.method) {
        return (
            Some(error_value(
                request.id,
                ErrorCode::ShuttingDown.code(),
                "the daemon is shutting down".to_string(),
                Some(json!({})),
            )),
            false,
        );
    }

    // The handshake gate: every method except `initialize` and the two
    // lifecycle methods is `-32000 not_initialized` until this connection has
    // handshaken. The refusal is per request, so the connection stays usable
    // and the `initialize` that should have come first still works on it.
    if let Some(refusal) = session.gate(&request.method) {
        return (
            Some(error_value(
                request.id,
                refusal.code,
                refusal.message,
                refusal.data,
            )),
            false,
        );
    }

    // The id and the method are copied out because the domain arm hands the
    // whole request to the dispatcher, which takes it by value.
    let id = request.id.clone();
    let method = request.method.clone();
    match method.as_str() {
        "initialize" => match session.initialize(&request.params, state) {
            Ok(result) => (result_value(id, result), false),
            Err(refusal) => (
                Some(error_value(id, refusal.code, refusal.message, refusal.data)),
                false,
            ),
        },
        "daemon.status" => (result_value(id, state.status_result()), false),
        "daemon.stop" => match stop_grace(&request.params) {
            // Steps 1 to 4 run here, synchronously, so the answer cannot
            // describe a daemon that has already moved past them; the rest is
            // the driver's, and this connection waits for its report.
            Ok(grace) => (
                result_value(
                    id,
                    super::shutdown::request(state, exit, grace, !owes_report),
                ),
                true,
            ),
            Err(refusal) => (
                Some(error_value(id, refusal.code, refusal.message, refusal.data)),
                false,
            ),
        },
        _ => {
            // The gate above already refused every unhandshaken connection, so
            // a session without a context here is a daemon bug rather than a
            // client's mistake.
            let Some(ctx) = session.client_ctx() else {
                return (
                    Some(error_value(
                        id,
                        INTERNAL_ERROR,
                        "the connection passed the handshake gate without a client context"
                            .to_string(),
                        None,
                    )),
                    false,
                );
            };
            match state.dispatcher.dispatch(&ctx, request).await {
                // `revision` and `affected` are dropped here until the first
                // Command method exists (P3a-U8, then Phase 3b) and they become
                // the events its caller and every other client receive; a
                // query, which is all this build serves, carries neither.
                Ok(outcome) => (result_value(id, outcome.result), false),
                Err(error) => {
                    let refusal = RpcError::from(error);
                    (
                        Some(error_value(id, refusal.code, refusal.message, refusal.data)),
                        false,
                    )
                }
            }
        }
    }
}

/// A `Response`, or nothing when the request carried no id (a notification).
///
/// Built by moving `result` into the object rather than through
/// `serde_json::to_value(Response { .. })`, which serialises the whole result
/// tree into a second copy: 5 to 12 ms for a 5000-row `message.list`, and every
/// byte of a large `message.get` body. The frame is byte-identical to that
/// path's either way serde_json is built: without `preserve_order` both sort
/// the keys, and with it both keep [`Response`]'s field order, which is the
/// order they are inserted in here.
fn result_value(id: Option<RequestId>, result: Value) -> Option<Value> {
    let id = serde_json::to_value(id?).ok()?;
    let mut response = serde_json::Map::with_capacity(3);
    response.insert(
        "jsonrpc".to_string(),
        Value::String(JSONRPC_VERSION.to_string()),
    );
    response.insert("id".to_string(), id);
    response.insert("result".to_string(), result);
    Some(Value::Object(response))
}

/// An `ErrorResponse` as a `Value`, so both arms of a dispatch share one type.
fn error_value(id: Option<RequestId>, code: i32, message: String, data: Option<Value>) -> Value {
    let response = ErrorResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id,
        error: RpcError {
            code,
            message,
            data,
        },
    };
    serde_json::to_value(response).unwrap_or_else(|_| {
        json!({"jsonrpc": JSONRPC_VERSION, "error": {"code": code, "message": "internal error"}})
    })
}

/// Encode one reply, or the refusal that replaces it when it is too big.
///
/// The client decodes with the same cap, so a reply above it would be a frame
/// the reader rejects and a connection that dies mid-answer. Sending
/// `frame_too_large` instead keeps the failure a named error carrying the cap
/// and the size that breached it, on the id the caller is waiting for, which is
/// the same pair the decoder reports for an oversized request.
fn encode_capped(reply: &Value, cap: usize) -> Result<Vec<u8>> {
    let bytes = frame::encode(reply)?;
    if bytes.len() <= cap {
        return Ok(bytes);
    }
    warn!(
        "[daemon] a {}-byte response exceeds the {cap}-byte cap; answering frame_too_large",
        bytes.len()
    );
    let id = reply
        .get("id")
        .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok());
    let refusal = error_value(
        id,
        ErrorCode::FrameTooLarge.code(),
        format!(
            "the response is {} bytes, over the {cap}-byte response cap",
            bytes.len()
        ),
        Some(json!({"limit": cap, "seen": bytes.len()})),
    );
    Ok(frame::encode(&refusal)?)
}

/// The grace a `daemon.stop` asked for, in seconds.
///
/// `None` for a stop that named none, which takes
/// [`DEFAULT_STOP_GRACE_SECS`](super::shutdown::DEFAULT_GRACE_SECS). An
/// explicit `0` is honoured as "no waiting at all" rather than read as unset,
/// which is the difference between a parameter and one of the daemon's ambient
/// numeric environment hooks.
fn stop_grace(params: &Value) -> Result<Option<u64>, RpcError> {
    match params.get("grace_secs") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| RpcError {
            code: INVALID_PARAMS,
            message: format!("grace_secs must be a whole number of seconds, not {value}"),
            data: None,
        }),
    }
}

/// Map a framing failure onto the wire error that describes it.
fn frame_error(error: &FrameError) -> RpcError {
    match error {
        FrameError::TooLarge { limit, seen } => RpcError {
            code: mp_protocol::ErrorCode::FrameTooLarge.code(),
            message: error.to_string(),
            data: Some(json!({"limit": limit, "seen": seen})),
        },
        FrameError::InvalidUtf8 | FrameError::InvalidJson(_) => RpcError {
            code: PARSE_ERROR,
            message: error.to_string(),
            data: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon state whose configuration is `config`, at revision 0.
    fn state_from(state: super::super::config::ConfigState, accounts: &[&str]) -> DaemonState {
        use std::path::PathBuf;
        let config = crate::config::GlobalConfig {
            accounts: accounts
                .iter()
                .map(|name| crate::config::AccountConfig {
                    name: name.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        DaemonState::new(
            InstanceMeta {
                app_version: "0.0.0-test".to_string(),
                protocol_min: mp_protocol::PROTOCOL_MIN,
                protocol_max: mp_protocol::PROTOCOL_MAX,
                instance_id: "abcd".to_string(),
                pid: 42,
                started_at: "2026-01-01T00:00:00Z".to_string(),
                data_dir: PathBuf::from("/tmp/data"),
                config_dir: PathBuf::from("/tmp/config"),
            },
            Arc::new(ConfigStore::new(
                PathBuf::from("/tmp/config/config.toml"),
                state,
                config,
                false,
            )),
        )
    }

    /// A daemon state with no accounts at all.
    fn state_fixture() -> Arc<DaemonState> {
        Arc::new(state_from(super::super::config::ConfigState::Absent, &[]))
    }

    /// [`dispatch_request`] with the two arguments no unit test varies: an
    /// exit channel nobody is serving, and a connection that owes no report.
    async fn dispatch(
        value: Value,
        state: &Arc<DaemonState>,
        session: &mut Session,
    ) -> (Option<Value>, bool) {
        let (exit, _held) = watch::channel(false);
        dispatch_request(value, state, session, &exit, false).await
    }

    /// The same, with one configured account, so the canonical state has a
    /// seed a change can be addressed to.
    fn state_with_one_account(name: &str) -> DaemonState {
        state_from(super::super::config::ConfigState::Ok, &[name])
    }

    /// Two refreshes either side of a status flip publish the two statuses in
    /// the order they were evaluated. Sequential rather than raced: the
    /// runtime table cannot be made to flip mid-evaluation from a test, so this
    /// pins the order the serialised refresh guarantees, not the race itself.
    #[test]
    fn refreshes_publish_an_account_check_in_evaluation_order() {
        let state = state_with_one_account("alpha");
        let conn = ConnectionId(7);
        let mut queue = state.canonical.subscribe(conn);
        let _ = state.canonical.bootstrap(conn);

        state.diagnostics.refresh();
        state.runtimes.insert_failure("alpha", "boom".to_string());
        state.diagnostics.refresh();

        let statuses: Vec<String> = queue
            .drain_all()
            .into_iter()
            .map(|(_, event)| event.payload())
            .filter(|payload| payload["name"] == "account:alpha")
            .map(|payload| payload["status"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(statuses, ["warn", "fail"]);
    }

    /// A tick whose body fails, so the commit path is exercised without an
    /// IMAP server: the hooks are injected, and the failure is what makes the
    /// committed severity distinguishable from a default.
    #[tokio::test]
    async fn a_tick_commits_its_outcome_as_one_sync_completed_event() {
        use super::super::runtime::account::{BodyHook, DrainHook, TickContext, TickHooks};
        use futures::future::FutureExt;
        use mp_protocol::events::{Severity, SyncCompleted, KIND_SYNC_COMPLETED};

        let state = state_with_one_account("alpha");
        let dir = tempfile::tempdir().expect("tempdir");
        let quiet: DrainHook = Arc::new(|_ctx: TickContext| async { String::new() }.boxed());
        let body: BodyHook =
            Arc::new(|_ctx: TickContext| async { Err(anyhow::anyhow!("login refused")) }.boxed());
        let runtime = AccountRuntime::start_at_with_hooks(
            dir.path(),
            crate::config::AccountConfig {
                name: "alpha".to_string(),
                ..Default::default()
            },
            1,
            TickHooks {
                outbox: Arc::clone(&quiet),
                mutations: quiet,
                body,
            },
        )
        .expect("a runtime starts against a free lock");
        state.runtimes.insert(Arc::new(runtime));

        let conn = ConnectionId(9);
        let mut queue = state.canonical.subscribe(conn);
        let (_, bootstrap, _) = state.canonical.bootstrap(conn);

        let outcome = state
            .tick_account("alpha", TickKind::Quick)
            .await
            .expect("a live runtime ticks");
        assert_eq!(outcome.error.as_deref(), Some("login refused"));

        let drained = queue.drain_all();
        assert_eq!(drained.len(), 1, "one tick is one event: {drained:?}");
        let (revision, event) = &drained[0];
        assert!(revision.get() > bootstrap.get());
        assert_eq!(event.kind(), KIND_SYNC_COMPLETED);
        assert!(
            event.is_lifecycle(),
            "an outcome merges with nothing and survives an overflow"
        );
        let payload: SyncCompleted =
            serde_json::from_value(event.payload()).expect("a typed outcome");
        assert_eq!(payload.account, "alpha");
        assert_eq!(payload.severity, Severity::Error);
        assert_eq!(payload.error.as_deref(), Some("login refused"));
    }

    /// An account with no runtime has no tick, and therefore nothing to
    /// commit: a fabricated clean outcome would be a sync that never ran.
    #[tokio::test]
    async fn an_account_without_a_runtime_ticks_nothing_and_commits_nothing() {
        let state = state_with_one_account("alpha");
        let before = state.canonical.revision();
        assert!(state.tick_account("alpha", TickKind::Quick).await.is_none());
        assert_eq!(state.canonical.revision(), before);
    }

    fn request(method: &str, id: Option<i64>) -> Value {
        let mut value = json!({"jsonrpc": "2.0", "method": method, "params": {}});
        if let Some(id) = id {
            value["id"] = json!(id);
        }
        value
    }

    /// `daemon.status` answers before any handshake, which is what lets
    /// `mp daemon status` describe an incompatible daemon.
    #[tokio::test]
    async fn status_answers_without_an_initialize() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let (reply, stop) = dispatch(request("daemon.status", Some(7)), &state, &mut session).await;
        let reply = reply.expect("a request with an id is answered");
        assert!(!stop);
        assert_eq!(reply["id"], json!(7));
        assert_eq!(reply["result"]["instance_id"], json!("abcd"));
        assert_eq!(reply["result"]["accounts"], json!([]));
    }

    /// `daemon.stop` is answered first and only then ends the daemon, and the
    /// answer names the grace it will honour and the work it has to settle.
    #[tokio::test]
    async fn stop_replies_before_it_shuts_down() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let (reply, stop) = dispatch(request("daemon.stop", Some(1)), &state, &mut session).await;
        assert!(stop, "daemon.stop asks for a shutdown");
        let result = reply.expect("answered")["result"].clone();
        assert_eq!(result["stopping"], json!(true));
        assert_eq!(
            result["grace_secs"],
            json!(super::super::shutdown::DEFAULT_GRACE_SECS),
            "a stop that named no grace gets the default: {result}"
        );
        assert_eq!(
            result["pending"],
            json!([]),
            "and an idle daemon has nothing to settle: {result}"
        );
    }

    /// A stop that named a grace is answered with that one, and a stop of a
    /// stopping daemon is answered rather than refused.
    #[tokio::test]
    async fn a_named_grace_is_the_one_the_answer_carries() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let mut stop = request("daemon.stop", Some(1));
        stop["params"] = json!({"grace_secs": 3});
        let (reply, _) = dispatch(stop, &state, &mut session).await;
        assert_eq!(reply.expect("answered")["result"]["grace_secs"], json!(3));

        let (again, _) = dispatch(request("daemon.stop", Some(2)), &state, &mut session).await;
        let result = again.expect("answered")["result"].clone();
        assert_eq!(
            result["grace_secs"],
            json!(3),
            "a second stop reads what the first settled rather than deciding again: {result}"
        );
        assert_eq!(result["stopping"], json!(true));
    }

    /// A grace that is not a whole number of seconds is `-32602`, because a
    /// stop that silently ignored it would wait for a length nobody asked for.
    #[tokio::test]
    async fn a_grace_that_is_not_a_number_of_seconds_is_invalid_params() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let mut stop = request("daemon.stop", Some(1));
        stop["params"] = json!({"grace_secs": "soon"});
        let (reply, shutting) = dispatch(stop, &state, &mut session).await;
        assert!(!shutting, "a refused stop stops nothing");
        assert_eq!(reply.expect("answered")["error"]["code"], json!(-32602));
        assert!(!state.shutdown.is_stopping());
    }

    /// Once the stop is accepted, every other method is `-32009` with the one
    /// sentence a client can print, `initialize` included: the handshake gate
    /// exempts it, so the refusal has to sit ahead of that gate.
    #[tokio::test]
    async fn a_stopping_daemon_refuses_everything_but_the_two_lifecycle_methods() {
        let state = state_fixture();
        let mut session = Session::new(1);
        dispatch(request("daemon.stop", Some(1)), &state, &mut session).await;

        for method in ["initialize", "account.list", "state.bootstrap"] {
            let (reply, _) = dispatch(request(method, Some(2)), &state, &mut session).await;
            let error = reply.expect("answered")["error"].clone();
            assert_eq!(
                error["code"],
                json!(ErrorCode::ShuttingDown.code()),
                "{method} during the grace: {error}"
            );
            assert_eq!(error["message"], json!("the daemon is shutting down"));
        }

        let (status, _) = dispatch(request("daemon.status", Some(3)), &state, &mut session).await;
        assert_eq!(
            status.expect("answered")["result"]["instance_id"],
            json!("abcd"),
            "daemon.status keeps answering: it is how a stop is watched"
        );
    }

    /// A domain method before the handshake is `-32000`, not `-32601`: the gate
    /// runs before the method lookup, so an uninitialized client cannot probe
    /// which methods a daemon serves.
    #[tokio::test]
    async fn a_domain_method_before_initialize_is_not_initialized() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let (reply, stop) = dispatch(request("account.list", Some(2)), &state, &mut session).await;
        assert!(!stop);
        assert_eq!(reply.expect("answered")["error"]["code"], json!(-32000));
    }

    /// After the handshake, a method no family serves is `-32601` again.
    #[tokio::test]
    async fn an_unknown_method_after_initialize_is_method_not_found() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "client": {"type": "cli", "version": "0.9.0"},
                "protocol": {"min": 1, "max": 1},
                "capabilities": {"required": [], "optional": []},
                "identity": {"data_dir": "/tmp/data", "config_dir": "/tmp/config"},
            },
        });
        let (reply, _) = dispatch(initialize, &state, &mut session).await;
        assert!(
            reply.expect("answered")["result"]["instance_id"] == json!("abcd"),
            "the handshake succeeds against the fixture's own directories"
        );

        let (reply, stop) =
            dispatch(request("no.such.method", Some(2)), &state, &mut session).await;
        assert!(!stop);
        assert_eq!(reply.expect("answered")["error"]["code"], json!(-32601));
    }

    /// A reply over the cap becomes `frame_too_large` on the id the caller is
    /// waiting for, carrying the cap and the size that breached it. The cap is
    /// a parameter of [`encode_capped`] rather than a constant it reads, which
    /// is what makes this checkable without a 16 MiB fixture.
    #[test]
    fn a_response_over_the_cap_becomes_frame_too_large() {
        let reply = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {"messages": "x".repeat(4096)},
        });
        let oversized = frame::encode(&reply).expect("encodes").len();
        let bytes = encode_capped(&reply, 256).expect("an oversized reply still encodes");
        assert!(bytes.len() <= 256, "the refusal itself fits in the cap");
        let sent: Value = serde_json::from_str(
            std::str::from_utf8(&bytes)
                .expect("a frame is UTF-8")
                .trim_end(),
        )
        .expect("a JSON frame");
        assert_eq!(sent["id"], json!(9), "the refusal answers the same request");
        assert_eq!(
            sent["error"]["code"],
            json!(ErrorCode::FrameTooLarge.code())
        );
        assert_eq!(
            sent["error"]["data"],
            json!({"limit": 256, "seen": oversized})
        );
        assert!(sent.get("result").is_none(), "got {sent}");
    }

    /// A reply under the cap goes out untouched.
    #[test]
    fn a_response_under_the_cap_is_written_verbatim() {
        let reply = json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}});
        let bytes = encode_capped(&reply, MAX_RESPONSE_BYTES).expect("encodes");
        assert_eq!(bytes, frame::encode(&reply).expect("encodes"));
    }

    /// The moved-in reply is the frame the old `to_value(Response)` gave, byte for byte,
    /// whatever the id's type and however deep the result.
    #[test]
    fn a_result_value_is_byte_identical_to_the_serialised_response() {
        let result = json!({
            "rows": [{"id": "inbox/1", "subject": "z", "flags": ["\\Seen"]}],
            "total": 1,
            "a": null,
        });
        for id in [RequestId::Num(7), RequestId::Str("req-1".to_string())] {
            // The path this replaced: the struct into a `Value`, then out.
            let expected = serde_json::to_vec(
                &serde_json::to_value(mp_protocol::Response {
                    jsonrpc: JSONRPC_VERSION.to_string(),
                    id: id.clone(),
                    result: result.clone(),
                })
                .expect("converts"),
            )
            .expect("serialises");
            let built = result_value(Some(id), result.clone()).expect("an id was given");
            assert_eq!(serde_json::to_vec(&built).expect("serialises"), expected);
        }
        assert!(result_value(None, result).is_none());
    }

    /// A request without an id is a notification: no answer, no shutdown.
    #[tokio::test]
    async fn a_notification_gets_no_response() {
        let state = state_fixture();
        let mut session = Session::new(1);
        let (reply, stop) = dispatch(request("daemon.status", None), &state, &mut session).await;
        assert!(reply.is_none());
        assert!(!stop);
    }
}
