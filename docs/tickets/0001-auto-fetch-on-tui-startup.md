---
id: 0001
title: Auto-fetch on TUI startup
type: feature
priority: now
status: open
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
