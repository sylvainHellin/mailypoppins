---
id: 0001
title: Auto-fetch on TUI startup
type: feature
priority: now
status: done
created: 2026-05-01
---

The TUI currently spawns IMAP IDLE watchers at launch but does **not** trigger an initial sync. IDLE only catches mail arriving while the watcher is running, so anything that landed between TUI sessions is invisible until the user manually presses `s`.

Verified in [.claude/research/2026-04-29_sync-timing-analysis.md](../../.claude/research/2026-04-29_sync-timing-analysis.md): no `sync_mailboxes` calls between `App::new()` and the first user keypress.

## Fix

In `tui/mod.rs::run_loop`, after spawning watchers, push `Action::Fetch` for each account with IMAP/Graph config.

Best landed *after* [#0002 persist-mailbox-states](0002-persist-mailbox-states.md) -- with the cache populated, the startup fetch is a warm-state quick sync (~1.2 s on TUM, ~0.8 s on Proton) instead of the cold 15 s. Without the cache, startup auto-fetch would be a regression.

## Polish

- Kick the fetch from a background thread so it doesn't block the first frame.
- Stagger per-account fetches so a slow reconcile (if ever forced) doesn't delay a fast account's sync.

## Resolution (2026-05-03)

- New `Action::FetchAccount(usize)` variant in `src/tui/app/types.rs`.
  Mirrors `Action::Fetch` but reads configs from `app.accounts[idx]`
  instead of the active-account snapshots, and **does not** gate on
  `bg_count > 0` (multiple per-account fetches must run concurrently).
- Handler in `src/tui/actions.rs` spawns the same
  `lib_do_sync` / `lib_do_sync_graph` background thread as
  `Action::Fetch`, sending a `BgResult::Fetch { account_index, ... }`
  whose existing `bg.rs` arm already routes correctly for active vs.
  background accounts and persists the returned `mailbox_states`.
- Trigger lives in `src/tui/bg.rs` -- the `BgResult::IndexReady`
  handler pushes `Action::FetchAccount(account_index)` after the
  account's `message_id_index` is populated, gated on
  `imap_config.is_some() || graph_config.is_some()` so local-only
  accounts are skipped. `push_action` (not `push_action_dedup`) is
  used because different `FetchAccount(idx)` values share a
  discriminant and would otherwise be falsely deduplicated.
- Stagger falls out of the design for free: each account's fetch fires
  the moment its own index lands, so a slow reconcile on TUM does not
  hold up Proton.
- No new fields on `App` or `AccountState`; no new `BgResult`
  variants. 295 unit tests + integration tests pass.
