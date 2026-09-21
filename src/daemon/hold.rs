//! The undo-send hold, owned by the daemon (`SND-04`, P6-U2, ticket #0125).
//!
//! A held send is an operation with a deadline. `send.draft` and
//! `send.approved` answer their `{operation_id}` at once, the scheduler arms a
//! timer, and the work the operation would have started immediately runs when
//! the window elapses. Until then the hold is a row in [`HoldScheduler`], a
//! per-second `send.hold_tick` on every bootstrapped connection, and an
//! `operation_id` any client may call `send.cancel_hold` with.
//!
//! # Why the daemon reads `email.send_hold_secs` itself
//!
//! `hold` is a boolean on the wire. A client that read the configuration and
//! passed a number of seconds would leave the policy where #0090 put it, under
//! a longer name, and two clients could then disagree about one window. The
//! daemon already owns the configuration and already serves it through
//! `config.get`, so it resolves the window and the clients render what it
//! sends. `send_hold_secs = 0` is the opt-out, resolved here: no hold is armed,
//! no event is published and the send starts at once.
//!
//! # The two races, and why neither needs a token
//!
//! A cancel that arrives while the timer is waking, and a second cancel behind
//! the first, are both settled by one rule: the map is the authority, and a
//! hold is *taken out of it* exactly once. [`HoldScheduler::fire`] and
//! [`HoldScheduler::cancel`] both remove, so whichever gets the mutex first
//! decides and the loser finds nothing and does nothing. The timer task
//! therefore needs no cancellation token: it wakes, finds its hold gone, and
//! returns.
//!
//! # The last client
//!
//! The plan's rule is that *"when the last client exits mid-hold the daemon
//! cancels the hold and leaves the draft approved"*, which is what killing the
//! TUI does today. [`HoldScheduler::cancel_all`] is that rule, called from the
//! connection loop once the subscriber count reaches zero. It is deliberately
//! not a cancel scope: `send.*` is [`CancelScope::Durable`](super::dispatch::CancelScope)
//! and a hold whose *own* client closed its window while another client watches
//! must still fire, which is what makes a confirmed send survive a `q`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use mp_protocol::events::{
    KIND_SEND_HOLD_CANCELLED, KIND_SEND_HOLD_FIRED, KIND_SEND_HOLD_STARTED, KIND_SEND_HOLD_TICK,
};
use mp_protocol::send::{HoldListing, HoldStatus};
use serde_json::{json, Value};

use super::operations::{OperationId, OperationRegistry};
use super::state::events::Event;
use super::state::CanonicalState;

/// What a send has to say about itself for a client to render its countdown.
///
/// Everything in it is known before the operation starts, which is what lets
/// the arm publish a complete [`HoldStatus`] in the same breath as the answer
/// that carried the operation id.
#[derive(Clone, Debug)]
pub struct HoldPlan {
    /// The window, in seconds, as the daemon resolved it. Never zero: a zero
    /// window arms nothing.
    pub hold_secs: u64,
    /// The draft that is waiting; the first of a batch.
    pub draft_id: String,
    /// Its subject, empty when it has none.
    pub subject: String,
}

/// One armed hold.
#[derive(Debug)]
struct Armed {
    /// Everything but the remainder, which is derived from [`Armed::deadline`].
    status: HoldStatus,
    /// When it fires, on this process's monotonic clock.
    deadline: Instant,
}

/// Every hold this daemon is carrying, by operation id.
///
/// One table per daemon process, held by [`DaemonState`](super::server::DaemonState)
/// because two things outside the dispatcher reach it: the connection loop,
/// which applies the last-client rule, and the timer tasks.
///
/// # Lock order
///
/// Two mutexes, and the order is **`armed` before `order`, never the
/// reverse**. [`HoldScheduler::arm`], [`HoldScheduler::listing`] and
/// [`HoldScheduler::take`] all hold `armed` while they reach for `order`, so a
/// method that held `order` while it reached for `armed` would deadlock the
/// daemon at the first interleaving - and both sides of that interleaving are
/// reachable from a live socket, `cancel_all` from the last client leaving or
/// from step 2 of a shutdown and `listing` from `send.hold_status`,
/// `diagnostic.health` and `state.bootstrap` on another connection.
/// A method that needs only `order` takes it alone and drops the guard before
/// it touches `armed`, which is what [`HoldScheduler::cancel_all`] does with
/// its snapshot; `cancel_all_and_listing_do_not_deadlock` is the row that
/// fails when that stops being true.
#[derive(Debug, Default)]
pub struct HoldScheduler {
    /// Arm order, which is the order a listing and a mass cancel walk in.
    order: Mutex<Vec<OperationId>>,
    armed: Mutex<HashMap<OperationId, Armed>>,
}

impl HoldScheduler {
    /// An empty scheduler.
    pub fn new() -> HoldScheduler {
        HoldScheduler::default()
    }

    /// Arm one hold and publish its `send.hold_started`.
    ///
    /// The status is answered as well as published, because the caller spawns
    /// the timer with it and a second read of the map would race the first
    /// tick.
    pub fn arm(
        &self,
        canonical: &CanonicalState,
        id: &OperationId,
        account: &str,
        origin: &str,
        plan: &HoldPlan,
    ) -> HoldStatus {
        let deadline = Instant::now() + Duration::from_secs(plan.hold_secs);
        let status = HoldStatus {
            operation_id: id.as_str().to_string(),
            account: account.to_string(),
            draft_id: plan.draft_id.clone(),
            subject: plan.subject.clone(),
            hold_secs: plan.hold_secs,
            remaining_secs: plan.hold_secs,
            fires_at: fires_at(plan.hold_secs),
            origin: origin.to_string(),
        };
        {
            let mut armed = lock(&self.armed);
            armed.insert(
                id.clone(),
                Armed {
                    status: status.clone(),
                    deadline,
                },
            );
            lock(&self.order).push(id.clone());
        }
        publish(canonical, KIND_SEND_HOLD_STARTED, &status);
        status
    }

    /// The hold `id` names, with the remainder it has left, or `None` when
    /// nothing is holding under that id.
    pub fn status(&self, id: &OperationId) -> Option<HoldStatus> {
        let now = Instant::now();
        lock(&self.armed).get(id).map(|hold| hold.at(now))
    }

    /// Every hold, in arm order, optionally narrowed to one account: the
    /// `result` of `send.hold_status`.
    pub fn listing(&self, account: Option<&str>) -> HoldListing {
        let now = Instant::now();
        let armed = lock(&self.armed);
        HoldListing {
            holds: lock(&self.order)
                .iter()
                .filter_map(|id| armed.get(id))
                .filter(|hold| account.is_none_or(|name| hold.status.account == name))
                .map(|hold| hold.at(now))
                .collect(),
        }
    }

    /// Take the hold `id` names because its window elapsed, and publish its
    /// `send.hold_fired`.
    ///
    /// `None` when it is no longer there, which is a cancel that won the race
    /// and means the caller must not send.
    pub fn fire(&self, canonical: &CanonicalState, id: &OperationId) -> Option<HoldStatus> {
        let status = self.take(id).map(ended)?;
        publish(canonical, KIND_SEND_HOLD_FIRED, &status);
        Some(status)
    }

    /// Cancel one hold: publish its `send.hold_cancelled` and settle the
    /// operation it was holding, which is what leaves the draft approved.
    ///
    /// `false` when no live hold answers to `id`, which is what
    /// `send.cancel_hold` refuses with `-32602`: an id that never named a hold
    /// and one whose hold has already fired are the same mistake, because a
    /// cancel is not a recall.
    pub fn cancel(
        &self,
        canonical: &CanonicalState,
        operations: &OperationRegistry,
        id: &OperationId,
    ) -> bool {
        let Some(status) = self.take(id).map(ended) else {
            return false;
        };
        publish(canonical, KIND_SEND_HOLD_CANCELLED, &status);
        // The operation the send answered with is settled as cancelled, so
        // `operation.status` agrees with the event and no client is left
        // waiting for work that will never run. Its own worker never started:
        // the timer task finds the hold gone and returns.
        let _ = operations.cancel(id);
        true
    }

    /// Cancel every hold, which is the last client leaving.
    ///
    /// Answers how many went, for the log line: a daemon that cancelled a send
    /// the user confirmed has to say so somewhere.
    pub fn cancel_all(&self, canonical: &CanonicalState, operations: &OperationRegistry) -> usize {
        // The snapshot is taken under `order` alone and that guard is dropped
        // before `cancel` reaches for `armed`, because the lock order of this
        // type is `armed` before `order` and a walk of the queue that held it
        // while testing the map would be the one inversion. An id that left
        // the map between the snapshot and the cancel is what `cancel`'s
        // `false` already means, so nothing is lost by not filtering here.
        let doomed: Vec<OperationId> = lock(&self.order).clone();
        doomed
            .iter()
            .filter(|id| self.cancel(canonical, operations, id))
            .count()
    }

    /// Remove one hold from the table, whoever is asking.
    fn take(&self, id: &OperationId) -> Option<Armed> {
        let taken = lock(&self.armed).remove(id);
        if taken.is_some() {
            lock(&self.order).retain(|queued| queued != id);
        }
        taken
    }
}

impl Armed {
    /// This hold as a client reads it now.
    fn at(&self, now: Instant) -> HoldStatus {
        HoldStatus {
            remaining_secs: remaining(self.deadline, now),
            ..self.status.clone()
        }
    }
}

/// A hold that is over, however it ended: nothing is left of its window.
fn ended(hold: Armed) -> HoldStatus {
    HoldStatus {
        remaining_secs: 0,
        ..hold.status
    }
}

/// Wait out one hold, then run `work`.
///
/// The countdown is driven off the deadline rather than off a repeating sleep:
/// a task that slept a second at a time would drift on a loaded machine and the
/// last tick would disagree with the fire. Every wake re-reads the map, so a
/// cancelled hold stops the countdown at the next second at the latest and
/// stops the send always.
pub async fn run_held<F: std::future::Future<Output = ()>>(
    scheduler: Arc<HoldScheduler>,
    canonical: Arc<CanonicalState>,
    id: OperationId,
    hold_secs: u64,
    work: F,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(hold_secs);
    loop {
        let left = remaining(deadline.into_std(), Instant::now());
        if left <= 1 {
            tokio::time::sleep_until(deadline).await;
            break;
        }
        // The instant the remainder becomes `left - 1`, which is when the next
        // tick is due.
        tokio::time::sleep_until(deadline - Duration::from_secs(left - 1)).await;
        match scheduler.status(&id) {
            Some(status) => publish(&canonical, KIND_SEND_HOLD_TICK, &status),
            None => return,
        }
    }
    if scheduler.fire(&canonical, &id).is_none() {
        return;
    }
    work.await;
}

/// How many whole seconds of a window are left, rounded up, so a countdown
/// shows `1` for the last fraction of a second rather than `0`.
fn remaining(deadline: Instant, now: Instant) -> u64 {
    let left = deadline.saturating_duration_since(now);
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// The RFC3339 instant a window of `secs` ends at, in UTC.
///
/// From the wall clock rather than from the monotonic deadline, because it is
/// for a client to render and a monotonic instant means nothing outside this
/// process.
fn fires_at(secs: u64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Publish one hold event to every bootstrapped connection.
///
/// A lifecycle event: a hold is a fact about a moment rather than a resource a
/// snapshot carries, so two ticks never coalesce into one and an overflowing
/// queue keeps them.
fn publish(canonical: &CanonicalState, kind: &'static str, status: &HoldStatus) {
    canonical.publish(Event::Lifecycle {
        kind,
        payload: payload(status),
    });
}

/// One [`HoldStatus`] as the payload all four kinds carry.
fn payload(status: &HoldStatus) -> Value {
    serde_json::to_value(status).unwrap_or_else(|e| {
        log::error!("[hold] a HoldStatus did not serialise: {e}");
        json!({})
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::InstanceId;

    fn state() -> Arc<CanonicalState> {
        Arc::new(CanonicalState::new(
            InstanceId::new("hold-test"),
            Vec::new(),
        ))
    }

    fn plan() -> HoldPlan {
        HoldPlan {
            hold_secs: 20,
            draft_id: "freigabe".to_string(),
            subject: "Angebot".to_string(),
        }
    }

    /// An armed hold is listed for its account, carries the window it was
    /// armed with, and answers a remainder no larger than that window.
    #[test]
    fn an_armed_hold_is_listed_with_what_is_left_of_it() {
        let scheduler = HoldScheduler::new();
        let canonical = state();
        let id = OperationId::new("op-1");
        let status = scheduler.arm(&canonical, &id, "alice", "tui", &plan());
        assert_eq!(status.remaining_secs, 20);
        assert_eq!(status.origin, "tui");

        let listed = scheduler.listing(Some("alice")).holds;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].remaining_secs <= 20);
        assert!(
            scheduler.listing(Some("bob")).holds.is_empty(),
            "another account's holds are not this account's"
        );
    }

    /// One hold ends once, whichever of the two ways reaches it first: the
    /// map is the authority and a taken hold is gone for everyone.
    #[test]
    fn a_hold_can_only_end_once() {
        let scheduler = HoldScheduler::new();
        let canonical = state();
        let operations = OperationRegistry::new();
        let id = OperationId::new("op-2");
        scheduler.arm(&canonical, &id, "alice", "tui", &plan());

        assert!(scheduler.cancel(&canonical, &operations, &id));
        assert!(
            !scheduler.cancel(&canonical, &operations, &id),
            "a second cancel names nothing"
        );
        assert!(
            scheduler.fire(&canonical, &id).is_none(),
            "and the timer that wakes afterwards must not send"
        );
        assert!(scheduler.listing(None).holds.is_empty());
    }

    /// The last client leaving cancels every hold there is.
    #[test]
    fn the_last_client_leaving_cancels_all_of_them() {
        let scheduler = HoldScheduler::new();
        let canonical = state();
        let operations = OperationRegistry::new();
        for name in ["op-3", "op-4"] {
            scheduler.arm(&canonical, &OperationId::new(name), "alice", "tui", &plan());
        }
        assert_eq!(scheduler.cancel_all(&canonical, &operations), 2);
        assert!(scheduler.listing(None).holds.is_empty());
        assert_eq!(
            scheduler.cancel_all(&canonical, &operations),
            0,
            "and a daemon with nothing holding cancels nothing"
        );
    }

    /// `cancel_all` and `listing` run against each other without deadlocking.
    ///
    /// The regression this guards is a lock-order inversion: `cancel_all`
    /// once held `order` for the whole of its snapshot statement and took
    /// `armed` inside the filter, where `arm`, `listing` and `take` all take
    /// `armed` first. Both sides are reachable from a live socket at the same
    /// instant - `cancel_all` from the last client leaving or from step 2 of
    /// a shutdown, `listing` from `send.hold_status`, `diagnostic.health` or
    /// `state.bootstrap` on another connection - so the interleaving is a
    /// daemon that stops answering, not a theoretical one.
    ///
    /// A deadlock shows up as the timeout on the channel rather than as a
    /// hung test binary, which is why the threads report through one.
    #[test]
    fn cancel_all_and_listing_do_not_deadlock() {
        /// Enough interleavings that the inverted version hangs reliably, few
        /// enough that the row costs milliseconds.
        const ROUNDS: usize = 200;
        /// Far longer than the work needs; only a deadlock reaches it.
        const PATIENCE: Duration = Duration::from_secs(30);

        let scheduler = Arc::new(HoldScheduler::new());
        let canonical = state();
        let operations = Arc::new(OperationRegistry::new());
        let (done, finished) = std::sync::mpsc::channel();

        let cancelling = {
            let scheduler = Arc::clone(&scheduler);
            let canonical = Arc::clone(&canonical);
            let operations = Arc::clone(&operations);
            let done = done.clone();
            std::thread::spawn(move || {
                for round in 0..ROUNDS {
                    for name in ["a", "b", "c"] {
                        scheduler.arm(
                            &canonical,
                            &OperationId::new(format!("op-{name}-{round}")),
                            "alice",
                            "tui",
                            &plan(),
                        );
                    }
                    scheduler.cancel_all(&canonical, &operations);
                }
                let _ = done.send("cancel_all");
            })
        };
        let listing = {
            let scheduler = Arc::clone(&scheduler);
            std::thread::spawn(move || {
                for _ in 0..ROUNDS {
                    let _ = scheduler.listing(None);
                    let _ = scheduler.listing(Some("alice"));
                }
                let _ = done.send("listing");
            })
        };

        for _ in 0..2 {
            finished.recv_timeout(PATIENCE).unwrap_or_else(|e| {
                panic!("a hold thread did not finish within {PATIENCE:?}, which is the lock-order inversion: {e}")
            });
        }
        cancelling.join().expect("the cancelling thread");
        listing.join().expect("the listing thread");
        assert!(
            scheduler.listing(None).holds.is_empty(),
            "and every hold the cancelling thread armed is gone"
        );
    }

    /// The remainder is rounded up, so a window with a fraction of a second
    /// left reads as one second rather than as none.
    #[test]
    fn the_remainder_is_rounded_up() {
        let now = Instant::now();
        assert_eq!(remaining(now + Duration::from_millis(1), now), 1);
        assert_eq!(remaining(now + Duration::from_millis(1500), now), 2);
        assert_eq!(remaining(now, now), 0);
        assert_eq!(remaining(now - Duration::from_secs(5), now), 0);
    }
}
