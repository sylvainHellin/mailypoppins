//! A client's daemon session (P5-U2, #0124): one connection, one thread, and a
//! blocking door onto it.
//!
//! Written for the TUI and lifted here from `clients/tui` unchanged, so every
//! client (the TUI, the desktop GUI) shares one session kernel; `mp_tui`
//! re-exports it as `mp_tui::session`. What follows still reads from the TUI's
//! side, because the TUI is the client whose constraints shaped it.
//!
//! The TUI's main loop is synchronous and `mp`'s `main` is already inside a
//! `#[tokio::main]` runtime, so neither can drive an async call: blocking on
//! the current runtime from inside it panics, and turning `run_loop` async
//! would put a frame-painting loop on a worker thread. A [`Session`] therefore
//! owns a thread of its own with a current-thread runtime on it, and the UI
//! thread talks to it over channels. That thread does the connecting too:
//! [`Connection`] holds a `tokio::net::UnixStream` registered with
//! the runtime that created it, so a connection opened in `main` and moved here
//! would be bound to a driver that is not running.
//!
//! # What routes through it
//!
//! `mailypoppins::daemon::client::client_session`, the same door every migrated
//! CLI command goes through, so the TUI inherits its behaviour whole: the
//! socket path, the on-demand start with its one budget,
//! `MAILYPOPPINS_DAEMON_AUTOSTART`, the exit-4 diagnostic, and the
//! `MAILYPOPPINS_DAEMON_REQUIRE` flag that a run really did reach a daemon. A
//! second connect routine here would be a second set of those decisions.
//!
//! It is **injected** rather than called (#0126, P5-U10f). All three of the
//! things that door needs are the binary's: the data directory the socket path
//! is derived from, the routine that spawns `mp daemon run`, and the
//! process-global flag the parity gate reads. This crate links neither the
//! daemon nor the lifecycle that owns them, so a [`Connector`] of two function
//! pointers is what crosses the boundary; the binary builds it, `mp_tui::run`
//! passes it straight through, and nothing else about the connect changed: the
//! same routine runs, on the same thread, with the same budget.
//!
//! # The events, and the daemon going away
//!
//! The connection is a subscriber from its first `state.bootstrap` on, and the
//! thread reads that stream between calls (P5-U8): every notification is
//! decoded into an [`Incoming`] and posted to the UI thread, which drains it
//! before the next paint. [`Connection`] buffers a notification it
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
//! The reconnect goes through the connector's second half,
//! `mailypoppins::daemon::client::reopen_session`, which is `client_session`
//! with the exit-4 diagnostic replaced by a `None`: the
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

use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc as sync_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use serde_json::{json, Value};
use tokio::sync::mpsc as async_mpsc;

use crate::types::ClientError;
use crate::Connection;
use mp_protocol::listing::METHOD_MESSAGE_LIST_STREAM;
use mp_protocol::listing::{MessageListRow, MessageListStreamStarted, MessageListing};
use mp_protocol::state::Bootstrap;
use mp_protocol::{
    EventEnvelope, RpcError, METHOD_MESSAGE_ROWS, METHOD_STATE_EVENT, METHOD_STATE_RESYNC_REQUIRED,
};

use crate::events::{Incoming, Subscription};

/// How long [`Session::connect`] waits for the connect-and-handshake sequence
/// before giving up on the session thread.
///
/// A ceiling over the connector's own budget (5 s of auto-start by default,
/// plus one 10 s handshake timeout) rather than a competing deadline: reaching
/// it means the session thread is wedged, not that a daemon was slow, and the
/// TUI starts without a session instead of never starting at all.
const CONNECT_CEILING: Duration = Duration::from_secs(30);

/// How long a blocking [`Session::call`] waits for its answer; a caller with
/// another budget uses [`Session::call_within`].
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// First gap between reconnect attempts after the daemon went away.
///
/// Short, because the common case is a daemon that was restarted deliberately
/// and is back within a second; the gap widens to [`RECONNECT_MAX`] so a
/// machine with no daemon coming back does not spend the session connecting.
const RECONNECT_MIN: Duration = Duration::from_millis(250);

/// The longest gap between two reconnect attempts.
const RECONNECT_MAX: Duration = Duration::from_secs(2);

/// How much longer than its budget the caller of a streamed listing waits on
/// its channel, so the answer it sees is the session thread's (a listing, or
/// the timeout that thread answered after cancelling the stream) and not its
/// own `recv_timeout`.
const STREAM_ANSWER_GRACE: Duration = Duration::from_secs(2);

/// How long the session thread keeps reading for a stream's
/// `operation.finished` once it has given up on the stream.
///
/// The finish normally follows the `operation.cancel` within a frame or two;
/// this bounds the one case where it never comes, a connection that never
/// bootstrapped and therefore receives no operation event at all.
const STREAM_SETTLE_GRACE: Duration = Duration::from_secs(5);

/// One piece of work handed to the session thread, with what to do with the
/// answer.
///
/// The continuation is a closure rather than a reply channel so one call site
/// can block on the answer and another can post it to the UI's background
/// channel, without a second variant for each.
enum Call {
    /// One method, answered with its `result`.
    Plain {
        method: String,
        params: Value,
        then: Box<dyn FnOnce(Result<Value, Failure>) + Send>,
    },
    /// One `message.list_stream`, collected into a listing (#0138).
    ///
    /// The deadline travels with the call because the session thread is the
    /// only place that can act on it: at the deadline it cancels the stream,
    /// answers the caller with a timeout, and keeps discarding the stream's
    /// rows until its finish arrives.
    Stream {
        account: String,
        mailbox: String,
        deadline: std::time::Instant,
        budget: Duration,
        then: ListingReply,
    },
}

/// What a streamed listing's caller is answered through.
type ListingReply = Box<dyn FnOnce(Result<MessageListing, Failure>) + Send>;

impl Call {
    /// Answer the call with `failure`, whichever kind it is.
    fn fail(self, failure: Failure) {
        match self {
            Call::Plain { then, .. } => then(Err(failure)),
            Call::Stream { then, .. } => then(Err(failure)),
        }
    }
}

/// Why a call on the session thread failed: the daemon's refusal, kept typed
/// so its `data` reaches the caller, or anything else, as its sentence.
enum Failure {
    Refused(RpcError),
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused(error) => ClientError::Rpc(error.clone()).fmt(f),
            Failure::Other(text) => f.write_str(text),
        }
    }
}

/// A daemon refusal a blocking call answered, `data` included (#0131).
///
/// The `anyhow::Error` [`Session::call_within`] and [`QueryHandle::call_within`]
/// answer wraps one of these when the daemon refused the call, and its text is
/// the text those calls always answered, `<method>: the daemon refused the
/// call: <message> (<code>)`; [`refusal`] reads it back. A client whose
/// refusal carries a payload, such as `draft_invalid`'s `draft.invalid`
/// diagnostics, decodes the payload instead of rebuilding it.
#[derive(Debug, thiserror::Error)]
#[error("{method}: {}", ClientError::Rpc(.error.clone()))]
pub struct Refused {
    /// The method that was refused.
    pub method: String,
    /// The daemon's error, code, message and `data` as they arrived.
    pub error: RpcError,
}

/// The daemon's refusal behind an error a blocking call answered, or `None`
/// for a failure of any other kind (a timeout, a closed session).
pub fn refusal(error: &anyhow::Error) -> Option<&RpcError> {
    error
        .downcast_ref::<Refused>()
        .map(|refused| &refused.error)
}

/// How the binary opens the first connection, and how it opens a replacement.
///
/// Two function pointers rather than a trait: both are plain `async fn`s of
/// `mailypoppins::daemon::client` and neither carries state, so a pointer is
/// the whole of what has to cross the crate boundary, and `Copy + Send` is what
/// lets the session thread keep one for the reconnect loop.
///
/// The halves differ in exactly one way, and it is the reason there are two.
/// `open` may end the process with the exit-4 diagnostic, which is right for a
/// first connect that has not entered the alternate screen yet; `reopen`
/// answers `None` instead, because a TUI in raw mode may not be ended by a
/// sentence printed into it.
#[derive(Clone, Copy)]
pub struct Connector {
    /// Connect, starting a daemon on demand, or end the process.
    pub open: OpenSession,
    /// Connect again, or answer `None` and let the caller back off.
    pub reopen: ReopenSession,
}

/// The first connect: a connection, or a process that has already exited.
pub type OpenSession = fn() -> Pin<Box<dyn Future<Output = Connection> + Send>>;

/// A reconnect: a connection and the instance id behind it, or nothing yet.
pub type ReopenSession = fn() -> Pin<Box<dyn Future<Output = Option<(Connection, String)>> + Send>>;

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
    pub fn connect(connector: Connector) -> Result<Session> {
        Session::start(connector.open, connector.reopen)
    }

    /// [`Session::connect`] with the first connect as any closure, which is
    /// what lets a test point a session at a socket of its own without a
    /// static: [`Connector`]'s halves are plain function pointers.
    fn start<O>(open: O, reopen: ReopenSession) -> Result<Session>
    where
        O: FnOnce() -> Pin<Box<dyn Future<Output = Connection> + Send>> + Send + 'static,
    {
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
                    let connection = open().await;
                    info!("[tui] daemon session open");
                    // A closed receiver means `connect` gave up waiting; the
                    // loop below still runs, and the first dropped sender ends
                    // it.
                    let _ = ready.try_send(());
                    serve(connection, inbox, events, reopen).await;
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn serving<Q, B>(build: B) -> Session
    where
        Q: crate::queries::Queries,
        B: FnOnce() -> Q + Send + 'static,
    {
        let (calls, mut inbox) = async_mpsc::unbounded_channel::<Call>();
        let thread = std::thread::Builder::new()
            .name("mp-tui-session-test".to_string())
            .spawn(move || {
                let queries = build();
                let failure = |e: anyhow::Error| match e.downcast::<Refused>() {
                    Ok(refused) => Failure::Refused(refused.error),
                    Err(e) => Failure::Other(format!("{e:#}")),
                };
                while let Some(call) = inbox.blocking_recv() {
                    match call {
                        Call::Plain {
                            method,
                            params,
                            then,
                        } => then(queries.call(&method, params).map_err(failure)),
                        Call::Stream {
                            account,
                            mailbox,
                            then,
                            ..
                        } => then(queries.list_stream(&account, &mailbox).map_err(failure)),
                    }
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
        let call = Call::Plain {
            method: method.to_string(),
            params,
            then: Box::new(move |answer| then(answer.map_err(|e| e.to_string()))),
        };
        let closed = || Failure::Other("the daemon session is closed".to_string());
        let Some(sender) = self.calls.as_ref() else {
            call.fail(closed());
            return;
        };
        if let Err(e) = sender.send(call) {
            e.0.fail(closed());
        }
    }

    /// Call one method and wait for its answer.
    ///
    /// The door the query layer (P5-U4) uses for a read the frame cannot be
    /// painted without. It blocks the UI thread, so a call that can be awaited
    /// off-frame belongs in [`Session::dispatch`] or on a [`QueryHandle`] of a
    /// worker thread instead.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_within(method, params, DEFAULT_CALL_TIMEOUT)
    }

    /// [`Session::call`] under a budget of the caller's choosing.
    ///
    /// The budget bounds how long this thread waits, not the call: a call that
    /// outlives it still runs to its end on the session thread, and its answer
    /// is dropped.
    pub fn call_within(&self, method: &str, params: Value, budget: Duration) -> Result<Value> {
        match self.calls.as_ref() {
            Some(calls) => call_on(calls, method, params, budget),
            None => Err(anyhow!("{method}: the daemon session is closed")),
        }
    }

    /// One whole mailbox through `message.list_stream`, collected into the
    /// listing a `message.list` answer decodes into (#0138), within
    /// [`DEFAULT_CALL_TIMEOUT`].
    pub fn list_stream(&self, account: &str, mailbox: &str) -> Result<MessageListing> {
        self.list_stream_within(account, mailbox, DEFAULT_CALL_TIMEOUT)
    }

    /// [`Session::list_stream`] under a budget of the caller's choosing.
    ///
    /// Unlike [`Session::call_within`]'s, the budget bounds the stream itself:
    /// the session thread cancels a stream still running at the deadline,
    /// answers this call with a timeout, and serves the next call once the
    /// stream's finish has arrived.
    pub fn list_stream_within(
        &self,
        account: &str,
        mailbox: &str,
        budget: Duration,
    ) -> Result<MessageListing> {
        match self.calls.as_ref() {
            Some(calls) => stream_on(calls, account, mailbox, budget),
            None => Err(anyhow!(
                "{METHOD_MESSAGE_LIST_STREAM}: the daemon session is closed"
            )),
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
    /// therefore keep the thread alive and make quitting block on it, which the
    /// two worker threads that are left can do for as long as their call takes:
    /// the background mailbox load and the startup per-account count. Since
    /// P5-U8 an operation is awaited on the event stream rather than polled, so
    /// no worker outlives a call any more. Weak, the channel closes on quit, the
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
        self.call_within(method, params, DEFAULT_CALL_TIMEOUT)
    }

    /// [`QueryHandle::call`] under a budget of the caller's choosing, with
    /// [`Session::call_within`]'s meaning.
    pub fn call_within(&self, method: &str, params: Value, budget: Duration) -> Result<Value> {
        match self.calls.as_ref().and_then(|calls| calls.upgrade()) {
            Some(calls) => call_on(&calls, method, params, budget),
            None => Err(anyhow!("{method}: the daemon session is closed")),
        }
    }

    /// [`Session::list_stream`], on whatever thread holds this.
    pub fn list_stream(&self, account: &str, mailbox: &str) -> Result<MessageListing> {
        self.list_stream_within(account, mailbox, DEFAULT_CALL_TIMEOUT)
    }

    /// [`Session::list_stream_within`], on whatever thread holds this: the
    /// budget is the stream's deadline on the session thread.
    pub fn list_stream_within(
        &self,
        account: &str,
        mailbox: &str,
        budget: Duration,
    ) -> Result<MessageListing> {
        match self.calls.as_ref().and_then(|calls| calls.upgrade()) {
            Some(calls) => stream_on(&calls, account, mailbox, budget),
            None => Err(anyhow!(
                "{METHOD_MESSAGE_LIST_STREAM}: the daemon session is closed"
            )),
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
    reopen: ReopenSession,
) {
    loop {
        // Connected: serve calls, publish notifications.
        let lost = loop {
            tokio::select! {
                call = inbox.recv() => match call {
                    None => return,
                    Some(Call::Plain { method, params, then }) => {
                        let answer = connection
                            .call(&method, params)
                            .await
                            .map_err(|e| match e {
                                ClientError::Rpc(error) => Failure::Refused(error),
                                other => Failure::Other(format!("{other}")),
                            });
                        if let Err(ref e) = answer {
                            warn!("[tui] {method} failed: {e}");
                        }
                        then(answer);
                    }
                    Some(Call::Stream { account, mailbox, deadline, budget, then }) => {
                        let stream = StreamCall {
                            account,
                            mailbox,
                            deadline: tokio::time::Instant::from_std(deadline),
                            budget,
                            answer: Some(then),
                        };
                        if let Some(lost) = stream.serve(&mut connection, &events).await {
                            break lost;
                        }
                    }
                },
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
                    call.fail(Failure::Other("the daemon is not reachable".to_string()));
                }
                _ = tokio::time::sleep_until(next_attempt) => {
                    if let Some((connection, instance_id)) = reopen().await {
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
        // A chunk of a stream the session thread is no longer collecting: one
        // it gave up on and whose finish never came. Dropped quietly, since a
        // stream is dozens of them.
        METHOD_MESSAGE_ROWS => return debug!("[tui] dropping a message.rows chunk nobody awaits"),
        other => return info!("[tui] ignoring the {other} notification"),
    };
    let _ = events.send(incoming);
}

/// Post one call and block for its answer, which is what both doors do.
fn call_on(
    calls: &async_mpsc::UnboundedSender<Call>,
    method: &str,
    params: Value,
    budget: Duration,
) -> Result<Value> {
    let (answer, wait) = sync_mpsc::sync_channel::<Result<Value, Failure>>(1);
    let call = Call::Plain {
        method: method.to_string(),
        params,
        then: Box::new(move |result| {
            let _ = answer.send(result);
        }),
    };
    if calls.send(call).is_err() {
        return Err(anyhow!("{method}: the daemon session is closed"));
    }
    match wait.recv_timeout(budget) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(Failure::Refused(error))) => Err(anyhow::Error::new(Refused {
            method: method.to_string(),
            error,
        })),
        Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
        Err(e) => Err(anyhow!("{method}: no answer from the daemon ({e})")),
    }
}

/// Post one streamed listing and block for its answer, which is what both
/// doors do.
fn stream_on(
    calls: &async_mpsc::UnboundedSender<Call>,
    account: &str,
    mailbox: &str,
    budget: Duration,
) -> Result<MessageListing> {
    let method = METHOD_MESSAGE_LIST_STREAM;
    let (answer, wait) = sync_mpsc::sync_channel::<Result<MessageListing, Failure>>(1);
    let call = Call::Stream {
        account: account.to_string(),
        mailbox: mailbox.to_string(),
        deadline: std::time::Instant::now() + budget,
        budget,
        then: Box::new(move |result| {
            let _ = answer.send(result);
        }),
    };
    if calls.send(call).is_err() {
        return Err(anyhow!("{method}: the daemon session is closed"));
    }
    match wait.recv_timeout(budget + STREAM_ANSWER_GRACE) {
        Ok(Ok(listing)) => Ok(listing),
        Ok(Err(Failure::Refused(error))) => Err(anyhow::Error::new(Refused {
            method: method.to_string(),
            error,
        })),
        Ok(Err(e)) => Err(anyhow!("{method}: {e}")),
        Err(e) => Err(anyhow!("{method}: no answer from the daemon ({e})")),
    }
}

/// One streamed listing on the session thread (#0138): the call, the rows it
/// collects, and the caller still waiting for an answer.
struct StreamCall {
    account: String,
    mailbox: String,
    deadline: tokio::time::Instant,
    budget: Duration,
    /// `None` once the caller has been answered, after which the stream's
    /// rows are discarded until its finish arrives.
    answer: Option<ListingReply>,
}

impl StreamCall {
    /// Answer the caller, once; a second answer is dropped.
    fn reply(&mut self, result: Result<MessageListing, Failure>) {
        if let Some(then) = self.answer.take() {
            if let Err(ref e) = result {
                warn!("[tui] {METHOD_MESSAGE_LIST_STREAM} failed: {e}");
            }
            then(result);
        }
    }

    /// Run the stream to its finish, publishing every notification that is
    /// not one of its rows, and answer `Some(reason)` when the connection was
    /// lost on the way.
    ///
    /// The session thread serves no other call meanwhile, exactly as it serves
    /// none while one large `message.list` answer is in flight. Every other
    /// notification is published as it arrives, the stream's own finish
    /// included, so each client's watermark sees its revision; a
    /// `state.resync_required` is published at once, and the `state.bootstrap`
    /// it provokes waits behind the stream.
    async fn serve(
        mut self,
        connection: &mut Connection,
        events: &sync_mpsc::Sender<Incoming>,
    ) -> Option<String> {
        if tokio::time::Instant::now() >= self.deadline {
            let timeout = self.timeout();
            self.reply(Err(timeout));
            return None;
        }
        let params = json!({"account": self.account, "mailbox": self.mailbox});
        let started = match connection.call(METHOD_MESSAGE_LIST_STREAM, params).await {
            Ok(started) => started,
            Err(ClientError::Rpc(error)) => {
                self.reply(Err(Failure::Refused(error)));
                return None;
            }
            // A connection that broke here is noticed by the next read of the
            // serve loop, which is where a lost connection is handled.
            Err(other) => {
                self.reply(Err(Failure::Other(format!("{other}"))));
                return None;
            }
        };
        let started: MessageListStreamStarted = match serde_json::from_value(started) {
            Ok(started) => started,
            Err(e) => {
                self.reply(Err(Failure::Other(format!(
                    "the {METHOD_MESSAGE_LIST_STREAM} answer did not decode: {e}"
                ))));
                return None;
            }
        };
        let id = started.operation_id.clone();
        let mut rows = RowsCollector::new(started);
        // Set once the caller has been answered with a failure: how long to
        // keep reading for the finish before serving the next call anyway.
        let mut settle_by: Option<tokio::time::Instant> = None;

        loop {
            let wake = settle_by.unwrap_or(self.deadline);
            tokio::select! {
                notification = connection.next_notification() => {
                    let Some(notification) = notification else {
                        self.reply(Err(Failure::Other(
                            "the daemon closed the connection before the stream finished"
                                .to_string(),
                        )));
                        return Some("the daemon closed the connection".to_string());
                    };
                    if notification.method == METHOD_MESSAGE_ROWS {
                        if self.answer.is_some() {
                            if let Err(failure) = rows.push(&notification.params) {
                                self.abandon(connection, &id, failure, &mut settle_by).await;
                            }
                        }
                        continue;
                    }
                    let finish = finish_payload(&notification, &id);
                    publish(events, notification);
                    if let Some(payload) = finish {
                        let result = rows.finish(&payload);
                        self.reply(result);
                        return None;
                    }
                }
                _ = tokio::time::sleep_until(wake) => {
                    if settle_by.is_some() {
                        warn!(
                            "[tui] the {METHOD_MESSAGE_LIST_STREAM} operation {id} sent no \
                             finish after its cancel; serving the next call"
                        );
                        return None;
                    }
                    let timeout = self.timeout();
                    self.abandon(connection, &id, timeout, &mut settle_by).await;
                }
            }
        }
    }

    /// The failure a stream that outlived its budget answers.
    ///
    /// "went unanswered" is the phrase [`ClientError::Timeout`] uses, which a
    /// client that classifies a failure by its text (the desktop's
    /// `GuiError::from_call_text`) reads as a timeout.
    fn timeout(&self) -> Failure {
        Failure::Other(format!(
            "the listing of {}/{} went unanswered within {}s; the stream was cancelled",
            self.account,
            self.mailbox,
            self.budget.as_secs_f64()
        ))
    }

    /// Give up on the stream: answer the caller with `failure`, cancel the
    /// operation so the daemon stops sending rows nobody will use, and give
    /// its finish [`STREAM_SETTLE_GRACE`] to arrive.
    async fn abandon(
        &mut self,
        connection: &mut Connection,
        id: &str,
        failure: Failure,
        settle_by: &mut Option<tokio::time::Instant>,
    ) {
        self.reply(Err(failure));
        // A refusal here is an operation that settled on its own in the
        // meantime, whose finish is on its way regardless.
        if let Err(e) = connection
            .call("operation.cancel", json!({"operation_id": id}))
            .await
        {
            debug!("[tui] cancelling the stream {id}: {e}");
        }
        *settle_by = Some(tokio::time::Instant::now() + STREAM_SETTLE_GRACE);
    }
}

/// The `operation.finished` payload of `id`, when `notification` is one.
fn finish_payload(notification: &mp_protocol::Notification, id: &str) -> Option<Value> {
    let params = &notification.params;
    (notification.method == METHOD_STATE_EVENT
        && params["kind"].as_str() == Some(mp_protocol::events::KIND_OPERATION_FINISHED)
        && params["payload"]["operation_id"].as_str() == Some(id))
    .then(|| params["payload"].clone())
}

/// The rows of one `message.list_stream`, checked as they arrive (#0138).
///
/// The stream is contiguous by contract: each chunk starts where the last one
/// ended and none runs past the answer's `total`, and a success carries exactly
/// `total` rows. A stream that breaks any of the three is a protocol error and
/// no listing, so the caller keeps the list it held.
struct RowsCollector {
    started: MessageListStreamStarted,
    rows: Vec<MessageListRow>,
}

impl RowsCollector {
    fn new(started: MessageListStreamStarted) -> Self {
        let capacity = usize::try_from(started.total).unwrap_or(0).min(1 << 20);
        RowsCollector {
            started,
            rows: Vec::with_capacity(capacity),
        }
    }

    /// Fold one `message.rows` chunk in, answering `false` for a chunk of
    /// another stream, which is dropped.
    ///
    /// Each row is decoded on its own, as a `message.list` row is
    /// ([`crate::queries::row_from_wire`]), so one row a newer daemon shaped
    /// differently costs that row and not the listing.
    fn push(&mut self, params: &Value) -> Result<bool, Failure> {
        let id = &self.started.operation_id;
        if params["operation_id"].as_str() != Some(id.as_str()) {
            return Ok(false);
        }
        let (Some(offset), Some(rows)) = (params["offset"].as_u64(), params["rows"].as_array())
        else {
            return Err(Failure::Other(format!(
                "a message.rows chunk of operation {id} carries no offset or no rows"
            )));
        };
        let held = self.rows.len() as u64;
        if offset != held {
            return Err(Failure::Other(format!(
                "a message.rows chunk of operation {id} starts at offset {offset}, \
                 where the stream is at {held}"
            )));
        }
        if held + rows.len() as u64 > self.started.total {
            return Err(Failure::Other(format!(
                "a message.rows chunk of operation {id} runs past the announced total of {}",
                self.started.total
            )));
        }
        self.rows
            .extend(rows.iter().map(crate::queries::row_from_wire));
        Ok(true)
    }

    /// The listing a `succeeded` finish completes, or why there is none.
    ///
    /// A failed or cancelled finish answers the error it carries as a
    /// refusal, so its `data` (a `frame_too_large`'s `{limit, seen}`) reaches
    /// the caller through [`refusal`].
    fn finish(self, payload: &Value) -> Result<MessageListing, Failure> {
        let id = &self.started.operation_id;
        match payload["state"].as_str() {
            Some("succeeded") => {
                let held = self.rows.len() as u64;
                if held != self.started.total {
                    return Err(Failure::Other(format!(
                        "operation {id} succeeded with {held} rows where the stream announced {}",
                        self.started.total
                    )));
                }
                Ok(MessageListing {
                    account: self.started.account,
                    mailbox: self.started.mailbox,
                    total: self.started.total,
                    messages: self.rows,
                })
            }
            state => {
                let state = state.unwrap_or("without a state");
                match serde_json::from_value::<RpcError>(payload["error"].clone()) {
                    Ok(error) => Err(Failure::Refused(error)),
                    Err(_) => Err(Failure::Other(format!(
                        "operation {id} finished {state} and carried no error"
                    ))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    /// The socket the scripted daemon listens on, which a [`Connector`] of
    /// plain function pointers can only reach through a static.
    static SOCKET: OnceLock<PathBuf> = OnceLock::new();

    fn open() -> Pin<Box<dyn Future<Output = Connection> + Send>> {
        Box::pin(async {
            Connection::connect(SOCKET.get().expect("the socket is set"))
                .await
                .expect("the scripted daemon listens")
        })
    }

    /// No second connection: the test ends with the first.
    const REOPEN: ReopenSession = || Box::pin(async { None });

    /// A refusal keeps its `data` through the session thread, and its text is
    /// the one every caller already reads (#0131).
    #[test]
    fn a_refusal_keeps_its_data_and_its_text() {
        let dir = std::env::temp_dir().join(format!("mp-client-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let path = dir.join("d.sock");
        let _ = std::fs::remove_file(&path);
        SOCKET.set(path.clone()).expect("set once");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime.block_on(async { UnixListener::bind(&path).expect("bind") });
        let script = std::thread::spawn(move || {
            runtime.block_on(async move {
                let (stream, _) = listener.accept().await.expect("accept");
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                let answer = json!({"jsonrpc": "2.0", "id": request["id"], "error": {
                    "code": -32010, "message": "line 2: no",
                    "data": {"account": "a", "id": "x", "path": "/p/x.md", "diagnostics": []},
                }});
                let mut frame = serde_json::to_vec(&answer).unwrap();
                frame.push(b'\n');
                write.write_all(&frame).await.unwrap();
                // Hold the socket open until the session closes it.
                let _ = lines.next_line().await;
            })
        });

        let session = Session::connect(Connector {
            open,
            reopen: REOPEN,
        })
        .expect("a session");
        let error = session
            .call_within("draft.approve", json!({}), Duration::from_secs(5))
            .expect_err("refused");
        assert_eq!(
            format!("{error:#}"),
            "draft.approve: the daemon refused the call: line 2: no (-32010)"
        );
        let refused = refusal(&error).expect("a typed refusal");
        assert_eq!(refused.code, -32010);
        assert_eq!(
            refused.data.as_ref().map(|d| d["path"].clone()),
            Some(json!("/p/x.md"))
        );
        assert!(refusal(&anyhow!("x: the daemon session is closed")).is_none());
        drop(session);
        script.join().expect("the script ran");
    }

    // -----------------------------------------------------------------------
    // The streamed listing (#0138), over a canned daemon
    // -----------------------------------------------------------------------

    /// The operation id every canned stream runs under.
    const OP: &str = "8f2c41d6b0e94a7fa3c5d81e6b0947fc";

    /// A canned daemon on a socket of its own: it accepts one connection and
    /// answers each request with the frames its script returns, recording
    /// every request it saw.
    struct Canned {
        path: PathBuf,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Canned {
        /// A daemon that answers `message.list_stream` with the head of a
        /// `total`-row listing followed by `on_stream`, `operation.cancel` with
        /// its answer followed by `on_cancel`, and anything else with an echo.
        fn new(name: &str, total: u64, on_stream: Vec<Value>, on_cancel: Vec<Value>) -> Canned {
            let dir = std::env::temp_dir()
                .join(format!("mp-client-stream-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a scratch dir");
            let path = dir.join("d.sock");
            let _ = std::fs::remove_file(&path);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            let listener = runtime.block_on(async { UnixListener::bind(&path).expect("bind") });
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = std::sync::Arc::clone(&seen);
            let thread = std::thread::spawn(move || {
                runtime.block_on(async move {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let (read, mut write) = stream.into_split();
                    let mut lines = BufReader::new(read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let request: Value = serde_json::from_str(&line).expect("a request");
                        log.lock().expect("the log").push(request.clone());
                        let mut frames = Vec::new();
                        match request["method"].as_str() {
                            Some("message.list_stream") => {
                                frames.push(answer(
                                    &request,
                                    json!({"operation_id": OP, "account": "work",
                                           "mailbox": "inbox", "total": total}),
                                ));
                                frames.extend(on_stream.iter().cloned());
                            }
                            Some("operation.cancel") => {
                                frames.push(answer(
                                    &request,
                                    json!({"operation_id": OP, "state": "cancelled"}),
                                ));
                                frames.extend(on_cancel.iter().cloned());
                            }
                            other => frames.push(answer(&request, json!({"echo": other}))),
                        }
                        for frame in frames {
                            let mut bytes = serde_json::to_vec(&frame).expect("encodes");
                            bytes.push(b'\n');
                            if write.write_all(&bytes).await.is_err() {
                                return;
                            }
                        }
                    }
                })
            });
            Canned {
                path,
                seen,
                thread: Some(thread),
            }
        }

        /// A session whose first connect is to this daemon.
        fn session(&self) -> Session {
            let path = self.path.clone();
            Session::start(
                move || -> Pin<Box<dyn Future<Output = Connection> + Send>> {
                    Box::pin(async move {
                        Connection::connect(&path)
                            .await
                            .expect("the canned daemon listens")
                    })
                },
                REOPEN,
            )
            .expect("a session")
        }

        /// The methods the daemon was asked for, in order.
        fn methods(&self) -> Vec<String> {
            self.seen
                .lock()
                .expect("the log")
                .iter()
                .map(|request| request["method"].as_str().unwrap_or_default().to_string())
                .collect()
        }

        /// Close `session` and wait for the daemon's thread to see it go.
        fn finish(mut self, session: Session) {
            drop(session);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("the canned daemon ran");
            }
        }
    }

    fn answer(request: &Value, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
    }

    /// A `message.rows` chunk of [`OP`] carrying rows with these ids.
    fn rows(offset: u64, ids: &[i64]) -> Value {
        let rows: Vec<Value> = ids
            .iter()
            .map(|id| json!({"id": id, "uid": id, "subject": format!("row {id}")}))
            .collect();
        json!({"jsonrpc": "2.0", "method": "message.rows",
               "params": {"offset": offset, "operation_id": OP, "rows": rows}})
    }

    fn event(revision: u64, kind: &str, payload: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "state.event", "params": {
            "instance_id": "i", "revision": revision, "kind": kind, "payload": payload,
        }})
    }

    /// An event that is not about the stream, which must be published.
    fn unrelated(revision: u64) -> Value {
        event(
            revision,
            "state.invalidate",
            json!({"resource": "mailbox:work/archive", "scope": {"query": "counts"}}),
        )
    }

    fn finished(revision: u64, payload: Value) -> Value {
        event(revision, "operation.finished", payload)
    }

    fn succeeded(revision: u64, total: u64) -> Value {
        finished(
            revision,
            json!({"operation_id": OP, "state": "succeeded",
                   "result": {"account": "work", "mailbox": "inbox", "total": total}}),
        )
    }

    /// The kinds of every event published so far.
    fn published(events: &Subscription) -> Vec<String> {
        let mut kinds = Vec::new();
        while let Ok(incoming) = events.recv_timeout(Duration::from_millis(200)) {
            if let Incoming::Event(envelope) = incoming {
                kinds.push(envelope.kind);
            }
        }
        kinds
    }

    /// The chunks fold into one listing in order, the head is the answer's,
    /// every other notification is published as it arrives, the stream's own
    /// finish included, and the session serves the next call.
    #[test]
    fn a_stream_collects_into_one_listing_and_publishes_everything_else() {
        let daemon = Canned::new(
            "ok",
            3,
            vec![
                rows(0, &[3, 2]),
                unrelated(5),
                rows(2, &[1]),
                succeeded(6, 3),
            ],
            Vec::new(),
        );
        let mut session = daemon.session();
        let events = session.events().expect("the event stream");

        let listing = session
            .list_stream_within("work", "inbox", Duration::from_secs(10))
            .expect("a listing");
        assert_eq!(
            (listing.account.as_str(), listing.mailbox.as_str()),
            ("work", "inbox")
        );
        assert_eq!(listing.total, 3);
        assert_eq!(
            listing
                .messages
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![3, 2, 1]
        );
        assert_eq!(listing.messages[2].subject, "row 1");

        assert_eq!(
            session.call("account.list", json!({})).expect("served"),
            json!({"echo": "account.list"})
        );
        assert_eq!(
            published(&events),
            vec![
                "state.invalidate".to_string(),
                "operation.finished".to_string()
            ]
        );
        assert_eq!(daemon.methods(), ["message.list_stream", "account.list"]);
        let request = daemon.seen.lock().expect("the log")[0].clone();
        assert_eq!(
            request["params"],
            json!({"account": "work", "mailbox": "inbox"})
        );
        daemon.finish(session);
    }

    /// A gap in `offset`, rows past `total`, a success short of `total` and a
    /// failed finish each answer an error rather than a listing, and the
    /// session serves the next call once the stream's finish has arrived.
    #[test]
    fn a_broken_stream_answers_an_error_and_the_session_serves_on() {
        let too_large = finished(
            9,
            json!({"operation_id": OP, "state": "failed", "error": {
                "code": -32004, "message": "row 1 needs a 17000000-byte frame",
                "data": {"limit": 16777216, "seen": 17000000},
            }}),
        );
        let cases = [
            (
                "gap",
                3,
                vec![rows(0, &[3]), rows(2, &[1]), succeeded(9, 3)],
                "offset 2",
                true,
            ),
            (
                "past",
                1,
                vec![rows(0, &[3, 2]), succeeded(9, 1)],
                "past the announced total of 1",
                true,
            ),
            (
                "short",
                3,
                vec![rows(0, &[3, 2]), succeeded(9, 3)],
                "succeeded with 2 rows",
                false,
            ),
            (
                "failed",
                2,
                vec![rows(0, &[3]), too_large],
                "(-32004)",
                false,
            ),
        ];
        for (name, total, frames, says, cancels) in cases {
            let daemon = Canned::new(name, total, frames, Vec::new());
            let session = daemon.session();
            let error = session
                .list_stream_within("work", "inbox", Duration::from_secs(10))
                .expect_err(name);
            let text = format!("{error:#}");
            assert!(
                text.starts_with("message.list_stream: ") && text.contains(says),
                "{name}: {text}"
            );
            if name == "failed" {
                let refused = refusal(&error).expect("a failed finish is a typed refusal");
                assert_eq!(refused.code, -32004);
                assert_eq!(
                    refused.data,
                    Some(json!({"limit": 16777216, "seen": 17000000}))
                );
            }
            assert_eq!(
                session.call("account.list", json!({})).expect("served"),
                json!({"echo": "account.list"}),
                "{name}: the session serves the next call"
            );
            let methods = daemon.methods();
            assert_eq!(
                methods.contains(&"operation.cancel".to_string()),
                cancels,
                "{name}: a broken stream still running is cancelled: {methods:?}"
            );
            daemon.finish(session);
        }
    }

    /// A deadline that passes mid-stream sends `operation.cancel`, answers the
    /// caller with a timeout at the deadline rather than at its own
    /// `recv_timeout`, discards the late rows, publishes everything else, and
    /// leaves the session serving the next call once the finish arrives.
    #[test]
    fn a_stream_past_its_deadline_is_cancelled_and_the_session_serves_on() {
        let daemon = Canned::new(
            "deadline",
            5,
            vec![rows(0, &[5, 4])],
            vec![
                rows(2, &[3]),
                unrelated(7),
                finished(
                    8,
                    json!({"operation_id": OP, "state": "cancelled", "error": {
                        "code": -32008, "message": "cancelled", "data": {"operation_id": OP},
                    }}),
                ),
            ],
        );
        let mut session = daemon.session();
        let events = session.events().expect("the event stream");

        let asked = std::time::Instant::now();
        let error = session
            .list_stream_within("work", "inbox", Duration::from_millis(300))
            .expect_err("the stream never finishes on its own");
        let waited = asked.elapsed();
        let text = format!("{error:#}");
        assert!(text.contains("within 0.3s"), "{text}");
        assert!(
            text.contains("went unanswered"),
            "the phrase a client reads as a timeout: {text}"
        );
        assert!(
            waited < Duration::from_millis(300) + STREAM_ANSWER_GRACE,
            "answered by the session thread at the deadline, after {waited:?}"
        );

        assert_eq!(
            session.call("account.list", json!({})).expect("served"),
            json!({"echo": "account.list"})
        );
        assert_eq!(
            daemon.methods(),
            ["message.list_stream", "operation.cancel", "account.list"]
        );
        let cancel = daemon.seen.lock().expect("the log")[1].clone();
        assert_eq!(cancel["params"], json!({"operation_id": OP}));
        assert_eq!(
            published(&events),
            vec![
                "state.invalidate".to_string(),
                "operation.finished".to_string()
            ]
        );
        daemon.finish(session);
    }
}
