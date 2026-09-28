---
id: 0133
title: Drain the mutation queue after a queued mutation, without waiting for a tick
type: bug
priority: now
status: done
created: 2026-09-28
---

## Problem

The TUI's five mutations (archive, delete, set_read, set_flag, move) reach the daemon with `settle: false` (`crates/mp-tui/src/commands.rs`).
`mutate` in `src/daemon/methods/message.rs` commits the row change and a `pending_ops` row in one transaction and answers without touching the server.
The queue was drained only inside a sync tick (`run_tick_with_drains`: drain, sync, drain; the outbox before the mutation queue).
Nothing scheduled a tick after a local mutation: ticks came only from a manual `sync.quick`/`sync.full`, the TUI's startup auto-fetch, or the watcher (`src/daemon/runtime/watcher.rs`) seeing INBOX change on the server, and a queued archive has not touched the server, so the watcher never fired for it.
The user's changes stayed local until a manual sync or unrelated new mail.

## Fix

A per-account drainer, `src/daemon/runtime/drainer.rs`, spawned beside the watcher for a ready runtime with a server and bound to that runtime the same way: held weakly, stopped by its retirement or its drop.

- A successful `settle: false` mutation calls `AccountRuntime::request_drain`, which bumps a `watch` counter and returns; the RPC answer does not wait.
- The drainer runs `AccountRuntime::drain` once the requests have been quiet for `DRAIN_DEBOUNCE` (1.5 s, a constant). The debounce trails, so a thousand-row selection is one drain.
- `AccountRuntime::drain` runs the tick's outbox hook and then its mutation hook (`crate::send::resume_outbox`, then `crate::pending_ops::resume_account`, off-thread), entered as `Phase::DrainOutbox` and `Phase::DrainMutations`, and no sync.
- A tick's run and a drain hold one async mutex, the runtime's turn, so they never run side by side and never write the shared `TickReport` at once. A drain requested during a tick waits for it and then drains, which catches an op queued after the tick's tail read the queue. `retire` waits the turn out within its bound, and a drain or a tick that gets the turn after retirement refuses.
- A blocked runtime gets no drainer and refuses `drain`; a retired one refuses too. A drain the in-process engine gate refuses drains nothing and loses nothing: the row stays queued for the next tick.
- A drain that rolled ops back commits `Change::MutationsRolledBack`, published as the new lifecycle kind `mutations.rolled_back` `{account, failed}` (`mp_protocol::events::MutationsRolledBack`). The TUI shows the tick's rollback wording as a warning and reloads the account's rows. A `sync.completed` was rejected for this because it moves the `last_sync` ledger and the TUI's sync-health mark, and a drain ran no sync.
- Every drain that ran commits one `MailboxCounts` per mailbox whose store counts differ from the canonical state's (`CanonicalState::mailbox_counts`), so the sidebar converges.

The CLI path (`settle: true`, `run_and_settle`) is unchanged.

## Tests

- `src/daemon/runtime/drainer.rs`: a burst is one drain after the last request; one request drains after the quiet period and not before; a request made before the drainer subscribed still drains; a retired runtime drains nothing and its drainer stops; a blocked runtime gets no drainer and drains nothing; a drain requested during a tick waits for it and then runs; only moved counts are committed; a rollback is published and a clean drain is not, and neither touches `last_sync`.
- `src/daemon/runtime/account.rs`: a drain enters its two slots, outbox first, runs no body, and reports its rollbacks.
- `src/tui_tests/events.rs`: a `mutations.rolled_back` warns and reloads without a sync verdict.
- `crates/mp-protocol/fixtures/notification.mutations_rolled_back.json`, checked by `tests/daemon_protocol_fixtures.rs`.

## Left open

- An op that fails with retries left stays queued behind its backoff; nothing re-arms the drainer for it, so it is retried by the next tick or by the drain a later mutation asks for. The periodic tick (the `BACKLOG.md` scheduler item) is what closes this.
- A drain refused by the in-process engine gate (a guarded pass holding it at that moment) is not retried either, for the same reason.
- The watcher's own `publish_counts` still compares against a private map rather than the canonical state.
