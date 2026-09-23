//! Graceful shutdown: the eight steps a `daemon.stop` or a SIGTERM runs
//! (P6-U4, ticket #0125).
//!
//! The plan's sentence is *"stops accepting commands, settles or checkpoints
//! active operations, closes watchers and connections, removes only its own
//! socket, releases locks"*, and the order it runs in is the contract, because
//! it is what makes each of those clauses observable:
//!
//! 1. mark shutting down - from here every method but `daemon.status` and a
//!    repeated `daemon.stop` is `-32009`, `initialize` included;
//! 2. cancel every armed hold, publishing `send.hold_cancelled`, drafts left
//!    `approved`;
//! 3. publish `daemon.shutting_down` to every bootstrapped connection;
//! 4. answer the `daemon.stop` with the effective grace and what is still live;
//! 5. wait up to the grace for those operations to settle, cancelling whatever
//!    is left;
//! 6. stop the watchers and the account runtimes, releasing the engine locks;
//! 7. send `daemon.stopped` on the asking connection and close every
//!    connection;
//! 8. unlink its own socket, `daemon.pid` and `daemon.json`, and exit 0.
//!
//! Steps 1 to 4 run inside the connection task that answered, synchronously, so
//! the answer cannot describe a daemon that has already moved on; steps 5 and 6
//! run in the driver task [`request`] spawns, and step 7 is split between the
//! driver, which publishes the report, and the asking connection, which writes
//! it as its last frame.
//!
//! # Why a hold is never in `pending`
//!
//! Step 2 is ahead of step 4, so an armed hold is cancelled rather than waited
//! out. A daemon that sat out the remaining seconds and then sent would send
//! mail nobody could stop any more, because the client that would have pressed
//! `u` is losing its socket in the same second. That is the plan's *"settled or
//! cancelled, never silently dropped"* for a window whose whole point is that
//! the user may still close it.
//!
//! # Why the grace is a ceiling and never a sleep
//!
//! With nothing live at step 4 the daemon does not enter the grace at all, and
//! [`wait_out`] returns the moment the registry has nothing left. Every suite
//! in this tree stops an idle daemon, and a stop that cost ten seconds because
//! ten is the default would make all of them ten seconds slower for nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use log::{info, warn};
use serde_json::{json, Value};
use tokio::sync::{watch, Notify};

use mp_protocol::events::KIND_DAEMON_SHUTTING_DOWN;
use mp_protocol::{frame, Notification, JSONRPC_VERSION, METHOD_DAEMON_STOPPED};

use super::operations::OperationId;
use super::server::DaemonState;
use super::state::events::Event;

/// The grace a `daemon.stop` that named none gets, in seconds.
///
/// A parameter is explicit where an environment hook is ambient, so an
/// explicit `0` means "no waiting at all" rather than "unset", which is how the
/// daemon's numeric env hooks read a zero.
pub const DEFAULT_GRACE_SECS: u64 = 10;

/// How often step 5 re-checks whether the registry has emptied.
const POLL: Duration = Duration::from_millis(25);

/// The ceiling on how long step 7 waits for the connection that asked to stop
/// to take its `daemon.stopped` report, before the daemon exits without it.
///
/// Only a connection still alive and not reading is ever waited on this long:
/// one that went away, by EOF or by a failed write, releases the driver as its
/// [`ReportOwed`] drops. The frame is one small write into a socket buffer the
/// peer can still drain after this process is gone, so a live reporter that
/// has not taken it by now is a client that stopped reading.
const REPORT_TAKE_CEILING: Duration = Duration::from_secs(2);

/// The ceiling on how long step 7 waits for the other connections to write
/// out what they were queued and close, before the daemon exits under them.
const CONNECTIONS_CLOSE_CEILING: Duration = Duration::from_secs(2);

/// What the daemon reports about its own shutdown, on the connection that asked
/// for it.
#[derive(Clone, Debug)]
pub struct Report {
    /// Whether everything settled inside the grace.
    pub clean: bool,
    /// What did not, as `operation.status` objects in start order, captured
    /// before they were cancelled.
    pub unsettled: Vec<Value>,
}

/// The one shutdown of one daemon process.
///
/// Held by [`DaemonState`] because three things outside any one connection
/// reach it: the dispatcher, which refuses work once it is marked; the signal
/// watch, which starts the same sequence with no connection to answer; and the
/// draft watcher, which stops when step 6 says so.
#[derive(Debug)]
pub struct Shutdown {
    inner: Mutex<Inner>,
    /// `true` once step 6 is done and [`Inner::report`] is there to be read.
    ///
    /// Written with `send_replace` and never with `send`, here and below: a
    /// `watch::Sender` with no live receiver refuses a `send` and leaves the
    /// value as it was, and both of these are legitimately sent into a daemon
    /// nobody has subscribed to yet.
    settled: watch::Sender<bool>,
    /// `true` once step 6 has begun, which is what stops the watcher loop.
    watchers: watch::Sender<bool>,
    /// Connections that asked to stop and owe their peer a `daemon.stopped`.
    reporters: AtomicUsize,
    /// Notified by each reporter once it has written that frame.
    reported: Notify,
}

#[derive(Debug, Default)]
struct Inner {
    /// Whether step 1 has run, which is the whole of "stops accepting
    /// commands".
    stopping: bool,
    /// The grace the first stop settled on, which a second stop is answered
    /// with rather than re-deciding.
    grace_secs: u64,
    /// What was live at step 4, for the answer and the announcement.
    pending: Vec<Value>,
    /// The outcome of steps 5 and 6, `None` until they are done.
    report: Option<Report>,
}

impl Default for Shutdown {
    fn default() -> Self {
        Shutdown {
            inner: Mutex::new(Inner::default()),
            settled: watch::channel(false).0,
            watchers: watch::channel(false).0,
            reporters: AtomicUsize::new(0),
            reported: Notify::new(),
        }
    }
}

impl Shutdown {
    /// A daemon that is not stopping.
    pub fn new() -> Shutdown {
        Shutdown::default()
    }

    /// Whether step 1 has run, which is what the `-32009` refusal reads.
    pub fn is_stopping(&self) -> bool {
        lock(&self.inner).stopping
    }

    /// The grace the shutdown is honouring, in seconds.
    pub fn grace_secs(&self) -> u64 {
        lock(&self.inner).grace_secs
    }

    /// A receiver that goes `true` when step 6 begins: the draft watcher's stop
    /// signal.
    pub fn watchers_stopped(&self) -> watch::Receiver<bool> {
        self.watchers.subscribe()
    }

    /// A receiver that goes `true` once the report exists, which is what a
    /// connection owed one waits on without giving up its read loop.
    pub fn settled(&self) -> watch::Receiver<bool> {
        self.settled.subscribe()
    }

    /// Register this connection as one that owes its peer a report.
    fn expect_report(&self) {
        self.reporters.fetch_add(1, Ordering::SeqCst);
    }

    /// Wait until the report exists, then answer the frame that carries it.
    ///
    /// `None` when the shutdown ended without one, which cannot happen while
    /// this daemon holds the sender but is not worth a panic inside a
    /// connection task.
    pub async fn report_frame(&self, instance_id: &str) -> Option<Vec<u8>> {
        let mut settled = self.settled.subscribe();
        while !*settled.borrow() {
            if settled.changed().await.is_err() {
                break;
            }
        }
        let report = lock(&self.inner).report.clone()?;
        let notification = Notification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: METHOD_DAEMON_STOPPED.to_string(),
            params: json!({
                "instance_id": instance_id,
                "clean": report.clean,
                "unsettled": report.unsettled,
            }),
        };
        frame::encode(&notification).ok()
    }

    /// Say that one reporter is done with its frame, releasing the driver.
    ///
    /// Only [`ReportOwed`]'s drop calls it, which is what makes it exactly
    /// once per registered reporter whichever way the connection ended.
    fn reported(&self) {
        self.reporters.fetch_sub(1, Ordering::SeqCst);
        self.reported.notify_waiters();
    }

    /// The debt a connection took on when its `daemon.stop` registered it as a
    /// reporter ([`request`] with `reporter: true`), owned by that connection
    /// from then on.
    pub fn report_owed(self: &Arc<Self>) -> ReportOwed {
        ReportOwed(Arc::clone(self))
    }
}

/// One connection's `daemon.stopped` report, still owed.
///
/// Dropping it releases the driver, once: after the report was written, and
/// equally when the peer hung up first (a Ctrl-C on `mp daemon stop`, a call
/// timeout) or a write failed. Without it, only the path that wrote the report
/// released the driver, and every other one cost the shutdown its whole
/// [`REPORT_TAKE_CEILING`] and a warning about a client that never took it.
#[must_use = "dropping the debt at once releases the driver before the report is written"]
pub struct ReportOwed(Arc<Shutdown>);

impl Drop for ReportOwed {
    fn drop(&mut self) {
        self.0.reported();
    }
}

/// Steps 1 to 4, and the driver for the rest: the whole of `daemon.stop`.
///
/// Idempotent by design. A second stop is answered rather than refused - it is
/// how `mp daemon stop` behaves against a daemon a service manager is already
/// stopping - and it re-reads the grace and the pending list the first one
/// settled instead of restarting the sequence.
///
/// `reporter` is whether the caller is a connection that will write the
/// `daemon.stopped` frame; a signal has none.
pub fn request(
    state: &Arc<DaemonState>,
    exit: &watch::Sender<bool>,
    grace_secs: Option<u64>,
    reporter: bool,
) -> Value {
    let shutdown = Arc::clone(&state.shutdown);
    if reporter {
        shutdown.expect_report();
    }

    let first = {
        let mut inner = lock(&shutdown.inner);
        let first = !inner.stopping;
        if first {
            // Step 1. From here `dispatch_request` refuses everything but the
            // two lifecycle methods, so nothing new can join the pending list
            // between this line and the capture below.
            inner.stopping = true;
            inner.grace_secs = grace_secs.unwrap_or(DEFAULT_GRACE_SECS);
        }
        first
    };

    if first {
        // Step 2, ahead of the capture: a cancelled hold settles its own
        // operation, so it is never work the grace has to wait for.
        let cancelled = state.holds.cancel_all(&state.canonical, &state.operations);
        if cancelled > 0 {
            info!(
                "[daemon] shutting down mid-hold: cancelled {cancelled} held send(s), their \
                 drafts are still approved"
            );
        }
        let pending = live_operations(state);
        let grace = {
            let mut inner = lock(&shutdown.inner);
            inner.pending = pending.clone();
            inner.grace_secs
        };
        // Step 3. Every bootstrapped connection is told, once, with the event
        // vocabulary it already reads.
        state.canonical.publish(Event::Lifecycle {
            kind: KIND_DAEMON_SHUTTING_DOWN,
            payload: json!({"grace_secs": grace, "pending": pending}),
        });
        info!(
            "[daemon] shutting down: {} operation(s) to settle within {grace}s",
            pending.len()
        );
        drive(Arc::clone(state), exit.clone());
    }

    // Step 4.
    let inner = lock(&shutdown.inner);
    json!({
        "stopping": true,
        "grace_secs": inner.grace_secs,
        "pending": inner.pending,
    })
}

/// Steps 5 to 7, in a task of their own so the answer to step 4 is not waiting
/// on them.
fn drive(state: Arc<DaemonState>, exit: watch::Sender<bool>) {
    tokio::spawn(async move {
        let grace = state.shutdown.grace_secs();
        // Step 5.
        let unsettled = wait_out(&state, Duration::from_secs(grace)).await;
        // Step 6.
        state.shutdown.watchers.send_replace(true);
        stop_runtimes(&state).await;
        // Step 7: publish the report, then let whoever asked write it.
        let report = Report {
            clean: unsettled.is_empty(),
            unsettled,
        };
        if report.clean {
            info!("[daemon] shutdown clean: everything settled");
        } else {
            warn!(
                "[daemon] shutdown cut {} operation(s) short",
                report.unsettled.len()
            );
        }
        {
            lock(&state.shutdown.inner).report = Some(report);
        }
        state.shutdown.settled.send_replace(true);
        await_reporters(&state.shutdown).await;
        await_connections(&state).await;
        // Step 8 is the caller's: `serve` returns, `run` unlinks its three
        // runtime files and the process exits.
        let _ = exit.send(true);
    });
}

/// Wait up to `grace` for the registry to empty, then cancel what is left and
/// answer it as it was at the deadline.
///
/// Returns before the deadline the moment nothing is live, which is what makes
/// the grace a ceiling. A `grace` of zero polls once and cancels immediately.
async fn wait_out(state: &Arc<DaemonState>, grace: Duration) -> Vec<Value> {
    let deadline = Instant::now() + grace;
    loop {
        if state.operations.live().is_empty() {
            return Vec::new();
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }

    // Captured before the cancel, so the report names the state that made the
    // stop unclean rather than the `cancelled` this line is about to write.
    let left = state.operations.live();
    let unsettled: Vec<Value> = left
        .iter()
        .map(super::operations::OperationStatus::to_json)
        .collect();
    for status in &left {
        let id = OperationId::new(status.id.as_str());
        if let Err(e) = state.operations.cancel(&id) {
            // It settled between the capture and the cancel, which is the race
            // the grace exists to win and is not a failure.
            info!("[daemon] {id} settled while the shutdown was cancelling it: {e}");
        }
    }
    unsettled
}

/// Stop every account runtime, off the reactor.
///
/// The drop is what closes SQLite and releases the engine lock, and it is a
/// blocking one, so it runs where `config.reload`'s stop already runs. The
/// lock would come free at exit anyway; doing it here is what makes "releases
/// locks" a step of the sequence rather than a side effect of the process
/// ending.
async fn stop_runtimes(state: &Arc<DaemonState>) {
    let runtimes = state.runtimes.take_all();
    if runtimes.is_empty() {
        return;
    }
    info!("[daemon] stopping {} account runtime(s)", runtimes.len());
    if let Err(e) = tokio::task::spawn_blocking(move || drop(runtimes)).await {
        warn!("[daemon] the runtime stop task {e}");
    }
}

/// Wait, bounded, for every connection that asked to stop to write its report.
async fn await_reporters(shutdown: &Shutdown) {
    let deadline = Instant::now() + REPORT_TAKE_CEILING;
    while shutdown.reporters.load(Ordering::SeqCst) > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            warn!("[daemon] a client that asked to stop never took its report");
            return;
        }
        // Racing the notification is harmless: the loop re-reads the counter.
        let _ = tokio::time::timeout(left.min(POLL), shutdown.reported.notified()).await;
    }
}

/// Wait, bounded, for every connection to write out what it was queued and
/// close.
///
/// The rest of step 7. Letting the process exit under them instead would close
/// the sockets just the same, but a client whose `daemon.shutting_down` was
/// still in its outbound queue would learn about an orderly stop as an EOF
/// mid-stream, which is exactly what a crash looks like. The subscription is
/// released when a connection task ends, so the subscriber count is what says
/// they are gone.
async fn await_connections(state: &Arc<DaemonState>) {
    let deadline = Instant::now() + CONNECTIONS_CLOSE_CEILING;
    while state.canonical.subscriber_count() > 0 {
        if Instant::now() >= deadline {
            warn!(
                "[daemon] {} connection(s) had not closed after {CONNECTIONS_CLOSE_CEILING:?}; \
                 exiting \
                 under them",
                state.canonical.subscriber_count()
            );
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Everything the registry still has live, as `operation.status` objects in
/// start order.
fn live_operations(state: &DaemonState) -> Vec<Value> {
    state
        .operations
        .live()
        .iter()
        .map(super::operations::OperationStatus::to_json)
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh daemon is not stopping and owes nobody a report.
    #[test]
    fn a_running_daemon_is_not_shutting_down() {
        let shutdown = Shutdown::new();
        assert!(!shutdown.is_stopping());
        assert_eq!(shutdown.grace_secs(), 0);
        assert!(!*shutdown.watchers_stopped().borrow());
    }

    /// The report frame is the notification the contract fixes, on the third
    /// notification method rather than as a `state.event`.
    #[tokio::test]
    async fn the_report_frame_is_a_daemon_stopped_notification() {
        let shutdown = Shutdown::new();
        lock(&shutdown.inner).report = Some(Report {
            clean: false,
            unsettled: vec![json!({"operation_id": "op-1", "method": "sync.full"})],
        });
        shutdown.settled.send_replace(true);

        let frame = shutdown
            .report_frame("abcd")
            .await
            .expect("a settled shutdown has a report");
        let value: Value = serde_json::from_slice(&frame[..frame.len() - 1]).expect("a JSON frame");
        assert_eq!(value["method"], json!(METHOD_DAEMON_STOPPED));
        assert_eq!(value["params"]["instance_id"], json!("abcd"));
        assert_eq!(value["params"]["clean"], json!(false));
        assert_eq!(
            value["params"]["unsettled"][0]["method"],
            json!("sync.full")
        );
        assert!(
            value.get("id").is_none(),
            "a notification carries no id: {value}"
        );
    }

    /// The driver does not wait for a reporter nobody registered, and stops
    /// waiting the moment the one that did says it is done.
    #[tokio::test]
    async fn the_driver_waits_only_for_the_connections_that_asked() {
        let shutdown = Arc::new(Shutdown::new());
        await_reporters(&shutdown).await;

        shutdown.expect_report();
        let owed = shutdown.report_owed();
        let waiter = Arc::clone(&shutdown);
        let handle = tokio::spawn(async move { await_reporters(&waiter).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!handle.is_finished(), "it is still waiting for the report");
        drop(owed);
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("the reporter released the driver")
            .expect("the waiter did not panic");
    }
}
