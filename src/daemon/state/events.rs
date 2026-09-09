//! The wire vocabulary of a state change, and one connection's outbound queue
//! (P3a-U6).
//!
//! [`Change`] is what the daemon commits; [`Event`] is what a client receives,
//! and [`Event::from_change`] is the one bridge between them. The difference is
//! not cosmetic: counts travel as an [`Event::Invalidate`] of the query whose
//! cached answer went stale, which is what lets a hundred of them coalesce into
//! one thing to re-read, while a small whole resource travels as an
//! [`Event::Replace`] a client can apply without asking anything back.
//!
//! [`Outbound`] is the only buffer between the fan-out and a socket. It is
//! bounded twice, by [`Subscriber::max_events`] and by [`Subscriber::max_bytes`],
//! and a connection that stops reading fills it and then loses its domain
//! events rather than the daemon's memory: the overflow discards what is
//! queued, poisons the queue and asks the client for a fresh
//! `state.bootstrap`. The unbounded [`EventQueue`](super::EventQueue) behind it
//! is drained eagerly by the connection task, so it is a hand-off and never a
//! backlog.
//!
//! # The push algorithm
//!
//! 1. **Poisoned and domain.** A domain event pushed onto a poisoned queue is
//!    dropped: everything the client holds is already wrong, and only
//!    [`Outbound::rebootstrap`] ends that.
//! 2. **Coalesce.** An event whose coalescing key matches a queued one replaces
//!    it, takes the new revision and moves to the tail, so `pop` order stays
//!    revision order. `Replace` is keyed by `(kind, resource)`, `Invalidate` by
//!    `(resource, scope)`, and `Remove` and `Lifecycle` have no key: a removal
//!    is a fact about a moment, and merging two would move the earlier one past
//!    whatever happened between them.
//! 3. **Fit.** An event that keeps both caps is appended.
//! 4. **Overflow.** Otherwise every queued domain event is discarded, the
//!    queued lifecycle events are kept in their order, the queue is poisoned
//!    and one `state.resync_required` marker is appended behind them. A
//!    lifecycle event that fits after the discard is still queued, because it
//!    is in no snapshot and a re-bootstrap would not bring it back.

use std::collections::VecDeque;

use serde_json::{json, Value};

use crate::daemon::dispatch::ResourceId;

use super::{Change, Revision};

/// The `reason` a queue overflow asks a client to resync with, and the string
/// `crates/mp-protocol/fixtures/notification.resync_required.json` carries.
pub const REASON_QUEUE_OVERFLOW: &str = "event_queue_overflow";

/// Test-only hook: commit this many `MailboxCounts` changes against the first
/// configured account after **every** `state.bootstrap`, with mailbox slugs
/// `burst-<i>` from a counter that never restarts for the life of the process.
///
/// Phase 3a commits no change of its own, so nothing would ever fill a queue
/// and the backpressure paths would be untestable. Distinct slugs are the
/// point: they are distinct resources, so no two of them coalesce and the queue
/// really fills. On the
/// [`FAKE_READY_ENV`](super::FAKE_READY_ENV) precedent: no flag exposes it and
/// `mp --help` never moves. Documented in `docs/daemon-operations.md`.
pub const FAKE_EVENT_BURST_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST";

/// The kind an [`Event::Invalidate`] travels as.
pub const KIND_INVALIDATE: &str = "state.invalidate";

/// The kind an [`Event::Remove`] travels as.
pub const KIND_REMOVE: &str = "state.remove";

/// How many changes [`FAKE_EVENT_BURST_ENV`] asks for, or `None` when it is
/// unset, is not a number, or is zero.
pub fn fake_event_burst() -> Option<u64> {
    let count = std::env::var(FAKE_EVENT_BURST_ENV)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    (count > 0).then_some(count)
}

// ---------------------------------------------------------------------------
// The wire vocabulary
// ---------------------------------------------------------------------------

/// One thing a client is told, in the form it travels.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A small whole resource, superseded by its newest payload.
    Replace {
        /// The event kind, which selects the client's handler.
        kind: &'static str,
        /// The whole of what the kind carries.
        payload: Value,
    },
    /// A cached query over a resource went stale and must be re-read.
    Invalidate {
        /// What to re-read.
        resource: ResourceId,
        /// Which query over it, as the identity of a cached answer.
        scope: Value,
    },
    /// A resource is gone.
    Remove {
        /// What no longer exists.
        resource: ResourceId,
    },
    /// Something about the daemon rather than about state: no snapshot carries
    /// it, so no overflow discards it and no re-bootstrap recovers it.
    Lifecycle {
        /// The event kind.
        kind: &'static str,
        /// The whole of what the kind carries.
        payload: Value,
    },
}

/// The kind a readiness or blocked change travels as.
const KIND_ACCOUNT_STATE_CHANGED: &str = "account.state_changed";

/// The kind a draft change travels as.
const KIND_DRAFT_CHANGED: &str = "draft.changed";

impl Event {
    /// The one bridge from the daemon's vocabulary to the wire's.
    ///
    /// Counts invalidate rather than replace because that is what makes them
    /// coalescible; readiness and a draft are replaced whole because they are
    /// small and a client that re-queried them would learn nothing the payload
    /// did not already carry.
    pub fn from_change(change: &Change) -> Event {
        match change {
            Change::AccountReady { .. } | Change::AccountBlocked { .. } => Event::Replace {
                kind: KIND_ACCOUNT_STATE_CHANGED,
                payload: change.payload(),
            },
            Change::DraftUpsert { .. } => Event::Replace {
                kind: KIND_DRAFT_CHANGED,
                payload: change.payload(),
            },
            Change::MailboxCounts {
                account, mailbox, ..
            } => Event::Invalidate {
                resource: ResourceId::new(format!("mailbox:{account}/{mailbox}")),
                scope: json!({"query": "counts"}),
            },
            Change::OutboxCounts { account, .. } => Event::Invalidate {
                resource: ResourceId::new(format!("outbox:{account}")),
                scope: json!({"query": "counts"}),
            },
            Change::DraftRemoved { account, id } => Event::Remove {
                resource: ResourceId::new(format!("draft:{account}/{id}")),
            },
        }
    }

    /// The resource this event addresses, which is half of a [`Event::Replace`]'s
    /// coalescing key and all of an [`Event::Invalidate`]'s.
    ///
    /// A `Replace` of a kind that names no resource replaces the whole of that
    /// kind, so it has no resource and coalesces with every other `Replace` of
    /// the same kind.
    pub fn resource(&self) -> Option<ResourceId> {
        match self {
            Event::Replace { kind, payload } => match *kind {
                KIND_ACCOUNT_STATE_CHANGED => Some(ResourceId::new(format!(
                    "account:{}",
                    payload.get("account")?.as_str()?
                ))),
                KIND_DRAFT_CHANGED => Some(ResourceId::new(format!(
                    "draft:{}/{}",
                    payload.get("account")?.as_str()?,
                    payload.get("id")?.as_str()?
                ))),
                _ => None,
            },
            Event::Invalidate { resource, .. } | Event::Remove { resource } => {
                Some(resource.clone())
            }
            Event::Lifecycle { .. } => None,
        }
    }

    /// The `kind` of the `state.event` this travels as.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Replace { kind, .. } | Event::Lifecycle { kind, .. } => kind,
            Event::Invalidate { .. } => KIND_INVALIDATE,
            Event::Remove { .. } => KIND_REMOVE,
        }
    }

    /// The `payload` of the `state.event` this travels as, always an object so
    /// a kind can gain fields.
    pub fn payload(&self) -> Value {
        match self {
            Event::Replace { payload, .. } | Event::Lifecycle { payload, .. } => payload.clone(),
            Event::Invalidate { resource, scope } => {
                json!({"resource": resource.as_str(), "scope": scope})
            }
            Event::Remove { resource } => json!({"resource": resource.as_str()}),
        }
    }

    /// Whether this is a lifecycle event, the one predicate the overflow path
    /// branches on.
    pub fn is_lifecycle(&self) -> bool {
        matches!(self, Event::Lifecycle { .. })
    }

    /// What this event weighs in a queue: the bytes it would put on the wire,
    /// which is what the byte cap bounds.
    fn weight(&self) -> usize {
        self.payload().to_string().len() + self.kind().len()
    }

    /// The key two events coalesce on, or `None` for an event that merges with
    /// nothing.
    fn key(&self) -> Option<Key> {
        match self {
            Event::Replace { kind, .. } => Some(Key::Replace {
                kind,
                resource: self.resource(),
            }),
            Event::Invalidate { resource, scope } => Some(Key::Invalidate {
                resource: resource.clone(),
                scope: scope.clone(),
            }),
            Event::Remove { .. } | Event::Lifecycle { .. } => None,
        }
    }
}

/// What makes two queued events one.
#[derive(Clone, Debug, PartialEq)]
enum Key {
    /// One kind's answer for one resource, or the whole of a kind that names
    /// no resource.
    Replace {
        kind: &'static str,
        resource: Option<ResourceId>,
    },
    /// One cached query over one resource.
    Invalidate { resource: ResourceId, scope: Value },
}

// ---------------------------------------------------------------------------
// The queue
// ---------------------------------------------------------------------------

/// The bounds one connection's outbound queue is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subscriber {
    /// How many entries may be queued before the queue overflows.
    pub max_events: usize,
    /// How many bytes of queued payload may be held before it overflows.
    pub max_bytes: usize,
}

impl Default for Subscriber {
    /// The two numbers the plan fixes: 512 events and 4 MiB.
    fn default() -> Self {
        Subscriber {
            max_events: 512,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

/// What one [`Outbound::push`] did with its event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    /// Appended as a new entry.
    Queued,
    /// Merged into a queued entry, which took its payload and its revision.
    Coalesced,
    /// Not queued: either the queue is poisoned, or this push overflowed it.
    Overflowed,
}

/// One thing the socket writer sends.
#[derive(Clone, Debug, PartialEq)]
pub enum Outgoing {
    /// A `state.event` notification, at the revision it committed at.
    Event(Revision, Event),
    /// The `state.resync_required` control notification.
    ResyncRequired {
        /// Why the client must bootstrap again.
        reason: String,
    },
}

/// One queued item, either an event or the control message behind it.
#[derive(Debug)]
enum Entry {
    Event {
        revision: Revision,
        event: Event,
        bytes: usize,
    },
    Resync,
}

/// One connection's outbound queue: the only buffer between the fan-out and its
/// socket, and therefore the only thing a stalled client can grow.
#[derive(Debug)]
pub struct Outbound {
    limits: Subscriber,
    queue: VecDeque<Entry>,
    /// The queued events' weight; the control message is not a payload a client
    /// made the daemon hold, so it does not count.
    bytes: usize,
    /// How many entries are events, so the event cap is a cap on payloads
    /// rather than on the marker that empties the queue.
    events: usize,
    poisoned: bool,
}

impl Outbound {
    /// An empty queue held to `limits`.
    pub fn new(limits: Subscriber) -> Self {
        Outbound {
            limits,
            queue: VecDeque::new(),
            bytes: 0,
            events: 0,
            poisoned: false,
        }
    }

    /// Offer one event, in non-decreasing revision order.
    ///
    /// The caller pushes in that order by construction, because a revision is
    /// taken under the same lock that fans the change out; this does not police
    /// it.
    pub fn push(&mut self, revision: Revision, event: Event) -> Push {
        let lifecycle = event.is_lifecycle();
        if self.poisoned && !lifecycle {
            return Push::Overflowed;
        }

        let weight = event.weight();
        if let Some(key) = event.key() {
            if let Some(at) = self.position_of(&key) {
                let queued = match &self.queue[at] {
                    Entry::Event { bytes, .. } => *bytes,
                    Entry::Resync => 0,
                };
                let merged = self.bytes - queued + weight;
                if merged <= self.limits.max_bytes {
                    self.queue.remove(at);
                    self.queue.push_back(Entry::Event {
                        revision,
                        event,
                        bytes: weight,
                    });
                    self.bytes = merged;
                    return Push::Coalesced;
                }
                // A merge that no longer fits is an overflow like any other:
                // falling through keeps one discard path instead of two.
                return self.overflow(revision, event, weight, lifecycle);
            }
        }

        if self.fits(weight) {
            self.append(revision, event, weight);
            return Push::Queued;
        }
        self.overflow(revision, event, weight, lifecycle)
    }

    /// The oldest thing the socket writer should send, or `None`.
    pub fn pop(&mut self) -> Option<Outgoing> {
        match self.queue.pop_front()? {
            Entry::Event {
                revision,
                event,
                bytes,
            } => {
                self.bytes -= bytes;
                self.events -= 1;
                Some(Outgoing::Event(revision, event))
            }
            Entry::Resync => Some(Outgoing::ResyncRequired {
                reason: REASON_QUEUE_OVERFLOW.to_string(),
            }),
        }
    }

    /// How many items [`Outbound::pop`] will return, the control message
    /// included.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether there is nothing to send.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// What the queued events weigh, which never exceeds
    /// [`Subscriber::max_bytes`].
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether the queue has overflowed and is refusing domain events until the
    /// client bootstraps again.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Start again behind a fresh snapshot: the poison clears and everything
    /// queued goes, because the snapshot the client is about to apply
    /// supersedes all of it.
    pub fn rebootstrap(&mut self) {
        self.queue.clear();
        self.bytes = 0;
        self.events = 0;
        self.poisoned = false;
    }

    /// Where a queued entry shares `key`, if one does.
    fn position_of(&self, key: &Key) -> Option<usize> {
        self.queue.iter().position(|entry| match entry {
            Entry::Event { event, .. } => event.key().as_ref() == Some(key),
            Entry::Resync => false,
        })
    }

    /// Whether one more event of `weight` keeps both caps.
    fn fits(&self, weight: usize) -> bool {
        self.events < self.limits.max_events && self.bytes + weight <= self.limits.max_bytes
    }

    fn append(&mut self, revision: Revision, event: Event, weight: usize) {
        self.queue.push_back(Entry::Event {
            revision,
            event,
            bytes: weight,
        });
        self.bytes += weight;
        self.events += 1;
    }

    /// Discard the queued domain events, keep the lifecycle ones, poison the
    /// queue once, and take the offered event if it is a lifecycle event that
    /// now fits.
    fn overflow(
        &mut self,
        revision: Revision,
        event: Event,
        weight: usize,
        lifecycle: bool,
    ) -> Push {
        self.queue.retain(|entry| match entry {
            Entry::Event { event, .. } => event.is_lifecycle(),
            Entry::Resync => true,
        });
        self.bytes = self
            .queue
            .iter()
            .map(|entry| match entry {
                Entry::Event { bytes, .. } => *bytes,
                Entry::Resync => 0,
            })
            .sum();
        self.events = self
            .queue
            .iter()
            .filter(|entry| matches!(entry, Entry::Event { .. }))
            .count();
        if !self.poisoned {
            self.poisoned = true;
            self.queue.push_back(Entry::Resync);
        }
        if lifecycle && self.fits(weight) {
            self.append(revision, event, weight);
            return Push::Queued;
        }
        Push::Overflowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env hook is absent unless it is a number above zero, exactly as the
    /// readiness hook behaves.
    #[test]
    fn the_burst_is_absent_unless_it_is_a_positive_number() {
        assert_eq!(FAKE_EVENT_BURST_ENV, "MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST");
        assert!(fake_event_burst().is_none());
    }

    /// The two wire kinds this unit adds, which `docs/daemon-protocol.md` and
    /// the two `notification.state_*` fixtures spell the same way.
    #[test]
    fn the_two_new_kinds_are_the_documented_strings() {
        assert_eq!(KIND_INVALIDATE, "state.invalidate");
        assert_eq!(KIND_REMOVE, "state.remove");
        assert_eq!(REASON_QUEUE_OVERFLOW, "event_queue_overflow");
    }

    /// A payload big enough to matter weighs at least what it carries, which is
    /// what makes the byte cap a bound on memory.
    #[test]
    fn an_events_weight_covers_its_payload() {
        let event = Event::Invalidate {
            resource: ResourceId::new("mailbox:alpha/inbox"),
            scope: json!({"blob": "x".repeat(4096)}),
        };
        assert!(event.weight() >= 4096);
    }

    /// Only the two coalescible variants have a key, and a `Remove` has none in
    /// either direction.
    #[test]
    fn only_replace_and_invalidate_carry_a_coalescing_key() {
        assert!(Event::from_change(&Change::AccountReady {
            account: "alpha".to_string()
        })
        .key()
        .is_some());
        assert!(Event::from_change(&Change::MailboxCounts {
            account: "alpha".to_string(),
            mailbox: "inbox".to_string(),
            total: 0,
            unread: 0,
            badge: 0,
        })
        .key()
        .is_some());
        assert!(Event::from_change(&Change::DraftRemoved {
            account: "alpha".to_string(),
            id: "d1".to_string(),
        })
        .key()
        .is_none());
        assert!(Event::Lifecycle {
            kind: "daemon.shutting_down",
            payload: json!({}),
        }
        .key()
        .is_none());
    }
}
