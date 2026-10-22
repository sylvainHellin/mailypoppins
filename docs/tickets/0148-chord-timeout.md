---
id: 0148
title: Prefix keys in the desktop app wait for the second key, as in the TUI
type: feature
priority: next
status: done
created: 2026-10-22
---

## Problem

In the desktop app a prefix such as `c` expired after 1.2 s (`PREFIX_TIMEOUT_MS` in `clients/desktop/src/keymap/useKeymap.ts`).
One timer hid the which-key popup and dropped the prefix, so a second key pressed late ran its flat action: `c`, a pause, then `a` archived instead of replying to all.
The TUI never times out a prefix and was not affected.

## Decision

Sylvain decided on 2026-10-22 that the desktop behaves as the TUI: a pending prefix never times out, and there is no setting for it.
This replaces the earlier decision of the same day for a 5 s default configurable in Settings.
[docs/plans/chord-timeout.md](../plans/chord-timeout.md) holds the design.

Research:

- [.agents/research/2026-10-22-chord-timeout-scouting.md](../../.agents/research/2026-10-22-chord-timeout-scouting.md)
- [.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md)
- [.agents/research/2026-10-22-account-picker-scouting.md](../../.agents/research/2026-10-22-account-picker-scouting.md)

## Acceptance criteria, in short

- A pending prefix in the desktop never times out: `c`, a pause of any length, then `a` replies to all.
- The which-key popup appears at once and stays until the prefix ends.
- Escape over a pending prefix cancels it and does nothing else; a click, leaving the window or focusing a field cancels it.
- Settings has no row for it and nothing is stored.
- The activity log's `gg` waits for its second `g` the same way; the TUI is unchanged.
- `pnpm test` and `pnpm build` pass.

## Status

Done: `PREFIX_TIMEOUT_MS` and its timer are gone from `useKeymap.ts` and `ActivityLogDialog.tsx`, a `pointerdown` drops a pending prefix, and `prefixPopup.test.tsx` pins the no-timeout, Escape and click cases.
