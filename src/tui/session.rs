//! The TUI's daemon session (P5-U2, #0124): one connection, one thread, and a
//! blocking door onto it.
//!
//! The TUI's main loop is synchronous and `mp`'s `main` is already inside a
//! `#[tokio::main]` runtime, so neither can drive an async call: blocking on
//! the current runtime from inside it panics, and turning `run_loop` async
//! would put a frame-painting loop on a worker thread. A [`Session`] therefore
//! owns a thread of its own with a current-thread runtime on it, and the UI
//! thread talks to it over channels. That thread does the connecting too:
//! [`mp_client::Connection`] holds a `tokio::net::UnixStream` registered with
//! the runtime that created it, so a connection opened in `main` and moved here
//! would be bound to a driver that is not running.
//!
//! # What routes through it
//!
//! [`crate::daemon::client::client_session`], the same door every migrated CLI
//! command goes through, so the TUI inherits its behaviour whole: the socket
//! path, the on-demand start with its one budget,
//! `MAILYPOPPINS_DAEMON_AUTOSTART`, the exit-4 diagnostic, and the
//! `MAILYPOPPINS_DAEMON_REQUIRE` flag that a run really did reach a daemon. A
//! second connect routine here would be a second set of those decisions.
//!
//! # The events, and the daemon going away
//!
//! The connection is a subscriber from its first `state.bootstrap` on, and the
//! thread reads that stream between calls (P5-U8): every notification is
//! decoded into an [`Incoming`] and posted to the UI thread, which drains it
//! before the next paint. [`mp_client::Connection`] buffers a notification it
//! meets while reading a reply, so nothing is mistaken for an answer and
//! nothing is dropped; what used to make that buffer unbounded was that nobody
//! read it, and somebody does now.
//!
//! A daemon that goes away is read off the same stream: the notification
//! reader answering `None` is the socket closing, which is posted as
//! [`Incoming::Disconnected`] and followed by reconnect attempts on a widening
//! gap. A call made while there is no daemon is refused at once rather than
//! waiting out the 30 s ceiling, and **nothing falls back to the store**: a
//! client that answered a dead daemon by opening the store itself would be the
//! second engine the whole architecture exists to prevent.
//!
//! The reconnect goes through [`crate::daemon::client::reopen_session`], which
//! is `client_session` with the exit-4 diagnostic replaced by a `None`: the
//! auto-start policy and the `MAILYPOPPINS_DAEMON_REQUIRE` bookkeeping are the
//! same ones, because a reconnect is a connect, but a TUI on the alternate
//! screen may not be ended by a diagnostic printed into a terminal in raw mode.
//!
//! # Lifetime
//!
//! The `App` owns the `Session`, so quitting drops it, which drops the call
//! channel, which ends the loop on the session thread, which drops the
//! connection and closes the socket. [`Session::close`] is the same teardown
//! done early and waited for, which is what `run_loop` calls on its way out so
//! the daemon sees the client leave before the process does.

use std::sync::mpsc as sync_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{info, warn};
use serde_json::Value;
use tokio::sync::mpsc as async_mpsc;

use mp_client::Connection;
use mp_protocol::state::Bootstrap;
use mp_protocol::{EventEnvelope, METHOD_STATE_EVENT, METHOD_STATE_RESYNC_REQUIRED};

use super::events::{Incoming, Subscription};

/// How long [`Session::connect`] waits for the connect-and-handshake sequence
/// before giving up on the session thread.
///
/// A ceiling over `client_session`'s own budget (5 s of auto-start by default,
/// plus one 10 s handshake timeout) rather than a competing deadline: reaching
/// it means the session thread is wedged, not that a daemon was slow, and the
/// TUI starts without a session instead of never starting at all.
const CONNECT_CEILING: Duration = Duration::from_secs(30);

/// How long a blocking [`Session::call`] waits for its answer.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// First gap between reconnect attempts after the daemon went away.
///
/// Short, because the common case is a daemon that was restarted deliberately
/// and is back within a second; the gap widens to [`RECONNECT_MAX`] so a
/// machine with no daemon coming back does not spend the session connecting.
const RECONNECT_MIN: Duration = Duration::from_millis(250);

/// The longest gap between two reconnect attempts.
const RECONNECT_MAX: Duration = Duration::from_secs(2);

/// One method call handed to the session thread, with what to do with the
/// answer.
///
/// The continuation is a closure rather than a reply channel so one call site
/// can block on the answer and another can post it to the UI's background
/// channel, without a second variant for each.
struct Call {
    method: String,
    params: Value,
    then: Box<dyn FnOnce(Result<Value, String>) + Send>,
}

/// A live daemon session: the thread, and the door onto it.
pub struct Session {
    /// `None` once [`Session::close`] has run, which is what makes closing
    /// twice (explicitly, then in `Drop`) harmless.
    calls: Option<async_mpsc::UnboundedSender<Call>>,
    /// The UI thread's end of the event stream, handed out once by
    /// [`Session::events`] and `None` afterwards: one drain, one reader.
    events: Option<Subscription>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("open", &self.calls.is_some())
            .finish()
    }
}

impl Session {
    /// Connect to the daemon, starting one on demand, and hand back the
    /// session.
    ///
    /// Blocks until the handshake is done, because everything after it is
    /// cheaper with a connection than without one, and because a failure has to
    /// reach the user before the alternate screen is entered: `client_session`
    /// prints the exit-4 diagnostic and ends the process, and a terminal in raw
    /// mode would swallow it.
    ///
    /// `Err` is a wedged session thread and nothing else; every ordinary
    /// failure to reach a daemon has already exited by then.
    pub fn connect() -> Result<Session> {
        let (calls, inbox) = async_mpsc::unbounded_channel::<Call>();
        let (ready, connected) = sync_mpsc::sync_channel::<()>(1);
        let (events, subscription) = sync_mpsc::channel::<Incoming>();

        let thread = std::thread::Builder::new()
            .name("mp-tui-session".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        warn!("[tui] the daemon session could not start a runtime: {e}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    // Exits the process with the exit-4 diagnostic when no
                    // daemon can be reached, which is the contract every
                    // migrated command already runs under. Only the *first*
                    // connect does: a reconnect answers instead of ending a run
                    // that is already on the alternate screen.
                    let connection = crate::daemon::client::client_session().await;
                    info!("[tui] daemon session open");
                    // A closed receiver means `connect` gave up waiting; the
                    // loop below still runs, and the first dropped sender ends
                    // it.
                    let _ = ready.try_send(());
                    serve(connection, inbox, events).await;
                    info!("[tui] daemon session closed");
                });
            })?;

        match connected.recv_timeout(CONNECT_CEILING) {
            Ok(()) => Ok(Session {
                calls: Some(calls),
                events: Some(subscription),
                thread: Some(thread),
            }),
            Err(e) => Err(anyhow!(
                "the daemon session did not come up within {}s: {e}",
                CONNECT_CEILING.as_secs()
            )),
        }
    }

    /// A session served by something in this process rather than by a daemon
    /// over a socket.
    ///
    /// The shape is the real one exactly: a thread of its own owns the
    /// answering end, and the UI thread reaches it over the same call channel,
    /// so a test drives the whole door (`Session::handle`, `QueryHandle::call`,
    /// the 30 s ceiling) and not a shortcut around it.
    ///
    /// `build` runs **on the session thread** and hands back what answers
    /// there, because the fixture has thread-local state to install first (the
    /// data-root override of #0077) and because whatever it builds must not
    /// have to be `Send` after that point.
    #[cfg(test)]
    pub fn serving<Q, B>(build: B) -> Session
    where
        Q: crate::tui::queries::Queries,
        B: FnOnce() -> Q + Send + 'static,
    {
        let (calls, mut inbox) = async_mpsc::unbounded_channel::<Call>();
        let thread = std::thread::Builder::new()
            .name("mp-tui-session-test".to_string())
            .spawn(move || {
                let queries = build();
                while let Some(call) = inbox.blocking_recv() {
                    let answer = queries
                        .call(&call.method, call.params)
                        .map_err(|e| format!("{e:#}"));
                    (call.then)(answer);
                }
            })
            .expect("a test session thread");
        Session {
            calls: Some(calls),
            // Nothing publishes to a fixture, so a test that asked for the
            // stream would drain an empty one for ever; the rows that exercise
            // the drain feed it a `Sender` of their own.
            events: None,
            thread: Some(thread),
        }
    }

    /// The event stream, once.
    ///
    /// `None` on the second call and for a session that serves no daemon: the
    /// drain is the loop's and there is exactly one of it.
    pub fn events(&mut self) -> Option<Subscription> {
        self.events.take()
    }

    /// Post a call and hand its answer to `then`, on the session thread.
    ///
    /// Never blocks: this is what a paint-driven loop uses so that waiting for
    /// the daemon never costs a frame. `then` runs off the UI thread, so it
    /// posts to a channel rather than touching the `App`.
    pub fn dispatch<F>(&self, method: &str, params: Value, then: F)
    where
        F: FnOnce(Result<Value, String>) + Send + 'static,
    {
        let call = Call {
            method: method.to_string(),
            params,
            then: Box::new(then),
        };
        let Some(sender) = self.calls.as_ref() else {
            (call.then)(Err("the daemon session is closed".to_string()));
            return;
        };
        if let Err(e) = sender.send(call) {
            (e.0.then)(Err("the daemon session is closed".to_string()));
        }
    }

    /// Call one method and wait for its answer.
    ///
    /// The door the query layer (P5-U4) uses for a read the frame cannot be
    /// painted without. It blocks the UI thread, so a call that can be awaited
    /// off-frame belongs in [`Session::dispatch`] or on a [`QueryHandle`] of a
    /// worker thread instead.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        match self.calls.as_ref() {
            Some(calls) => call_on(calls, method, params),
            None => Err(anyhow!("{method}: the daemon session is closed")),
        }
    }

    /// A blocking door onto this session that a worker thread can own.
    ///
    /// The `App` owns the `Session` and the UI thread owns the `App`, so a
    /// background load cannot borrow one. A handle is the call channel and
    /// nothing else: it starts no second connection, it keeps no session alive
    /// (a call on a handle whose session has closed fails like any other), and
    /// it is what keeps the mailbox walk of #0003 off the draw thread now that
    /// the walk is a daemon call.
    ///
    /// **A weak sender**, and that is load-bearing (P5-U6): [`Session::close`]
    /// drops the strong one and then *joins* the session thread, whose loop
    /// ends when the last sender goes. A worker holding a strong clone would
    /// therefore keep the thread alive and make quitting block on it, which a
    /// sync arm polling `operation.status` to a terminal state can do for as
    /// long as the mailbox takes. Weak, the channel closes on quit, the
    /// worker's next call fails with the closed-session error every other
    /// refusal uses, and it ends.
    pub fn handle(&self) -> QueryHandle {
        QueryHandle {
            calls: self
                .calls
                .as_ref()
                .map(async_mpsc::UnboundedSender::downgrade),
        }
    }

    /// Ask for the whole state and hand the decoded snapshot to `then`.
    ///
    /// Off the first-paint path deliberately (#0003, and the plan's "the shell
    /// still paints before any store opens"): the TUI posts this after its
    /// first frame is drawn and applies the answer whenever it lands.
    pub fn dispatch_bootstrap<F>(&self, then: F)
    where
        F: FnOnce(Result<Bootstrap, String>) + Send + 'static,
    {
        self.dispatch("state.bootstrap", serde_json::json!({}), move |result| {
            then(result.and_then(|value| {
                serde_json::from_value::<Bootstrap>(value)
                    .map_err(|e| format!("the bootstrap did not decode: {e}"))
            }))
        });
    }

    /// Close the session and wait for the thread to finish.
    ///
    /// Idempotent: the second call has no sender to drop and no thread to
    /// join, which is what lets `run_loop` close explicitly and `Drop` close
    /// again.
    pub fn close(&mut self) {
        self.calls = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// A cloneable, sendable door onto a [`Session`], for a thread that is not the
/// UI thread. See [`Session::handle`].
#[derive(Clone, Debug)]
pub struct QueryHandle {
    /// `None` for a handle taken from a session that was already closed;
    /// weak, so holding one cannot keep the session thread alive past a quit.
    calls: Option<async_mpsc::WeakUnboundedSender<Call>>,
}

impl QueryHandle {
    /// A handle onto no session at all, which answers every call with the same
    /// error a handle taken from a closed session answers with.
    ///
    /// What a client with no `Session` holds: `Session::connect` only fails
    /// when the session thread wedged (every ordinary failure to reach a daemon
    /// has already exited the process), and the run's own
    /// `MAILYPOPPINS_DAEMON_REQUIRE` check fails on the way out. Nothing falls
    /// back to the store behind it.
    pub fn closed() -> QueryHandle {
        QueryHandle { calls: None }
    }

    /// Call one method and wait for its answer, on whatever thread holds this.
    ///
    /// A session that has closed since this handle was taken is the same
    /// refusal as no session at all, which is what lets a worker's poll loop
    /// end on a quit rather than outlive the process's terminal.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        match self.calls.as_ref().and_then(|calls| calls.upgrade()) {
            Some(calls) => call_on(&calls, method, params),
            None => Err(anyhow!("{method}: the daemon session is closed")),
        }
    }
}

// ---------------------------------------------------------------------------
// The session thread
// ---------------------------------------------------------------------------

/// Answer calls and publish events until the last door is dropped (P5-U8).
///
/// One `select!` over the two things that can happen: the UI thread asks
/// something, or the daemon says something. They share the connection, so they
/// cannot both hold it, and a loop that read notifications only between calls
/// would sit on an event until the next keystroke.
///
/// Both futures are cancellation-safe, which is what makes the `select!` sound:
/// `recv` on a tokio channel is, and `next_notification` awaits nothing but one
/// `read` and decodes what that read returned before it awaits again.
async fn serve(
    mut connection: Connection,
    mut inbox: async_mpsc::UnboundedReceiver<Call>,
    events: sync_mpsc::Sender<Incoming>,
) {
    loop {
        // Connected: serve calls, publish notifications.
        let lost = loop {
            tokio::select! {
                call = inbox.recv() => {
                    let Some(call) = call else { return };
                    let answer = connection
                        .call(&call.method, call.params)
                        .await
                        .map_err(|e| format!("{e}"));
                    if let Err(ref e) = answer {
                        warn!("[tui] {} failed: {e}", call.method);
                    }
                    (call.then)(answer);
                }
                notification = connection.next_notification() => match notification {
                    Some(notification) => publish(&events, notification),
                    None => break "the daemon closed the connection".to_string(),
                },
            }
        };

        // Disconnected: say so, refuse what is asked, and try again.
        warn!("[tui] the daemon session was lost: {lost}");
        if events
            .send(Incoming::Disconnected { reason: lost })
            .is_err()
        {
            return;
        }
        let mut gap = RECONNECT_MIN;
        // A deadline and not a fresh `sleep` per iteration: the UI thread polls
        // this session while it waits, and a timer recreated on every refused
        // call would be reset before it ever fired.
        let mut next_attempt = tokio::time::Instant::now() + gap;
        connection = loop {
            tokio::select! {
                call = inbox.recv() => {
                    let Some(call) = call else { return };
                    // At once rather than after the 30 s ceiling, and from
                    // nowhere else: a client that answered a dead daemon out of
                    // the store would be a second engine.
                    (call.then)(Err("the daemon is not reachable".to_string()));
                }
                _ = tokio::time::sleep_until(next_attempt) => {
                    if let Some((connection, instance_id)) =
                        crate::daemon::client::reopen_session().await
                    {
                        info!("[tui] reconnected to daemon instance {instance_id}");
                        if events.send(Incoming::Reconnected { instance_id }).is_err() {
                            return;
                        }
                        break connection;
                    }
                    gap = (gap * 2).min(RECONNECT_MAX);
                    next_attempt = tokio::time::Instant::now() + gap;
                }
            }
        };
    }
}

/// One server-initiated notification as the UI thread's [`Incoming`].
///
/// A notification of a method this client does not read is dropped here rather
/// than posted: the drain's bound is a budget for the paint, and spending it on
/// frames nothing reacts to would make a chatty daemon cost the screen.
fn publish(events: &sync_mpsc::Sender<Incoming>, notification: mp_protocol::Notification) {
    let incoming = match notification.method.as_str() {
        METHOD_STATE_EVENT => match serde_json::from_value::<EventEnvelope>(notification.params) {
            Ok(envelope) => Incoming::Event(envelope),
            Err(e) => return warn!("[tui] a state.event did not decode: {e}"),
        },
        METHOD_STATE_RESYNC_REQUIRED => Incoming::Resync {
            instance_id: notification.params["instance_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            reason: notification.params["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        },
        other => return info!("[tui] ignoring the {other} notification"),
    };
    let _ = events.send(incoming);
}

/// Post one call and block for its answer, which is what both doors do.
fn call_on(
    calls: &async_mpsc::UnboundedSender<Call>,
    method: &str,
    params: Value,
) -> Result<Value> {
    let (answer, wait) = sync_mpsc::sync_channel::<Result<Value, String>>(1);
    let call = Call {
        method: method.to_string(),
        params,
        then: Box::new(move |result| {
            let _ = answer.send(result);
        }),
    };
    if calls.send(call).is_err() {
        return Err(anyhow!("{method}: the daemon session is closed"));
    }
    match wait.recv_timeout(CALL_TIMEOUT) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
        Err(e) => Err(anyhow!("{method}: no answer from the daemon ({e})")),
    }
}
