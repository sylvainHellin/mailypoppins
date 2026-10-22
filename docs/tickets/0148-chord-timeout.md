---
id: 0148
title: Desktop key prefixes wait for the second key by default, with the wait set in Settings
type: feature
priority: next
status: open
created: 2026-10-22
---

## Problem

In the desktop app a prefix such as `c` expires after 1.2 s (`PREFIX_TIMEOUT_MS` in `clients/desktop/src/keymap/useKeymap.ts`).
One timer hides the which-key popup and drops the prefix, so a second key pressed late runs its flat action: `c`, a pause, then `a` archives instead of replying to all.
Sylvain asked (2026-10-22) for the wait to be tunable in Settings and for a longer default.
The TUI never times out a prefix and is not affected.

## Plan

[docs/plans/chord-timeout.md](../plans/chord-timeout.md) holds the design, six owner decisions with a recommendation each, three work units, the docs to change and the risks.
It recommends waiting for the second key by default, as the TUI does, with 5 s, 3 s and 1.5 s as the other choices, stored in the desktop's localStorage `Prefs`.

Research:

- [.agents/research/2026-10-22-chord-timeout-scouting.md](../../.agents/research/2026-10-22-chord-timeout-scouting.md)
- [.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md)
- [.agents/research/2026-10-22-account-picker-scouting.md](../../.agents/research/2026-10-22-account-picker-scouting.md)

## Acceptance criteria, in short

- By default, `c`, a pause of any length, then `a` replies to all, and the popup stays up until the second key.
- Settings has a "Prefix keys" row with four choices that applies at once and survives a restart; a timed choice ends the popup and the prefix together.
- Escape over a pending prefix cancels it and does nothing else; a click cancels it.
- The activity log's `gg` and the TUI are unchanged.
- `pnpm test` and `pnpm build` pass.

## Status

Implementation has not started.
The worker starts from the plan's first work unit, Unit 1 (the wait in `Prefs` and the keymap), proceeding on the plan's recommendations unless Sylvain has overruled one in this ticket.
