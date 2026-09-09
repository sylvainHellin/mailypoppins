---
id: 0113
title: Bound the body fetch so one slow mailbox cannot hold a whole sync tick
type: perf
priority: now
status: done
created: 2026-09-09
---

Fourth ticket of the sync-skip-list group, after [#0112](0112-gate-message-id-rebind.md), [#0117](0117-rebind-gate-after-a-windowed-reset.md), [#0116](0116-one-outbox-drain-per-account.md) and [#0114](0114-drain-mutation-queue-at-tail.md).
Independent of #0115, which is the remaining one and is not cut yet.

See the Resolution section at the end for what shipped.

## Problem

Pass 2 of `fetch_new_raw_on_session` (`src/imap_client/fetch.rs`) asked for every new UID in one `UID FETCH <set> (UID BODY.PEEK[] FLAGS)`, and nothing bounded how long that took.

The measurement is #0112's: one tick spent 160 seconds downloading about 34 MB of new mail from the TUM `Sent Items` mailbox.
#0112 removed the reason those 34 MB were new on *every* tick, which is the bug; it does not make a first pass over a large mailbox quick, and it says nothing about a mailbox that is legitimately 34 MB behind after a week away.

A tick in flight blocks everything else.
The targets of one account run under `buffered(fetch_concurrency)`, but a tick is one future: the mutation queue drains at its ends (#0114), the next account waits, and the TUI's status line says `Syncing...` until the slowest mailbox in it is done.
So the cost of one slow mailbox is paid by every mailbox and every queued archive, delete or send behind it.

What made the single command the obvious shape is that it is also the correct one: fewer round trips, one response to collect.
What makes it the wrong one is that it has no point at which the pass can decide to stop.

## Resolution

Pass 2 is chunked and given a budget.

`body_chunks(&new_uids, BODY_CHUNK_SIZE)` cuts the new UIDs into `UID FETCH` commands of 20, and the cut is from the top: `new_uids` is ascending, the chunks are taken off the end, so the first command asks for the newest twenty and the short chunk is the oldest.
A pass that stops early therefore has the newest mail and owes only the backlog, which is the half nobody is waiting for.
`out` is sorted back into ascending UID order before it is returned, so ingest sees exactly the order it saw when the window was one command.

The budget is `Option<Duration>`, converted to an `Instant` at the top of the fetch, so it bounds that mailbox's whole pass and each mailbox in a parallel sync gets its own.
`stop_before_chunk(index, deadline, now)` is checked *between* chunks and never inside one, and the first chunk always goes out.
Both halves are load-bearing:

- wrapping a `UID FETCH` in `tokio::time::timeout` would abandon a command mid-stream and leave unread response bytes on a session that goes straight back into the pool (`pool::checkout` / `pooled.check`), so the next borrower would read this fetch's mail as the answer to its own command;
- a deadline that has already passed still buys one chunk, or a mailbox whose fetch starts late would download nothing on every pass and never converge.

A stop is `Ok`, not `Err`: an error would discard the chunks already collected, which is the opposite of the point.
The pass reports itself with a new `MailboxFetch.bodies_complete`, false only on a deadline stop.

Everything downstream of a short pass already existed and is reused rather than re-derived.
`arrival_coverage` over the partial download sets `download_incomplete` and hands back a `pending_arrival_mark`, `ingest::pass_may_prune` suspends every prune in the pass, and the cursor written at the end of it is the resume point the next pass starts from.
Two things are new:

- `modseq_to_record` takes `bodies_complete` as a third gate. A truncated pass looked at every flag in the window and at only part of the mail, so a `CHANGEDSINCE` resuming from its modseq would skip every later flag change on the messages it never downloaded, which is #0004 again.
- The engine reads the flag too (`src/sync/engine.rs`): it counts the target into `SyncResult.bodies_truncated`, folds `!bodies_complete` into the coverage pair, and filters the modseq. The backend already answers all three correctly; the engine says it in the one place that owns the cursor, so a backend that forgets cannot open the prune gate.

Ingest-failure counts are untouched by a truncated pass: `clear_ingest_failure` runs per ingested message, so a UID that was never asked for keeps whatever count it had.

### Who gets a deadline

`[accounts.imap] body_fetch_deadline_secs`, default 30, `0` for unbounded, clamped to [0, 600] at load, next to `fetch_concurrency`.

TUI ticks pass it (`lib_do_sync`), because a tick is the thing being protected.
`mp sync` passes `None` whatever the config says: it is the explicit recovery path, and the pass a user runs to make the store converge is the one pass that must not stop early.

A stop is logged once per target at warn:

```
[sync] 'Sent Items': body fetch stopped at the 30s deadline after 40/112 new message(s); the rest resume on the next pass
```

and the tick's status line carries `, N mailbox(es) stopped at the fetch deadline (resuming next sync)` in the same place the deferred-prune suffix goes, so a cut tick reads as progress rather than as a clean sync that quietly did less.

## Witness tests

The IMAP session is not mockable offline (no test server, and `async_imap`'s `Session` is not a trait here), so the loop's two decisions are pure functions with their own tests, and the consequences are pinned through the engine against the fake backend:

- `the_body_pass_is_chunked_newest_first` (`fetch.rs`): 45 new UIDs are three commands, the first holds the newest 20, the short one holds the oldest 5, and every UID is asked for exactly once.
- `the_deadline_stops_the_next_chunk_and_never_the_first` (`fetch.rs`): an already-spent deadline still buys chunk 0 and stops chunk 1; a live budget and `None` never stop.
- `only_a_pass_that_saw_the_whole_mailbox_may_record_a_modseq` (`fetch.rs`): the existing modseq test grew the `bodies_complete` case.
- `a_pass_cut_by_the_body_deadline_defers_the_prune_records_no_modseq_and_resumes` (`sync/engine.rs`): a scripted truncated fetch ingests what it collected, counts one truncated mailbox, defers the prune, records no modseq, and the next pass downloads the backlog, applies the prune it held, and records its resume point.

What no test covers is a real socket being handed a partial set, which is the same gap every other IMAP-side property here has.

## Acceptance criteria

- A mailbox with more new mail than the budget allows downloads part of it, ingests that part, and resumes from its cursor on the next pass.
- A truncated pass prunes nothing and records no modseq.
- `mp sync` is never truncated.
- The stop is visible: one warn line per target and a status-line suffix.
- `cargo test` passes.

## Files

- `src/imap_client/fetch.rs`: `BODY_CHUNK_SIZE`, `body_chunks`, `stop_before_chunk`, the chunked pass 2, the `body_budget` parameter, `modseq_to_record`'s third gate.
- `src/sync/mod.rs`: `MailboxFetch.bodies_complete`, `SyncResult.bodies_truncated`.
- `src/sync/engine.rs`: the coverage fold, the modseq filter, the counter, and the witness test.
- `src/imap_client/store_sync.rs`: `ImapBackend::with_body_budget`, `sync_mailboxes`'s `body_budget` parameter.
- `src/config.rs`: `body_fetch_deadline_secs` and `ImapConfig::body_fetch_deadline`.
- `src/tui/helpers.rs`: the tick passes the configured budget; the status-line suffix.
- `src/main.rs`: `mp sync` passes `None`.
