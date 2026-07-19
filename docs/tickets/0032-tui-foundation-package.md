---
id: 0032
title: TUI foundation package - overlay enum, keymap-as-data, leaders, hint bar
type: refactor
priority: next
status: done
created: 2026-07-11
---

Shipped in 5a00d05 + 3c1a057.

Herdr-style foundation refactor that must land **before** any multi-view work. Design: [tui-restructure-views](../plans/tui-restructure-views.md) (Stage 1, D9). Inspiration: [herdr-tui-inspiration](../herdr-tui-inspiration.md) phases B + C + cheap A. Scout: `.agents/research/2026-07-11-tui-restructure-scout.md`.

**Subsumes [#0019](0019-configurable-keybindings.md)**: the `Action` enum + `KEYMAP` table is exactly the groundwork that ticket needs; close-reference #0019 to this one.

No new user-visible feature by itself — it is the base the invite RSVP overlay and the view switcher build on. Doing three views on top of the current 8-boolean overlays + 3-copy keymap multiplies existing debt.

## Scope

1. **Overlay `Mode` enum** replacing the ~8 booleans/options (`confirm_dialog`, `show_help`, `show_activity_overlay`, `show_search_overlay`, `compose_wizard`, `attachment_picker`, `dir_picker`, `mailbox_picker`, `persistent_error`): one `match` in `view`, one dispatcher in `handle_key`, exactly one overlay at a time.
2. **Shared modal/panel primitives** — `src/tui/ui/widgets.rs` with the herdr five (panel shell, centered modal shell, stack-areas, action-button chip, `panel_contrast_fg`); migrate existing overlays onto them.
3. **Keymap-as-data** — an `Action` enum + a static `KEYMAP: &[(KeyPattern, Action, Group, &str)]`. `keys.rs` matches through it; `help_sections()` renders from it (kills copy #2); add `mp --dump-keys` to generate the website table (copy #3). Delivers #0019.
4. **Leader / prefix support** — generalize the invisible `g` leader into first-class prefix chords in the keymap model (nvim-like leader combos to relieve keymap crowding).
5. **Mode/hint bar** — `render_hint_bar` switching on current `Mode` + pending prefix, showing the next valid keystrokes from `KEYMAP`.
6. **dim_background under modals** — `Modifier::DIM` on the buffer before drawing any overlay.

Non-goals (herdr D/E, separate if wanted): mouse/`compute_view` split, scrollbars, toasts, unicode-width fixes, TestBackend tests.

## Acceptance criteria

- Exactly one overlay renderable at a time (enum-enforced); no `if`-cascade of overlay booleans remains.
- Help overlay + hint bar + website key table all derive from `KEYMAP` (no hand-kept duplicates).
- `g` and any new leader combos show their continuation in the hint bar.
- Modals dim the background and share one shell.
- `#0019` marked done/dropped-as-subsumed, removed from BACKLOG.

## Unblocks

- Invite RSVP overlay ([#0029](0029-imip-rsvp-reply.md)) plugs into `Mode` + `KEYMAP`.
- View switcher ([#0033](0033-view-switcher-contacts-view.md)) becomes a keymap `Action` + render slot.

## Closing note

Shipped in two units: 5a00d05 (overlay `Mode` enum, shared modal primitives, dim background) and 3c1a057 (KEYMAP-as-data with runtime Normal-mode dispatch, `g`-leader as first-class prefix data, hint bar, `mp dump-keys` + generated website key table). All acceptance criteria met: exactly one overlay at a time is enum-enforced; help overlay, hint bar, and website key table all derive from `KEYMAP`; leader continuations show in the hint bar; modals share one shell and dim the background. [#0019](0019-configurable-keybindings.md) dropped as subsumed.
