# Bootstrap ordering contract (P1a-U6)

`tests/bootstrap_race.rs` is written against this contract and compiles only once a module
`ipc_bench::bootstrap` provides every item below with these exact names, shapes and behaviours.
The tests are the T half of P1a-U6; this file is the specification the I half implements.
Nothing here may be relaxed to make a test pass: a contract error is fixed in this document first,
in writing, and only then in the test file.

The model is deliberately tiny. It carries no store, no socket and no JSON, because the risk P1a-U6
retires is the ordering of the five bootstrap steps, not the transport that P1a-U1 already measured.
A row is a `u64` key and a `String` value; that is enough state to prove a lost, duplicated or
reordered event.

## What the implementer adds

- `src/lib.rs`, new, containing exactly `pub mod bootstrap;` and `pub mod proto;`.
  The package is a binary today, so integration tests cannot `use ipc_bench::…` until this file
  exists. Cargo picks up `src/lib.rs` as a lib target named `ipc-bench` with crate name
  `ipc_bench` without any `[lib]` section; the existing `[[bin]]` inference is unaffected.
- `src/bootstrap.rs`, new, the module specified here.
- `src/main.rs` currently declares `mod proto;` alongside `mod client/pool/server/stats/work`.
  Reconcile it: either drop the `mod proto;` line and switch its uses to `use ipc_bench::proto`, or
  leave it and accept that `proto` compiles twice. Either is fine; the tests do not look at the
  binary.
- No new dependencies. The module is `std` only, so `cargo test --offline` resolves from the
  committed `Cargo.lock`.

## Identity

`InstanceId(pub u64)`, deriving `Debug, Clone, Copy, PartialEq, Eq, Hash`.
Opaque in the real protocol; a number here so a test can write `InstanceId(7)` in a `const`.

`Revision(pub u64)`, deriving `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`.
`Revision(0)` is the revision of a hub that has never mutated. The first `mutate` assigns
`Revision(1)`, and every subsequent one the previous value plus one, with no gaps and no reuse
within an instance.

## Snapshot

```rust
pub struct Snapshot {
    pub rows: BTreeMap<u64, String>,
    pub revision: Revision,
}
```

Derives `Debug, Clone, PartialEq, Eq`.
`revision` is the revision the rows were captured at: a snapshot is always internally consistent,
never a mix of two revisions.

## Events

```rust
pub enum EventKind {
    Upsert { id: u64, value: String },
    Delete { id: u64 },
}

pub struct Event {
    pub revision: Revision,
    pub kind: EventKind,
}
```

Both derive `Debug, Clone, PartialEq, Eq`.

Payloads are complete replacements, which is the plan's rule for small resources: `Upsert` carries
the whole value rather than a patch, and `Delete` is the explicit removal event for a stable
identifier that disappeared. There is no field-level patch kind, because its correctness would
depend on every prior event being present, and that is exactly the property the gap test exists to
detect rather than to rely on.

```rust
pub fn apply(snapshot: &mut Snapshot, event: &Event);
```

`Upsert` inserts or replaces `rows[id]`; `Delete` removes `id`, and removing an absent id is not an
error. `snapshot.revision` becomes `max(snapshot.revision, event.revision)`.
`apply` is total and idempotent: applying the same event twice leaves the snapshot equal to
applying it once. It enforces no ordering, because ordering is the stream's job.

## StateHub

`StateHub` is `Send + Sync` and every method takes `&self`; tests hold it as `Arc<StateHub>` so a
race hook can mutate the hub it is registered on.

- `pub fn new(instance_id: InstanceId) -> StateHub` starts empty at `Revision(0)`.
- `pub fn instance_id(&self) -> InstanceId`.
- `pub fn state(&self) -> Snapshot` is the authoritative state, the value every test compares the
  client's converged snapshot against.
- `pub fn revision(&self) -> Revision`, the current authoritative revision.
- `pub fn mutate(&self, kind: EventKind) -> Revision` commits the change, assigns the next
  revision, publishes an `Event` carrying that revision and that kind to every live subscriber, and
  returns the revision it assigned. Commit and publish are one atomic step: no observer can see a
  state that has a change without its event, or an event without its change.
- `pub fn bootstrap(&self) -> (Snapshot, Revision, InstanceId, EventStream)`, specified below.
- `pub fn with_race_hook(&self, boundary: Boundary, hook: Box<dyn Fn() + Send + Sync>)` registers a
  test hook. At most one hook per boundary; a second registration replaces the first. A hook fires
  on **every** `bootstrap()` call, so a test that bootstraps twice guards its hook itself.
- `pub fn publish_unchecked(&self, event: Event)` is the test-only escape hatch: it pushes `event`
  onto every live subscriber queue without touching the authoritative state and without advancing
  the revision counter. It is the only way a test can manufacture a gap or a duplicate, both of
  which a correct hub never produces. Production code would not have it; the spike does, because
  the client-side rules for gaps and duplicates need proving too.

## The bootstrap sequence and its five boundaries

`bootstrap()` is one serialized operation. While it runs, a `mutate` from another thread blocks and
commits either wholly before the capture or wholly after it; it never interleaves with it.

The steps, and the boundary hook fired at each, in order:

1. `Boundary::BeforeRegister` fires.
2. Register the connection as a subscriber.
3. `Boundary::AfterRegister` fires.
4. Capture the snapshot and its revision R.
5. `Boundary::AfterCapture` fires.
6. Begin queuing events whose revision exceeds R.
7. `Boundary::AfterQueueStart` fires.
8. Assemble the response: the snapshot, R, the instance id, the stream handle.
9. `Boundary::AfterResponseWrite` fires.
10. Return. Queued events become readable through the returned stream, in revision order.

```rust
pub enum Boundary {
    BeforeRegister,
    AfterRegister,
    AfterCapture,
    AfterQueueStart,
    AfterResponseWrite,
}
```

Derives `Debug, Clone, Copy, PartialEq, Eq`.

Hooks run synchronously on the bootstrapping thread. A hook may call `mutate`, `state` and
`publish_unchecked` on the same hub from that thread and must not deadlock: a mutation made inside a
hook is treated exactly as one arriving from another connection at that instant. This reentrancy is
what makes the tests deterministic with no threads, no barriers and no sleeps, and it constrains the
implementation: the serialized section cannot be a plain non-reentrant lock held across the hook
calls.

The interesting boundaries are 5 and 7. A mutation at `AfterCapture` has a revision above R and the
queue is not open yet, so a naive implementation loses it. A mutation at `AfterRegister` commits
before the capture, so the snapshot already carries it, and any event the subscriber received for it
is redundant.

## EventStream

The stream is the subscription; there is no separate `Client` type. It is `Send`, and dropping it
unregisters the subscriber.

- `pub fn instance_id(&self) -> InstanceId`.
- `pub fn try_recv(&mut self) -> Result<Option<Event>, RecvError>` never blocks. `Ok(None)` means
  the queue is empty at this instant.
- `pub fn recv(&mut self) -> Result<Event, RecvError>` blocks until an event is available or the hub
  is gone. The tests use `try_recv` exclusively so that they terminate rather than hang when the
  implementation drops an event.

```rust
pub enum RecvError {
    Gap { expected: Revision, seen: Revision },
    Closed,
}
```

Derives `Debug, Clone, PartialEq, Eq`.

The stream holds a watermark, the highest revision it has already accounted for, initialised to R.
For each queued event, in queue order:

- revision <= watermark: dropped silently, and the stream moves to the next event.
  This is the pick between silent drop and an explicit `Duplicate` signal, and it is the drop: the
  plan's rule is that clients ignore duplicate or older revisions, and a signal a correct daemon
  never emits would be a shape Phase 3a has to carry for nothing. It also collapses the
  register-to-capture overlap: an event whose change is already in the snapshot is not delivered a
  second time.
- revision == watermark + 1: delivered, watermark advances.
- revision > watermark + 1: `Err(Gap { expected: watermark + 1, seen: revision })`. The stream is
  then poisoned: every later `recv` and `try_recv` returns that same error, the watermark does not
  move, and no event is delivered. A real client re-bootstraps here.
- The hub is gone: `Err(Closed)`.

`Closed` outranks nothing: queued events are drained before it is reported.

## Invariants the tests assert

- No loss. Every revision above R reaches the stream.
- Order. Revisions delivered by one stream are strictly increasing.
- At most once. A stream delivers a given revision at most once.
- Convergence. The snapshot at R, plus every delivered event applied in order, equals
  `hub.state()`, rows and revision both.
- Exactly once or by replacement. A change made at a boundary reaches the client either as one
  delivered event or as part of the captured snapshot, never as two applications of two different
  values and never as none.
- Shared order. Two streams of the same hub deliver the revisions they have in common in the same
  order.

## What this contract leaves open

- `Event` carries no `instance_id`, though the wire notification in section 3.0 of the plan does.
  The stream is per-instance and exposes `instance_id()`, so the field would be constant across a
  stream and untestable here. Phase 3a puts it back on the notification.
- Backpressure, coalescing and `state.resync_required` are out of scope. The queue is unbounded in
  the spike, so no test forces a resync.
- `Snapshot` models one flat map, not the account, mailbox, draft and operation resources the real
  snapshot carries. The ordering algorithm does not depend on the shape of the state.
- Capabilities, step 4 of the plan's sequence, are omitted from the return tuple: nothing in the
  spike negotiates.
- The hooks make the race deterministic by placing the mutation at the boundary rather than by
  racing threads at it. A genuinely concurrent stress test is worth having in Phase 3a against the
  real hub; it would not be reproducible enough to be a gate here.
