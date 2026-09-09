# Bootstrap ordering, what Phase 3a must reproduce

Unit P1a-U6 of `.agents/workflow/native-gui-daemon/plan.md`, ticket #0119, carried out of the spike so the algorithm survives it.
The prototype and its forced-race tests live in `spikes/ipc-bench/src/bootstrap.rs` and `spikes/ipc-bench/tests/bootstrap_race.rs`, with the API fixed in `spikes/ipc-bench/tests/BOOTSTRAP_CONTRACT.md`.
This file is the part Phase 3a implements against the real hub.

## The two rules

Register the subscriber before capturing the snapshot.
Every event committed from the capture onwards is then already queued, which is exactly what a register-last implementation loses at the boundary between the capture and the start of queuing.

Initialise the client watermark to R, the revision the snapshot was captured at, and silently drop every event whose revision is at or below it.
That is what makes the register-first ordering safe: an event for a change the snapshot already carries arrives, is recognised as redundant, and is dropped rather than applied a second time.
The drop is silent by design. The plan's rule is that clients ignore duplicate or older revisions, and a `Duplicate` signal a correct daemon never emits would be a shape Phase 3a has to carry for nothing.

Together they give the invariant the tests assert: a change made anywhere around the bootstrap reaches the client either as one delivered event or as part of the captured snapshot, never as two applications and never as none.

## The five boundaries and what each one produces

Boundaries are the points in the ten-step sequence where a mutation from another connection can land.
Each was forced deterministically by a race hook rather than by racing threads, and the column below is what the client actually observed.

| Boundary | Mutation lands | Client observes |
| --- | --- | --- |
| BeforeRegister | before the subscriber exists | snapshot only, no event |
| AfterRegister | after register, before capture | snapshot only; the event is queued, then dropped by the watermark |
| AfterCapture | after capture, before queuing starts | one delivered event |
| AfterQueueStart | after queuing starts, before the response is assembled | one delivered event |
| AfterResponseWrite | after the response frame | one delivered event |

No boundary needed an idempotent replacement to reach "exactly once", so Phase 3a does not have to carry that shape.
AfterRegister is the one that looks like a duplicate and is not: the change is in the snapshot and the event for it exists, and the watermark is what collapses the two into one application.
AfterCapture is the boundary that breaks a naive implementation, since the revision is above R and the queue is not open yet.

The other client-side rules the prototype pins, for the same reason:

- a revision exactly one above the watermark is delivered and advances it;
- a revision more than one above is a gap, which poisons the stream (every later receive returns the same error, no event is delivered, the watermark does not move) and a real client re-bootstraps there;
- queued events are drained before a closed hub is reported.

## The reentrant-gate lesson

The serialized section of `bootstrap()` cannot be a plain `Mutex`.
A race hook calls `mutate` on the bootstrapping thread, which is the whole reason the tests are deterministic with no threads, no barriers and no sleeps, and a non-reentrant lock held across the hook deadlocks on itself.
The prototype uses a reentrant thread-owned gate: the gate records the thread that holds it, so a reentrant `mutate` from the same thread proceeds while another thread blocks.

Phase 3a inherits the constraint from the product side as well as the test side.
Any callback the hub invokes while holding the bootstrap gate (an event hook, a metric, a log sink that reads state) can re-enter it, and the serialized section has to survive that rather than assume it cannot happen.
