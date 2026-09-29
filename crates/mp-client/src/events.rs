//! What the session thread tells its client (P5-U8, #0124), lifted from
//! `clients/tui` with the session that produces it.
//!
//! The decoded `state.event` travels on the same stream as the connection's
//! own state (`state.resync_required`, the daemon going away, a daemon
//! answering again), so the four stay ordered against each other: a second
//! channel would let a [`Incoming::Reconnected`] overtake the last event of the
//! dead instance. Applying them to a model is each client's own business;
//! `mp_tui::events` re-exports both names under their old path.

use std::sync::mpsc::Receiver;

use mp_protocol::EventEnvelope;

/// One thing the session thread has to tell the UI thread.
#[derive(Debug)]
pub enum Incoming {
    /// One decoded `state.event`.
    Event(EventEnvelope),
    /// `state.resync_required`: this connection's queue was poisoned and
    /// accepts no further domain event until it bootstraps again.
    Resync {
        /// The instance that gave up on the queue.
        instance_id: String,
        /// Why, in the daemon's own word (`event_queue_overflow`).
        reason: String,
    },
    /// The daemon went away.
    Disconnected {
        /// What the transport said, for the log and the status line.
        reason: String,
    },
    /// A daemon answers again, and it may not be the same one.
    Reconnected {
        /// The instance the handshake reported.
        instance_id: String,
    },
}

/// The UI thread's end of the session's event stream.
///
/// A plain `Receiver`, which is what `run_loop` already holds three of and what
/// a `try_recv` drain reads without a second vocabulary. It is also what lets a
/// test feed the drain without a socket: whether the far end is a session
/// thread or a `Sender` in a test is not a property the drain may observe.
pub type Subscription = Receiver<Incoming>;
