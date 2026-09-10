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
//! # What it does not do yet
//!
//! Consume events. The connection is a subscriber from its first
//! `state.bootstrap` on, and the daemon queues `state.event` notifications for
//! it, but nothing here reads them: the TUI's watcher threads still drive
//! refreshes and P5-U7/U8 replace them. [`mp_client::Connection`] buffers a
//! notification it meets while reading a reply, so the notifications that
//! arrive between two calls are held rather than mistaken for answers. That
//! buffer is unbounded, which is safe only because this build starts no account
//! runtime and therefore produces no events; draining it is P5-U8's, and
//! `BACKLOG.md` carries it.
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

use mp_protocol::state::Bootstrap;

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
        let (calls, mut inbox) = async_mpsc::unbounded_channel::<Call>();
        let (ready, connected) = sync_mpsc::sync_channel::<()>(1);

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
                    // migrated command already runs under.
                    let mut connection = crate::daemon::client::client_session().await;
                    info!("[tui] daemon session open");
                    // A closed receiver means `connect` gave up waiting; the
                    // loop below still runs, and the first dropped sender ends
                    // it.
                    let _ = ready.try_send(());
                    while let Some(call) = inbox.recv().await {
                        let answer = connection
                            .call(&call.method, call.params)
                            .await
                            .map_err(|e| format!("{e}"));
                        if let Err(ref e) = answer {
                            warn!("[tui] {} failed: {e}", call.method);
                        }
                        (call.then)(answer);
                    }
                    info!("[tui] daemon session closed");
                });
            })?;

        match connected.recv_timeout(CONNECT_CEILING) {
            Ok(()) => Ok(Session {
                calls: Some(calls),
                thread: Some(thread),
            }),
            Err(e) => Err(anyhow!(
                "the daemon session did not come up within {}s: {e}",
                CONNECT_CEILING.as_secs()
            )),
        }
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
    pub fn handle(&self) -> QueryHandle {
        QueryHandle {
            calls: self.calls.clone(),
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
    /// `None` for a handle taken from a session that was already closed.
    calls: Option<async_mpsc::UnboundedSender<Call>>,
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
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        match self.calls.as_ref() {
            Some(calls) => call_on(calls, method, params),
            None => Err(anyhow!("{method}: the daemon session is closed")),
        }
    }
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
