//! A gate a test holds hooked futures at (PERSO-222).
//!
//! The daemon tests keep holding a tick body, a drain or a server leg open
//! until the test has looked at something, and kept rebuilding that by hand
//! on a `tokio::sync::Notify`. A `Notify` stores at most one permit, and
//! `notify_one` delivered to a registered waiter stores none, so a test that
//! releases one body and then wants a second to pass has to know which of the
//! two cases it is in; three bodies on one `Notify` is the shape that got it
//! wrong. A [`Gate`] counts: [`Gate::open`] adds permits whether or not
//! anything is waiting yet, each [`Gate::pass`] takes one, and
//! [`Gate::arrived`] says when the held futures got there, without a sleep.
//!
//! A test target includes it by path (`#[path = "support/gate.rs"] mod
//! gate;`), and a library unit test by `#[path =
//! "../../tests/support/gate.rs"]`, as `fake_imap` is.

#![allow(dead_code)]

use tokio::sync::{watch, Semaphore};

/// Permits a test hands out, and a count of the futures that reached it.
#[derive(Debug)]
pub struct Gate {
    permits: Semaphore,
    arrived: watch::Sender<usize>,
}

impl Default for Gate {
    fn default() -> Self {
        Gate::new()
    }
}

impl Gate {
    /// A closed gate: nothing passes until [`Gate::open`].
    pub fn new() -> Gate {
        Gate {
            permits: Semaphore::new(0),
            arrived: watch::Sender::new(0),
        }
    }

    /// Arrive, then wait for a permit and use it up.
    pub async fn pass(&self) {
        self.arrived.send_modify(|count| *count += 1);
        self.permits
            .acquire()
            .await
            .expect("a gate is never closed")
            .forget();
    }

    /// Let `permits` more passes through, now or whenever they arrive.
    pub fn open(&self, permits: usize) {
        self.permits.add_permits(permits);
    }

    /// Wait until `count` passes have arrived, released or not.
    pub async fn arrived(&self, count: usize) {
        self.arrived
            .subscribe()
            .wait_for(|arrived| *arrived >= count)
            .await
            .expect("the gate owns its sender");
    }

    /// How many passes have arrived so far.
    pub fn arrivals(&self) -> usize {
        *self.arrived.borrow()
    }
}
