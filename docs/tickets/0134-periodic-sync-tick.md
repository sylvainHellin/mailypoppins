---
id: 0134
title: Schedule a quick tick per account when the watcher has not caused one
type: feature
priority: now
status: done
created: 2026-09-28
---

## Problem

Nothing scheduled a sync tick.
Ticks came from a manual `sync.quick`/`sync.full`, a client's startup auto-fetch, and the watcher (`src/daemon/runtime/watcher.rs`), which hears one mailbox: an IMAP IDLE round on INBOX, or the Graph inbox's id set every 60 s.
Everything else stayed stale until a manual sync or new INBOX mail: other mailboxes changed by another client, read/flag changes the IDLE round does not report, a queued op or an outbox Sent copy waiting out its backoff, a body fetch the deadline cut short, an IDLE connection the server dropped silently, and an account with no client open.
This was the `BACKLOG.md` item "The daemon's watcher is a watch and not a scheduler".

## Fix

A per-account scheduler, `src/daemon/runtime/scheduler.rs`, spawned in `config::start_account` beside the watcher and the drainer for a ready runtime and bound to it the same way: held weakly, stopped by its retirement or its drop, never looked up by name.

- The runtime records when each tick that ran a body finished (`AccountRuntime::completed_ticks`, a `watch` of `Instant` set by the runner in `tick`, initialised to the runtime's start). Joiners, refused ticks and drains do not set it.
- The scheduler waits until that moment plus the interval, and restarts the wait whenever the record moves, so a tick of any origin (manual, watcher, startup fetch, scheduled) pushes the next scheduled one back. It also remembers when it last fired and never fires sooner than an interval after that, so a tick that was refused and moved nothing cannot loop.
- The fire is `TickKind::Quick` through `publish::tick_and_publish`, which is `server::commit_tick` (the `sync.completed`) plus the mailbox counts that moved. The watcher now calls the same function, so the two publish identically; it joins a running tick and waits for a drain through the runtime's turn by `AccountRuntime::tick`'s own rules.
- `src/daemon/runtime/publish.rs` holds the count publication the drainer had (`publish_changed_counts`, compared against the canonical state); the watcher's private count map is gone.
- No scheduler for a local-only account, a blocked runtime, `sync_interval_secs = 0`, or a Graph account (the daemon tick has no Graph backend and would report a failed sync every interval).
- No stagger: each runtime's clock starts when it comes up, and ticks on different runtimes share no lock.

### The config key

`[accounts.imap] sync_interval_secs`, default `900`, `0` disables, values under `MIN_SYNC_INTERVAL_SECS` (60) raised to it by `ImapSettings::sync_interval`.
Per account and in `[accounts.imap]`, beside `body_fetch_deadline_secs`, the other knob of the same daemon tick, because the scheduler is per account runtime and only IMAP accounts get one.
It is part of `config.get`'s effective account, so `config.reload` sees a change as an update of that account and restarts its runtime, and the new scheduler comes up with the new interval through the existing swap path; `config.add_account` writes it like the other numeric `imap` fields.
`config.get` reports the configured value, not the floored one, as it does for the other `imap` clamps.

## Tests

- `src/daemon/runtime/scheduler.rs`: a tick fires after the interval and again an interval after it; a tick of another origin pushes the scheduled one back; a drain does not; `0`, local-only and Graph schedule nothing; a retired runtime stops its scheduler and fires nothing; a blocked runtime gets no scheduler and a forced one runs no body and does not loop; a dropped runtime stops its scheduler.
- `crates/mp-core/src/config.rs`: the default with and without an `[accounts.imap]` table, `0` disables, the 60 s floor.
- `src/daemon/config.rs`: a changed `sync_interval_secs` makes the account `updated` in a reload.
- `tests/daemon_config.rs`: `sync_interval_secs` is one of the `imap` keys `config.get` reports, with its default.
- `src/daemon/runtime/publish.rs`: only moved counts are committed (moved from the drainer).

## Left open

- Graph accounts: see `BACKLOG.md`.
- The first scheduled tick is an interval after the daemon starts; a daemon started with no client and no watcher event syncs nothing for fifteen minutes.
