//! The bootstrap ordering tests (P1a-U6, ticket #0119).
//!
//! Written against `tests/BOOTSTRAP_CONTRACT.md` before `ipc_bench::bootstrap`
//! exists, so this file does not compile until the I half lands the module with
//! the names that contract fixes.
//!
//! Every race is placed with a hook rather than raced with a thread: a hook runs
//! synchronously on the bootstrapping thread at a named boundary, so the same
//! interleaving happens on every run, under `--test-threads=1` and under the
//! default alike. Draining uses `try_recv`, which never blocks, so a test that
//! loses an event fails on an assertion instead of hanging.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// One `use` per contract item, so a build against a crate that does not have
// the module yet names every missing item in its own error rather than stopping
// at the crate. That output is the proof that these tests are stub-free.
use ipc_bench::bootstrap::apply;
use ipc_bench::bootstrap::Boundary;
use ipc_bench::bootstrap::Event;
use ipc_bench::bootstrap::EventKind;
use ipc_bench::bootstrap::EventStream;
use ipc_bench::bootstrap::InstanceId;
use ipc_bench::bootstrap::RecvError;
use ipc_bench::bootstrap::Revision;
use ipc_bench::bootstrap::Snapshot;
use ipc_bench::bootstrap::StateHub;

const INSTANCE: InstanceId = InstanceId(7);

fn upsert(id: u64, value: &str) -> EventKind {
    EventKind::Upsert { id, value: value.to_string() }
}

/// A subscriber that keeps what a real client keeps: the snapshot it
/// bootstrapped, the stream, and a count of how many times each revision was
/// delivered to it.
struct Client {
    snapshot: Snapshot,
    stream: EventStream,
    bootstrap_revision: Revision,
    deliveries: BTreeMap<u64, usize>,
    order: Vec<u64>,
}

impl Client {
    fn bootstrap(hub: &StateHub) -> Self {
        let (snapshot, revision, instance_id, stream) = hub.bootstrap();
        assert_eq!(instance_id, INSTANCE, "bootstrap returned another instance");
        assert_eq!(instance_id, stream.instance_id(), "stream and response disagree on the instance");
        assert_eq!(
            snapshot.revision, revision,
            "the snapshot must be captured at the revision the response reports"
        );
        Self {
            snapshot,
            stream,
            bootstrap_revision: revision,
            deliveries: BTreeMap::new(),
            order: Vec::new(),
        }
    }

    /// Take everything queued right now, applying it in the order it arrives.
    /// Asserts the order invariant on the way through, so every test gets it.
    fn drain(&mut self) -> Result<(), RecvError> {
        while let Some(event) = self.stream.try_recv()? {
            if let Some(&last) = self.order.last() {
                assert!(
                    event.revision.0 > last,
                    "revisions must be strictly increasing: {last} then {}",
                    event.revision.0
                );
            }
            assert!(
                event.revision > self.bootstrap_revision,
                "revision {} is at or below the bootstrap revision {} and belongs to the snapshot",
                event.revision.0,
                self.bootstrap_revision.0
            );
            *self.deliveries.entry(event.revision.0).or_insert(0) += 1;
            self.order.push(event.revision.0);
            apply(&mut self.snapshot, &event);
        }
        Ok(())
    }

    fn drain_ok(&mut self) {
        self.drain().expect("no gap expected on this stream");
    }

    fn deliveries(&self, revision: Revision) -> usize {
        self.deliveries.get(&revision.0).copied().unwrap_or(0)
    }

    /// Every revision above `floor`, in delivery order.
    fn order_above(&self, floor: Revision) -> Vec<u64> {
        self.order.iter().copied().filter(|r| *r > floor.0).collect()
    }
}

/// A hub whose `boundary` hook commits `kind` the first time it fires, plus the
/// slot holding the revision that mutation was given (0 until the hook runs).
fn hub_racing_at(boundary: Boundary, kind: EventKind) -> (Arc<StateHub>, Arc<AtomicU64>) {
    let hub = Arc::new(StateHub::new(INSTANCE));
    let assigned = Arc::new(AtomicU64::new(0));

    let inner = Arc::clone(&hub);
    let slot = Arc::clone(&assigned);
    let fired = AtomicBool::new(false);
    hub.with_race_hook(
        boundary,
        Box::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let revision = inner.mutate(kind.clone());
            slot.store(revision.0, Ordering::SeqCst);
        }),
    );

    (hub, assigned)
}

/// The shared body of the five boundary tests: one change committed at
/// `boundary`, one committed before the bootstrap and one after it, and the
/// client has to end up holding exactly what the hub holds.
fn assert_boundary_converges(boundary: Boundary) {
    let (hub, assigned) = hub_racing_at(boundary, upsert(1, "raced"));
    let before = hub.mutate(upsert(0, "committed before the bootstrap"));

    let mut client = Client::bootstrap(&hub);

    let raced = Revision(assigned.load(Ordering::SeqCst));
    assert_ne!(raced.0, 0, "{boundary:?}: the race hook never fired");
    assert!(
        raced > before,
        "{boundary:?}: the raced mutation must get a fresh revision, got {} after {}",
        raced.0,
        before.0
    );

    let after = hub.mutate(upsert(2, "committed after the bootstrap"));
    client.drain_ok();

    let authoritative = hub.state();
    assert_eq!(
        client.snapshot, authoritative,
        "{boundary:?}: the client did not converge on the authoritative state"
    );
    assert_eq!(
        client.snapshot.rows.get(&1).map(String::as_str),
        Some("raced"),
        "{boundary:?}: the raced row is missing from the converged state"
    );

    // Exactly once, or through the snapshot: a revision at or below R is in the
    // snapshot and must not also arrive on the stream, and one above R must
    // arrive exactly once.
    assert!(
        client.deliveries(raced) <= 1,
        "{boundary:?}: revision {} was delivered {} times",
        raced.0,
        client.deliveries(raced)
    );
    if raced <= client.bootstrap_revision {
        assert_eq!(
            client.deliveries(raced),
            0,
            "{boundary:?}: revision {} is already in the snapshot",
            raced.0
        );
    }
    for revision in (client.bootstrap_revision.0 + 1)..=authoritative.revision.0 {
        assert_eq!(
            client.deliveries(Revision(revision)),
            1,
            "{boundary:?}: revision {revision} was delivered {} times, expected exactly one",
            client.deliveries(Revision(revision))
        );
    }
    assert_eq!(
        client.deliveries(after),
        1,
        "{boundary:?}: the post-bootstrap mutation must arrive on the stream"
    );
    assert_eq!(
        client.deliveries(before),
        0,
        "{boundary:?}: the pre-bootstrap mutation belongs to the snapshot only"
    );
}

#[test]
fn a_mutation_before_the_subscriber_registers_converges() {
    assert_boundary_converges(Boundary::BeforeRegister);
}

#[test]
fn a_mutation_between_register_and_capture_converges() {
    assert_boundary_converges(Boundary::AfterRegister);
}

#[test]
fn a_mutation_between_capture_and_queue_start_converges() {
    assert_boundary_converges(Boundary::AfterCapture);
}

#[test]
fn a_mutation_between_queue_start_and_response_write_converges() {
    assert_boundary_converges(Boundary::AfterQueueStart);
}

#[test]
fn a_mutation_after_the_response_write_converges() {
    assert_boundary_converges(Boundary::AfterResponseWrite);
}

#[test]
fn a_revision_gap_is_reported_and_poisons_the_stream() {
    let hub = Arc::new(StateHub::new(INSTANCE));
    let mut client = Client::bootstrap(&hub);

    let delivered = hub.mutate(upsert(1, "one"));
    client.drain_ok();
    assert_eq!(client.deliveries(delivered), 1);

    let skipped = Revision(delivered.0 + 3);
    hub.publish_unchecked(Event { revision: skipped, kind: upsert(2, "from the future") });

    let expected = Revision(delivered.0 + 1);
    let gap = RecvError::Gap { expected, seen: skipped };
    assert_eq!(client.drain(), Err(gap.clone()), "the gap must be reported, not applied");
    assert_eq!(client.drain(), Err(gap), "the stream stays poisoned until the client re-bootstraps");
    assert!(
        !client.snapshot.rows.contains_key(&2),
        "an event past a gap must not be applied"
    );
}

#[test]
fn a_duplicate_revision_is_dropped() {
    let hub = Arc::new(StateHub::new(INSTANCE));
    let seeded = hub.mutate(upsert(0, "seeded before the bootstrap"));
    let mut client = Client::bootstrap(&hub);

    let delivered = hub.mutate(upsert(1, "one"));
    client.drain_ok();
    assert_eq!(client.deliveries(delivered), 1);

    // The same revision again, and one from below the bootstrap watermark.
    hub.publish_unchecked(Event { revision: delivered, kind: upsert(1, "one") });
    hub.publish_unchecked(Event { revision: seeded, kind: upsert(0, "stale replay") });
    client.drain_ok();

    assert_eq!(client.deliveries(delivered), 1, "a repeated revision must not be delivered twice");
    assert_eq!(client.deliveries(seeded), 0, "a revision at or below R must not be delivered");
    assert_eq!(
        client.snapshot.rows.get(&0).map(String::as_str),
        Some("seeded before the bootstrap"),
        "the stale replay must not overwrite the row"
    );
    assert_eq!(client.snapshot, hub.state());

    // The stream keeps working after the drops.
    let next = hub.mutate(EventKind::Delete { id: 1 });
    client.drain_ok();
    assert_eq!(client.deliveries(next), 1);
    assert_eq!(client.snapshot, hub.state());
}

#[test]
fn two_clients_observe_the_same_order() {
    let hub = Arc::new(StateHub::new(INSTANCE));
    hub.mutate(upsert(0, "zero"));

    let mut early = Client::bootstrap(&hub);
    hub.mutate(upsert(1, "one"));

    let mut late = Client::bootstrap(&hub);
    let mutations = [
        upsert(2, "two"),
        EventKind::Delete { id: 0 },
        upsert(1, "one, replaced"),
        EventKind::Delete { id: 404 },
    ];
    let mut revisions = Vec::new();
    for kind in mutations {
        revisions.push(hub.mutate(kind).0);
    }

    early.drain_ok();
    late.drain_ok();

    let authoritative = hub.state();
    assert_eq!(early.snapshot, authoritative, "the early client did not converge");
    assert_eq!(late.snapshot, authoritative, "the late client did not converge");

    assert!(
        late.bootstrap_revision >= early.bootstrap_revision,
        "the later bootstrap must not capture an older revision"
    );
    assert_eq!(
        early.order_above(late.bootstrap_revision),
        late.order_above(late.bootstrap_revision),
        "the two streams disagree on the order of the revisions they share"
    );
    assert_eq!(late.order, revisions, "the late client must see every revision committed after it");
    for revision in &revisions {
        assert_eq!(early.deliveries(Revision(*revision)), 1);
        assert_eq!(late.deliveries(Revision(*revision)), 1);
    }
}
