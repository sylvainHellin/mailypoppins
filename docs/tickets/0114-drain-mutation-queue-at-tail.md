---
id: 0114
title: Drain the mutation queue at the tail of a sync tick as well as the head
type: bug
priority: now
status: done
created: 2026-09-09
---

Third ticket of the sync-skip-list group, after [#0112](0112-gate-message-id-rebind.md) and alongside [#0116](0116-one-outbox-drain-per-account.md).
Filed as an improvement and raised to a bug: the queue converging only on the next tick is the mirror lying to every other client for the length of a tick.

See the Resolution section at the end for what shipped.

## Problem

The mutation queue and the outbox drained once per tick, at the head.

`lib_do_sync` (`src/tui/helpers.rs`) resumed the outbox and then drained the pending ops, and only then called `sync_mailboxes`; `lib_do_sync_graph` did the same around the Graph read, and `mp sync` did it around both.
The reasoning was the read: draining first means the server has converged by the time the reconcile looks at the mailboxes those ops changed.
That reasoning is sound and it says nothing about the other end of the tick.

Everything the TUI queues while a tick is in flight therefore waits for the next tick.
Archiving a mail, deleting one, toggling a flag or sending a message all commit locally and enqueue the server half (#0039, #0037 item 5), and the enqueue is instant, so the mailbox looks converged on screen while the server has heard nothing.
The wait is the length of the tick, and a tick is not short: #0112 measured one at 160 seconds on the TUM account before the skip list converged, and a full sync over several mailboxes is in the same band.
For that whole window another client shows the mail unarchived and the message unsent, which is the "changes made in the TUI do not show up in other clients" symptom this ticket was filed under.

The failing ticks are the long ones, which is what makes the naive fix wrong.
`sync_mailboxes(...).inspect_err(...)?` returns from the function on any account-level failure, so a tail drain written after that line runs on exactly the ticks that did not need it and is skipped on the ticks that did.

## Resolution

`sync::tick::run_tick_with_drains` (`src/sync/tick.rs`) is the sequencing, and it is all of it:

```rust
let head_suffix = head().await;
let result = body().await;
let tail_suffix = tail().await;
(format!("{head_suffix}{tail_suffix}"), result)
```

The body's `anyhow::Result` is carried out rather than propagated inside, so the tail runs on both paths and the caller still `?`s on what it gets back.
The two drains return the status suffix they want appended to the tick's message, which is empty when they did nothing.

Each caller passes the same drain closure at both ends, so head and tail are the same work in the same order:

- `lib_do_sync` (IMAP, `src/tui/helpers.rs`) passes `drain_queues`: `send::resume_outbox`, then `drain_pending_ops`.
- `lib_do_sync_graph` passes `drain_pending_ops` alone, since Graph's outbox resubmit is a no-op and the head never called it.
- `sync_one_account` (`src/main.rs`) passes `drain_queues_cli`, which is the same pair with the CLI's two `↻` report lines and a `dry_run` short-circuit.

A tail that finds nothing queued is free and silent.
`pending_ops::resume_account` returns `Ok(None)` on a `COUNT` of zero before it opens a backend or takes the engine lock, `send::drain_account` returns early on an outbox with no open rows, and neither logs at info on that path, so the second call costs a store open, a `COUNT` on the mutation queue and an open-row scan plus a `COUNT` on the outbox, and writes nothing to the log or the status bar.
The status suffix is built by the same `drain_pending_ops` at both ends, so a tail that rolled ops back reads exactly as a head that did: `Synced: 3 new, 41 existing; 2 mutation(s) failed and were rolled back (see the log)`, and the `FAILED_OPS_MARKER` the completion handler matches on is unchanged.

Running the drain twice per tick is safe because it was always re-entrant across processes.
`pending_ops::drain_account` gates on `EngineLock::try_acquire` and yields `Ok(None)` when another process holds it, and the outbox drain does the same since #0116; both re-read the clock per sweep and retire what they complete, so a second call in one tick either finds new rows or finds nothing.

What this does not change is the reconcile.
The tail drain runs after the read, so the ops it retires are reflected on the server but not in the store's view of it until the next tick reads those mailboxes again.
That is the same one-tick lag the head drain has always had for the ops it retired, and it is invisible: the local half of every op committed when it was enqueued, so the screen is already showing the outcome.

### The CLI's head drain no longer aborts the run

`sync_one_account` used to write `pending_ops::resume_account(account_config).await?`, so a drain that could not open the store or resolve a backend failed the whole `mp sync` before it read anything.
Both ends now report the error on stderr (`⚠ mutations: drain failed: ...`) and on the log, and the sync proceeds.
A tail drain must not turn a sync that worked into a failed command, and giving the head a different rule would mean two drain functions to say one thing; the queue is durable and is retried on the next tick either way.

## Witness test

There is no test that can observe the placement through a live tick without a server, so the sequencing is the unit under test.
`src/sync/tick.rs` drives `run_tick_with_drains` with three closures that append their label to a `RefCell<Vec<&str>>`:

- `the_tail_drain_runs_after_a_body_that_succeeded`: order is `head, body, tail`, the suffixes concatenate, and the body's value comes back.
- `the_tail_drain_runs_after_a_body_that_failed_and_the_error_still_propagates`: the same order, and `run_tick_with_drains` hands back the `Err` verbatim. Deleting the tail call, or restoring the `?` inside the body's position, fails this one.
- `a_quiet_tail_adds_nothing_to_the_status_text`: two drains that return an empty suffix produce an empty suffix, which is the no-noise half of the acceptance criteria.

The production wrappers stay thin enough that what is left in them is the choice of drain, which is read directly.

## Acceptance criteria

- A mutation or a send queued while a tick is in flight is retired at the end of that tick, not the next one, on the IMAP path, the Graph path and `mp sync`.
- The tail drain runs when the sync fails as well as when it succeeds.
- A tail drain with nothing to do adds no log line and no status text.
- A tail drain that rolled mutations back is reported the same way the head's is.
- `cargo test` passes.

## Files

- `src/sync/tick.rs`: new, `run_tick_with_drains` and its three tests.
- `src/sync/mod.rs`: `pub mod tick;`.
- `src/tui/helpers.rs`: `drain_queues`, and both `lib_do_sync` and `lib_do_sync_graph` going through the helper.
- `src/main.rs`: `drain_queues_cli`, and `sync_one_account` going through the helper.
