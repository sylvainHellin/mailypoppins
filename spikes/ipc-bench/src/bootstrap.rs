//! P1a-U6: the atomic bootstrap ordering prototype.
//!
//! The specification is `tests/BOOTSTRAP_CONTRACT.md`; this module is its
//! implementation and adds nothing the contract does not name. No store, no
//! socket, no JSON: the risk being retired is the ordering of the five
//! bootstrap steps, and a `BTreeMap<u64, String>` is enough state to expose a
//! lost, duplicated or reordered event.
//!
//! The whole correctness argument is one line of the sequence: the subscriber
//! is registered *before* the snapshot is captured, so from the capture onwards
//! every committed event is already landing in its queue, and the stream's
//! watermark - initialised to the captured revision R - discards the ones the
//! snapshot already contains. "Begin queuing events above R" is therefore not a
//! separate switch to flip; it is a consequence of registering first, which is
//! precisely why a naive implementation that registers last loses the mutation
//! at `AfterCapture`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::{self, ThreadId};

/// Lock without propagating poison: a panicking race hook must not turn every
/// later assertion in the test binary into a second, unrelated panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Opaque in the real protocol, a number here so a test can write it in a
/// `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u64);

/// `Revision(0)` is a hub that has never mutated; the first `mutate` assigns
/// `Revision(1)`, with no gaps and no reuse within an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(pub u64);

/// State captured at one revision, never a mix of two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: BTreeMap<u64, String>,
    pub revision: Revision,
}

/// Payloads are complete replacements, the plan's rule for small resources:
/// `Upsert` carries the whole value and `Delete` is the explicit removal of a
/// stable identifier. There is no field-level patch kind, whose correctness
/// would depend on every prior event being present - the property the gap test
/// exists to detect rather than to rely on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Upsert { id: u64, value: String },
    Delete { id: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub revision: Revision,
    pub kind: EventKind,
}

/// Total and idempotent: applying the same event twice leaves the snapshot
/// equal to applying it once, and deleting an absent id is not an error. It
/// enforces no ordering, because ordering is the stream's job.
pub fn apply(snapshot: &mut Snapshot, event: &Event) {
    apply_kind(&mut snapshot.rows, &event.kind);
    snapshot.revision = snapshot.revision.max(event.revision);
}

fn apply_kind(rows: &mut BTreeMap<u64, String>, kind: &EventKind) {
    match kind {
        EventKind::Upsert { id, value } => {
            rows.insert(*id, value.clone());
        }
        EventKind::Delete { id } => {
            rows.remove(id);
        }
    }
}

/// The five points of the bootstrap sequence a test can wedge a mutation into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    BeforeRegister,
    AfterRegister,
    AfterCapture,
    AfterQueueStart,
    AfterResponseWrite,
}

impl Boundary {
    const COUNT: usize = 5;

    fn index(self) -> usize {
        match self {
            Boundary::BeforeRegister => 0,
            Boundary::AfterRegister => 1,
            Boundary::AfterCapture => 2,
            Boundary::AfterQueueStart => 3,
            Boundary::AfterResponseWrite => 4,
        }
    }
}

/// `Gap` is the client's signal to re-bootstrap; `Closed` means the hub is
/// gone, and queued events are drained before it is reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvError {
    Gap { expected: Revision, seen: Revision },
    Closed,
}

type Hook = Arc<dyn Fn() + Send + Sync>;

/// A reentrant gate. `bootstrap` holds it across the hook calls, so a `mutate`
/// from another thread blocks and commits wholly before or wholly after the
/// capture, while a `mutate` from a hook - which runs on the bootstrapping
/// thread - re-enters instead of deadlocking. A plain `Mutex` cannot do both.
#[derive(Default)]
struct GateState {
    owner: Option<ThreadId>,
    depth: usize,
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    released: Condvar,
}

impl Gate {
    fn enter(&self) -> GateGuard<'_> {
        let me = thread::current().id();
        let mut state = lock(&self.state);
        while state.owner.is_some_and(|owner| owner != me) {
            state = self
                .released
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        state.owner = Some(me);
        state.depth += 1;
        GateGuard { gate: self }
    }
}

struct GateGuard<'a> {
    gate: &'a Gate,
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.gate.state);
        state.depth -= 1;
        if state.depth == 0 {
            state.owner = None;
            self.gate.released.notify_all();
        }
    }
}

#[derive(Default)]
struct QueueState {
    events: VecDeque<Event>,
    closed: bool,
}

#[derive(Default)]
struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl Queue {
    fn push(&self, event: Event) {
        let mut state = lock(&self.state);
        state.events.push_back(event);
        self.ready.notify_all();
    }
}

struct HubState {
    rows: BTreeMap<u64, String>,
    revision: Revision,
    subscribers: Vec<(u64, Arc<Queue>)>,
}

struct Shared {
    instance_id: InstanceId,
    gate: Gate,
    state: Mutex<HubState>,
    hooks: Mutex<Vec<Option<Hook>>>,
    next_subscriber: AtomicU64,
}

impl Shared {
    /// Publish to every live subscriber. Callers hold the state lock, so commit
    /// and publish are one atomic step: no observer sees a state carrying a
    /// change without its event, or an event without its change.
    fn publish(state: &HubState, event: &Event) {
        for (_, queue) in &state.subscribers {
            queue.push(event.clone());
        }
    }
}

/// The authoritative state and its subscribers. `Send + Sync`, every method
/// takes `&self`, so a race hook can mutate the hub it is registered on.
pub struct StateHub {
    shared: Arc<Shared>,
}

impl StateHub {
    pub fn new(instance_id: InstanceId) -> StateHub {
        StateHub {
            shared: Arc::new(Shared {
                instance_id,
                gate: Gate::default(),
                state: Mutex::new(HubState {
                    rows: BTreeMap::new(),
                    revision: Revision(0),
                    subscribers: Vec::new(),
                }),
                hooks: Mutex::new(vec![None; Boundary::COUNT]),
                next_subscriber: AtomicU64::new(0),
            }),
        }
    }

    pub fn instance_id(&self) -> InstanceId {
        self.shared.instance_id
    }

    pub fn state(&self) -> Snapshot {
        let state = lock(&self.shared.state);
        Snapshot { rows: state.rows.clone(), revision: state.revision }
    }

    pub fn revision(&self) -> Revision {
        lock(&self.shared.state).revision
    }

    /// Commit, assign the next revision, publish, return that revision.
    pub fn mutate(&self, kind: EventKind) -> Revision {
        let _serialized = self.shared.gate.enter();
        let mut state = lock(&self.shared.state);
        let revision = Revision(state.revision.0 + 1);
        state.revision = revision;
        apply_kind(&mut state.rows, &kind);
        let event = Event { revision, kind };
        Shared::publish(&state, &event);
        revision
    }

    /// One serialized operation: the ten steps of the contract, with the five
    /// boundary hooks fired in order.
    pub fn bootstrap(&self) -> (Snapshot, Revision, InstanceId, EventStream) {
        let _serialized = self.shared.gate.enter();

        // 1.
        self.fire(Boundary::BeforeRegister);

        // 2. Register first. Everything committed from here on is queued, which
        //    is what saves the mutation raced at `AfterCapture`.
        let queue = Arc::new(Queue::default());
        let subscriber_id = self.shared.next_subscriber.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = lock(&self.shared.state);
            state.subscribers.push((subscriber_id, Arc::clone(&queue)));
        }

        // 3.
        self.fire(Boundary::AfterRegister);

        // 4. Capture. Internally consistent by construction: one lock, rows and
        //    revision taken together.
        let snapshot = self.state();
        let r = snapshot.revision;

        // 5.
        self.fire(Boundary::AfterCapture);

        // 6. Queue events above R. The queue has been filling since step 2; the
        //    stream's watermark, set to R below, is what makes "above R" true,
        //    and it also collapses the register-to-capture overlap by dropping
        //    events the snapshot already carries.

        // 7.
        self.fire(Boundary::AfterQueueStart);

        // 8. Assemble the response.
        let stream = EventStream {
            instance_id: self.shared.instance_id,
            subscriber_id,
            queue,
            hub: Arc::downgrade(&self.shared),
            watermark: r,
            poison: None,
        };

        // 9.
        self.fire(Boundary::AfterResponseWrite);

        // 10.
        (snapshot, r, self.shared.instance_id, stream)
    }

    /// At most one hook per boundary; a second registration replaces the first.
    /// A hook fires on every `bootstrap()` call.
    pub fn with_race_hook(&self, boundary: Boundary, hook: Box<dyn Fn() + Send + Sync>) {
        let mut hooks = lock(&self.shared.hooks);
        hooks[boundary.index()] = Some(Arc::from(hook));
    }

    /// Test-only escape hatch: push `event` onto every live subscriber queue
    /// without touching the authoritative state and without advancing the
    /// revision counter. The only way to manufacture a gap or a duplicate,
    /// neither of which a correct hub produces. Production code would not have
    /// it; the client-side rules for both still need proving.
    pub fn publish_unchecked(&self, event: Event) {
        let state = lock(&self.shared.state);
        Shared::publish(&state, &event);
    }

    /// Hooks run synchronously on the bootstrapping thread, with no hub lock
    /// held, so a hook may call `mutate`, `state` and `publish_unchecked`.
    fn fire(&self, boundary: Boundary) {
        let hook = lock(&self.shared.hooks)[boundary.index()].clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl Drop for StateHub {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        for (_, queue) in state.subscribers.drain(..) {
            lock(&queue.state).closed = true;
            queue.ready.notify_all();
        }
    }
}

/// The subscription itself: there is no separate client type. `Send`; dropping
/// it unregisters the subscriber.
pub struct EventStream {
    instance_id: InstanceId,
    subscriber_id: u64,
    queue: Arc<Queue>,
    hub: Weak<Shared>,
    /// Highest revision already accounted for, initialised to R.
    watermark: Revision,
    poison: Option<RecvError>,
}

impl EventStream {
    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    /// Never blocks. `Ok(None)` means the queue is empty at this instant.
    pub fn try_recv(&mut self) -> Result<Option<Event>, RecvError> {
        if let Some(error) = &self.poison {
            return Err(error.clone());
        }
        loop {
            let queued = {
                let mut state = lock(&self.queue.state);
                match state.events.pop_front() {
                    Some(event) => Some(event),
                    None if state.closed => return Err(RecvError::Closed),
                    None => None,
                }
            };
            let Some(event) = queued else {
                // Queued events are drained before `Closed` is reported, so the
                // hub-gone check happens only on an empty queue.
                return if self.hub.upgrade().is_none() {
                    Err(RecvError::Closed)
                } else {
                    Ok(None)
                };
            };
            if let Some(event) = self.accept(event)? {
                return Ok(Some(event));
            }
        }
    }

    /// Blocks until an event is available or the hub is gone.
    pub fn recv(&mut self) -> Result<Event, RecvError> {
        loop {
            if let Some(event) = self.try_recv()? {
                return Ok(event);
            }
            let state = lock(&self.queue.state);
            if state.events.is_empty() && !state.closed && self.hub.strong_count() > 0 {
                let _unblocked = self.queue.ready.wait(state);
            }
        }
    }

    /// The watermark rules: below or at it, drop silently (the plan's rule is
    /// that clients ignore duplicate or older revisions, and a signal a correct
    /// daemon never emits would be a shape Phase 3a carries for nothing);
    /// exactly one above, deliver; further above, report the gap and poison the
    /// stream, since a real client re-bootstraps there.
    fn accept(&mut self, event: Event) -> Result<Option<Event>, RecvError> {
        if event.revision <= self.watermark {
            return Ok(None);
        }
        if event.revision.0 == self.watermark.0 + 1 {
            self.watermark = event.revision;
            return Ok(Some(event));
        }
        let gap = RecvError::Gap {
            expected: Revision(self.watermark.0 + 1),
            seen: event.revision,
        };
        self.poison = Some(gap.clone());
        Err(gap)
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        if let Some(shared) = self.hub.upgrade() {
            let mut state = lock(&shared.state);
            state.subscribers.retain(|(id, _)| *id != self.subscriber_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>() {}

    #[test]
    fn the_hub_is_shareable_and_the_stream_is_sendable() {
        assert_send_sync::<StateHub>();
        assert_send::<EventStream>();
    }

    #[test]
    fn a_dropped_hub_closes_the_stream_after_the_queue_drains() {
        let hub = StateHub::new(InstanceId(1));
        let (_snapshot, _r, _id, mut stream) = hub.bootstrap();
        hub.mutate(EventKind::Upsert { id: 1, value: "one".to_string() });
        drop(hub);
        assert!(matches!(stream.try_recv(), Ok(Some(_))));
        assert_eq!(stream.try_recv(), Err(RecvError::Closed));
    }

    #[test]
    fn a_dropped_stream_unregisters_its_subscriber() {
        let hub = StateHub::new(InstanceId(1));
        let (_snapshot, _r, _id, stream) = hub.bootstrap();
        assert_eq!(lock(&hub.shared.state).subscribers.len(), 1);
        drop(stream);
        assert_eq!(lock(&hub.shared.state).subscribers.len(), 0);
    }
}
