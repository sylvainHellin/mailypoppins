---
id: 0116
title: One outbox drain per account, and an APPEND attempt that is committed before it runs
type: bug
priority: now
status: done
created: 2026-09-09
---

Fifth ticket of [docs/plans/sync-skip-list.md](../plans/sync-skip-list.md), and it replaces that plan's premise for #0116.

## The earlier diagnosis was wrong

The plan said the duplicate Sent copies came from `config::server_saves_to_sent` failing to recognise the TUM Exchange host, so `save_to_sent = "auto"` resolved to "the client appends" while the server filed its own copy too, and the `UID SEARCH HEADER MESSAGE-ID` guard lost the race against the server's filing.

The log falsifies that.
Six messages sent on 2026-09-08 have 6, 5, 4, 3, 2 and 1 copies on the server, and `grep -c "[outbox] appended <mid>"` over that day's log returns 6, 5, 4, 3, 2 and 1 for the same six Message-IDs.
Every copy on the server was appended by mp.
The server filed none of them, so an extra copy from Exchange is not what is being counted, and extending the hostname set would have changed nothing.

The hostname observation is true on its own terms: `postout.lrz.de` and `xmail.mwn.de` are not recognised and `auto` does resolve to append for this account.
That is the account correctly saving its own Sent copy, which is what a generic IMAP account is supposed to do.

## Problem

`outbox::drain` was re-entrant per account.

Every send drains the account's outbox to file its own Sent copy (`send::DurableSend::settle`), and so do the startup resume and the sync tick.
A drain reads the open rows once, at the top, and works through them one APPEND at a time, so a drain that starts while another is in flight picks up every row the first has not finished yet and APPENDs it a second time.
A slow APPEND widens the window: a 19.7 MB message took 5.8 seconds on this account.

That is exactly what the log shows.
Six drains started between 15:00:58.771 and 15:01:03.899 and all six were still running at 15:01:04, and the appends land at 15:01:04.633, :07.624, :08.658, :09.821, :10.460 and :11.074.
The oldest row was in all six snapshots and was appended six times; the row created for the second send was in five of them; and so on down to one.

The `attempts > 0` gate on the dedup search could not catch any of it.
Each of the six drains was making the row's *first* attempt, `attempts` was still 0 in all of them, and `attempts` was only incremented after an attempt came back failed.
`grep -c "skipping the APPEND"` over the whole day is 0.

## Approach

Two changes, one exclusion mechanism and one durability marker.

### The drain runs under the engine lock

`outbox::drain_guarded` wraps the pass in the per-account advisory lock `src/engine_lock.rs` already provides and `pending_ops::drain_account` already uses.
`send::drain_account`, the single live entry point, calls it; `outbox::drain` keeps its signature and stays the lock-free state machine the tests drive.
A drain that is refused the lock returns `Ok(None)`, logs one line and does nothing, which is the contract the lock module states: the holder's engine drains what everyone enqueues.

`flock` is what makes the lock safe to hold across a 5.8-second APPEND.
The kernel releases it when the holding fd closes, on exit or on a `kill -9`, so a dead holder leaves no lease to expire and nothing to reap, and the next process takes the lock immediately.

The refused drain would otherwise leave its row for the next tick, so the holder sweeps again while the last pass completed something, up to `MAX_SWEEPS` (4).
A row a refused peer left behind, and a row enqueued while the holder was inside a slow APPEND, are both picked up by the holder instead of waiting.

### The attempt is committed before it goes out

`begin_append_attempt` increments `attempts` and moves `updated` immediately before the APPEND, in one statement, and the row is skipped when it is no longer in `sent_pending_append`.
`record_append` no longer increments on failure, so the counter still moves exactly once per attempt.

This is the APPEND's half of `submission_started_at`.
A process killed inside its APPEND leaves `attempts > 0` behind, so the drain that reclaims the row runs the dedup search and finds the copy the dead attempt filed, instead of appending a second one.
Without it the killed attempt is indistinguishable from a row that has never been touched.

The search stays off the common path: the decision reads the counter as it stood *before* the increment, so a row nobody has ever appended still costs one APPEND and no search.
That is sound on its own terms rather than by luck: the Message-ID is minted per build and `enqueue` admits one open row per draft, so a first attempt cannot find its own message already filed.

### Why a per-account lock and not a per-row claim

A per-row claim is more granular and it was the other candidate.
It needs a claim marker with an owner and a lease, since a durable SQLite queue has no daemon to reclaim what a dead process held, and the lease length is the problem.
The lease has to be longer than the slowest APPEND or a live attempt is reclaimed under itself, and the reclaiming drain cannot tell "the copy is not there" from "the copy is still being uploaded", so it appends and the duplicate is back.
A 19.7 MB message took 5.8 seconds here; a 40 MB attachment on a hotel link is not bounded by anything this code knows.
`flock` needs no lease at all, because the kernel already knows whether the holder is alive.

The lock also costs nothing in throughput that matters.
One drain is one `ImapSentMailbox` session appending rows one at a time, so per-row granularity buys parallelism only by opening a second session against the same account, which is not something to want.

The price is that the outbox drain and the `pending_ops` drain now exclude each other per account: a send while the mutation queue is draining files its Sent copy at the next tick instead of immediately.
The SMTP submission is unaffected, since the drain is only the APPEND.

### Deadlock

None is possible.
`EngineLock::try_acquire` is `LOCK_EX | LOCK_NB` and never blocks, so the worst case is a refused drain that skips, and no call path takes the lock twice: `lib_do_sync` and `mp sync` call `resume_outbox` and `pending_ops::resume_account` in sequence, `pending_ops`' executor is the IMAP or Graph backend and never drains the outbox, and `pending_ops::run_and_settle` (the CLI's synchronous path) takes no lock at all.

## Acceptance criteria

- Two drains racing for one account APPEND each row at most once.
- A drain refused the lock does nothing and opens no session, and the row it would have filed is filed by the lock holder rather than deferred.
- A drain killed inside its APPEND releases the lock, leaves the row in `sent_pending_append` and reclaimable, and the drain that reclaims it deduplicates instead of appending a second copy.
- A row's first attempt costs no dedup search.
- `cargo test` passes.

## Scope

This stops new duplicates.
It does not remove the copies already on the server, and nothing here deletes anything from any mail server.

Unimplemented, and only a suggestion: the copies are removable by hand from any IMAP client by sorting the Sent mailbox by subject and deleting all but one of each identical message.
They are byte-identical, so which one survives does not matter.
Everything before 2026-09-09 is the affected window, and after #0112 they show up honestly in mp's own Sent list, one row per server copy.
An `mp` command to do it is deliberately not built: deleting mail on a server is a decision that belongs to the user.

## Files

- `src/outbox.rs`: `drain_guarded`, `drain_guarded_at`, `MAX_SWEEPS`, `begin_append_attempt`, the `attempted_before` argument to `append_once`, and `record_append` no longer incrementing.
- `src/send.rs`: `drain_account` goes through `drain_guarded`.
- `tests/outbox_integration.rs`: `Ledger` and `SharedSent`, the shared Sent mailbox two racing drains append into, and the three tests above.
