---
id: 0121
title: Phase 3a of the daemon migration, the dispatcher, the canonical state, events and operations
type: feature
priority: now
status: done
created: 2026-09-09
---

Fourth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md`), after #0118, #0119 and #0120.

Phase 3a gives the daemon a spine: one dispatcher between the wire and the domain, one canonical state with a revision counter, one outbound queue per connection, and a registry for work that outlives the call that started it.
Nothing here owns domain state yet and nothing here takes an engine lock, so a client that never sets `--daemon` is bit-for-bit unaffected and `cargo install --path .` still ships an `mp` without a byte of it.

## Why a spine before any domain

Phase 2 served two read-only methods by hand: the server matched a method name, called a function and wrote a response.
That does not scale to the forty methods of Phase 5, and more to the point it has nowhere to put the three things a GUI needs and a CLI never did: a state every client mirrors, an event stream that keeps those mirrors current, and an operation that answers before it finishes.
So Phase 3a builds those four pieces against contract tests and nothing else, which is why the phase adds 5981 lines of test in four files against 3383 lines in the seven files it creates.

Each pair is a T unit that pins the contract and an I unit that makes it green.
The T unit's proof is that `cargo test --workspace` is unchanged and green while `cargo test --workspace --features daemon` fails to compile with unresolved imports naming exactly the contract items; all four are quoted in `docs/baselines/phase3a-gate-evidence.md`.

## What landed

Eight units in eight commits.

- P3a-U1 (`b6b07fe`) and P3a-U2 (`5d47786`): `tests/daemon_dispatcher.rs` (813 lines as pinned, 817 after P3a-U7's pre-approved amendment) and `src/daemon/dispatch.rs` (449, 520 after P3a-U8 added `CancelScope`). `Method`, `ClientCtx`, `MethodKind`, `MethodSpec`, `Outcome`, `ResourceId`, `CancelToken` and the `Dispatcher` itself, with `account.list` and `message.list` re-homed onto it. The handshake's capability list is derived from `specs()` behind the two lifecycle methods, so a method cannot be served unadvertised.
- P3a-U3 (`bcd55c4`) and P3a-U4 (`cc8a944`): `tests/daemon_bootstrap.rs` (1878) and `src/daemon/state/` (940). One `CanonicalState` per process, `subscribe` then `bootstrap` as the register-before-capture rule, a fresh state at `Revision(1)` so the wire never carries the `0` sentinel, and `state.bootstrap` as a query that reports the calling connection's negotiated capabilities. `mp-client` gains `StateTracker`, `Observe` and `Connection::next_notification`.
- P3a-U5 (`f01ace1`) and P3a-U6 (`75c95e2`): `tests/daemon_events.rs` (1710) and `src/daemon/state/events.rs` (546) plus the connection loop in `server.rs`. `Event::from_change` as the one bridge from a `Change` to the wire, coalescing keys, the 512-event and 4 MiB caps, the discard-and-poison overflow, and a connection loop of three `select!` arms where a blocked write delays nobody.
- P3a-U7 (`cf78730`) and P3a-U8 (`a493229`): `tests/daemon_operations.rs` (1576) and `src/daemon/operations.rs` (816). The registry, its forward-only state machine, `operation.status` and `operation.cancel`, `CancelScope` as the fourth field of `MethodSpec`, and `mailbox.list` as the third read-only method.
- P3a-U9 (this ticket): the protocol document's inconsistencies between units, this file, the CHANGELOG and BACKLOG entries, and `docs/baselines/phase3a-gate-evidence.md`.

## The shapes worth remembering

Cancellation is the method's own answer, not something the dispatcher does to it.
A method that has already committed a write reports the write; being reported as cancelled behind its own back would make `-32008` a lie a client cannot check.

A bootstrap registers the connection as a subscriber before it captures the snapshot, and the client watermarks at the captured revision and drops everything at or below it.
Register-last loses exactly the changes that land between the capture and the start of queuing; register-first delivers some of them twice, and the watermark is what makes the second one free.

Counts invalidate, small resources replace.
A hundred count changes for one mailbox coalesce into one thing to re-read only because they name a resource and a query rather than carrying a number, which is the whole reason `Event` is not `Change`.

Lifecycle events survive an overflow because no snapshot carries them.
A progress report that a re-bootstrap would not bring back must not be discarded by one, which is the rule; the exception this build ships is under Deviations.

The bootstrap gate is reentrant and thread-owned rather than a plain `Mutex`, because anything called while it is held (a race hook, a metric, a log sink that reads state) can commit a change on the same thread, and a non-reentrant lock would deadlock on itself.

## Deviations from the plan

Seven, all decided inside the units and recorded here rather than left to a reader's diff.
Three are budget overruns and four are behaviour.

### Three modules over their line budgets

The plan sizes `src/daemon/state/{mod.rs,snapshot.rs,revision.rs}` at under 600 lines; P3a-U4 landed 940.
It sizes `src/daemon/state/events.rs` plus the session's outbound queue at under 550; P3a-U6 landed 546 in `events.rs` and rewrote 243 lines of `server.rs` on top of it.
It sizes P3a-U8 at under 450; `src/daemon/operations.rs` is 816, and `mailbox.rs` adds 120 beside it.

The overrun is documentation, not logic: the three files carry module-level prose explaining the push algorithm step by step, the reentrancy of the bootstrap gate, and every invalid transition of the operation state machine, because those are the decisions a later reader will otherwise have to reconstruct from tests.
The budgets were a proxy for "do not build more than the unit asks", and none of the three serves a method or a shape the unit did not ask for.
Splitting them further would put one contract across two files, which the plan warns against elsewhere.

### `account.list` and `state.bootstrap` disagree about an account's state, on purpose

`account.list` reports `ready` or `blocked` from a read-only probe of the store file, and never `opening`.
A bootstrap snapshot reports `opening` for every account, always, because this build starts no account runtimes.

They answer different questions, "can I read this account's store on disk" and "has this account's runtime come up", and the plan fixes both spellings without saying they would collide in one build.
The protocol document now says so in both sections rather than in neither.

### An unrecognised `client.type` is `-32602`

The plan lists `cli`, `tui` and `gui` and does not say what a fourth value does.
The handshake refuses it with `-32602` rather than defaulting to `cli`, because the daemon logs the kind and later phases will branch on it, and a default is a wrong answer that never gets reported.

### Lifecycle events use a second per-connection queue

`EventQueue::drain` is pinned by the P3a-U3 contract to `(Revision, Change)`, and a lifecycle event reduces into no state, so it is not a `Change` and cannot ride that queue.
The registry therefore has its own per-connection queue sharing the endpoint's `attached` flag, and the connection loop merges the two drains by revision.
The alternative was a `Change::Lifecycle` variant that every match arm would have to ignore, which is a worse shape for a longer time.

### `Outbound::rebootstrap` discards queued lifecycle events

An overflow keeps lifecycle events, precisely because no snapshot carries them.
A second `state.bootstrap` clears the whole queue, lifecycle events included, so a progress report queued behind a resync is lost where the same report behind an overflow is kept.

Nothing in Phase 3a can observe it: a re-bootstrap follows a resync, and a client that resyncs re-reads `operation.status`, whose newest report survives the finish.
It is still the two rules disagreeing, and it is a follow-up in `BACKLOG.md`.

### `mailbox.list` opens the account's store twice

`count_all_emails` opens the store for the grouped totals and `unread_counts` opens it again to list the account's rows and count the ones without `\Seen`.
One open and one grouped query would do both, but the grouped count that exists is the TUI sidebar's and it does not carry the read flag, so using it meant either a second query or a new one.
A read-only method that opens a SQLite file twice is a cost and not a contract; the wire shape is unaffected and the fix is a follow-up.

### The revision stream is dense on the daemon's side and not on the wire

P3a-U3 pins a client tracker that treats any revision above `watermark + 1` as a gap.
P3a-U6 coalesces, and a merged entry takes the newer revision, so the delivered stream skips numbers whenever anything coalesces.

The two agree in this build, because Phase 3a registers no method that commits a change and the only producer is the burst hook, whose events name distinct resources on purpose.
The protocol document now states the delivery guarantee (strictly increasing, not dense) separately from the counter's (dense), and reconciling the tracker with it is a follow-up rather than a Phase 3a edit, since changing it would mean editing a T unit's file.

## Gate evidence

`docs/baselines/phase3a-gate-evidence.md` maps each of the four Phase 3a exit-gate lines to the test or command that proves it, with the commands as run and their results, plus the four T-unit compile-failure proofs.

All four pass as written.
1601 tests pass under the feature and 1354 without it, `mp --help` is byte-identical to the pre-daemon baseline in both builds, and clippy reports nothing in `src/daemon/` or `crates/`.

## Acceptance criteria

- Two clients observe ordered authoritative state. Met, 28 tests in `daemon_bootstrap` and 29 in `daemon_events`, including the five forced race boundaries and the stalled-reader case.
- Event overflow produces bounded recovery. Met, the overflow and `state.resync_required` cases in `daemon_events`, with the queue's byte cap asserted directly rather than inferred.
- A bootstrap taken before any account is ready converges by event without a second bootstrap. Met, the `opening`-account cases in `daemon_bootstrap`, driven by `MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS`.
- Nothing in this half changes the behaviour of a client that never sets the debug flag. Met, the plain suite is 1354 with every addition in a new file, and the help capture diffs empty in both builds.

## Files

- `src/daemon/{dispatch.rs,operations.rs}`, `src/daemon/state/{mod.rs,snapshot.rs,revision.rs,events.rs}`, `src/daemon/methods/{state.rs,mailbox.rs,mod.rs,account.rs,message.rs}`, `src/daemon/{server.rs,session.rs,lifecycle.rs,mod.rs}`
- `crates/mp-client/src/{state.rs,connection.rs,lib.rs}`, twelve new fixtures under `crates/mp-protocol/fixtures/`
- `tests/{daemon_dispatcher,daemon_bootstrap,daemon_events,daemon_operations}.rs`
- `docs/daemon-protocol.md`, `docs/daemon-operations.md`, `docs/baselines/phase3a-gate-evidence.md`, `docs/lessons-learned.md`
- `Cargo.toml`, `CHANGELOG.md`, `BACKLOG.md`
