# Design: key prefixes in the desktop app wait for the second key, as in the TUI

> Status: implemented (2026-10-22).
> The ticket is #0148.
> Sylvain decided on 2026-10-22 that the desktop behaves as the TUI: no timeout, no setting (decision log, same date).
> Citations name a symbol and its file; line numbers move with the code.
> Research: [chord timeout scouting](../../.agents/research/2026-10-22-chord-timeout-scouting.md), [prior art](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md).

## Goal

Sylvain's report (2026-10-22): in the desktop app, after a prefix key such as `c` the which-key overlay "disappears after one or two seconds" and the chord is cancelled, so a late `a` archives the message instead of replying to all (`ca`).
The TUI never had a timeout and is the model.

## Design

- A pending prefix (`g`, `f`, `c`, `t`, `s`, Space) waits for its second key with no timer, in Mail and in every full-pane view.
- There is no setting: no Settings row, no `Prefs` field, no localStorage key.
- The which-key popup (`PrefixPopup`, `src/components/palette/PrefixPopup.tsx`) appears at once and follows the pending prefix exactly, so it stays until the prefix ends.
- In `useKeymap` (`src/keymap/useKeymap.ts`), `arm` stores and publishes `{ key }` (`src/keymap/pendingPrefix.ts`), and `take` hands back and disarms whatever is armed; there is no `PREFIX_TIMEOUT_MS` and no age check.
- The next key consumes the prefix, resolved or not.
- Escape, as any key the family does not bind, only cancels it: the `if (pending)` branch swallows it, so it neither clears the selection nor leaves a search or a view.
- A click anywhere drops it, through a capture-phase `pointerdown` listener on the window beside `blur` and `focusin`, so a chord abandoned for the pointer never resolves with a later key.
- Leaving the window and moving the focus into a field or the embedded editor drop it, as before.
- An open dialog hides the popup; the first key inside the dialog drops the prefix.
- The activity log's own `gg` (`ActivityLogDialog`) waits for its second `g` the same way, with no timer.
- The TUI does not change: `pending_prefix` on `App` has no timer, and strict prefix mode in `resolve` makes Esc clear the chord and fire nothing.

## Rejected

- A configurable wait with a 5 s default and presets in Settings (the earlier decision of 2026-10-22): it keeps two behaviours between the clients and a setting nobody changes twice.
- A short delay before the popup shows: the TUI shows it at once.

## Tests

- `src/components/palette/prefixPopup.test.tsx`: after `c` and a minute of fake time the popup still lists the compose rows and `a` replies to all without archiving; Escape over `c` hides the popup, keeps the selection and opens nothing; a `pointerdown` drops the prefix so the next key is flat; the next key, the window's blur and a focused field still drop it.

## Docs

- `clients/desktop/docs/shell.md`, Keys: the prefix waits for the second key, Escape cancels it, a click drops it.
- `website/src/pages/faq.astro`: the which-key list waits for the second key and Esc cancels it.
- `README.md`, `docs/parity-matrix.md`, `website/src/data/tui-keys.json` and the in-app help: unchanged, since no binding changes and none mentions a time.
