---
description: "mailypoppins-herdr-inspiration"
id: "mailypoppins-herdr-inspiration"
---

# What mailypoppins can learn from herdr

> Status: analysis only, nothing implemented yet (2026-07-11). Next step when picked up:
> turn the phases at the bottom into `docs/tickets/` entries and index them in BACKLOG.md.

Read-only analysis of `/home/sylvain/code/_ref/herdr` (ratatui 0.30, ~9.2k lines in `src/ui/`)
vs `src/tui/` in mailypoppins (ratatui 0.29, ~9.9k lines). No code was changed.

The two codebases are the same size and both are Elm-ish (state → render), so the gap you
perceive is not "they had more budget" — it's a handful of deliberate patterns. Ranked below
by impact-per-effort.

---

## 1. The polish you're seeing is mostly ONE widget: the mode-badge hint bar

**herdr:** `src/ui/menus.rs`. Every modal mode (PREFIX, NAVIGATE, COPY, RESIZE) paints a
single-line bar at the bottom of the content area:

```
 NAVIGATE  esc back  J/K ws  ⇥ pane  g navigator  c new tab  % split│  " split─  x close …
```

- ` NAVIGATE ` badge = bold + **accent background** + contrast fg → instantly tells you what mode you're in.
- Then alternating spans: key in bold accent, label in dim (`overlay0`).
- The bar always shows *the next valid keystrokes for the current mode* — this is the
  "hints for next keystroke" you like.
- Labels are derived from the live keybind config, never hardcoded twice.

**mailypoppins today:** the status bar shows `? help` and nothing else contextual. Notably,
`g_pending` (the `g` leader in `src/tui/app/keys.rs`, ~15 call sites) is *completely
invisible* — you press `g` and the UI gives zero feedback about what `g` can be followed by.

**Port:** a `render_hint_bar(app, frame, area)` that switches on current state:
- `g` pending → ` GOTO  i inbox  s sent  d drafts  a archive  g top  G bottom  esc cancel`
- search focus → ` SEARCH  enter apply  \ body search  esc clear`
- selection active → ` 3 SELECTED  a archive  d delete  r mark read  esc clear`
- compose wizard → field-specific hints
- idle → account tabs + `? help` (what you have now)

This is one file, no architecture change, and it's 70% of the herdr feel.

## 2. Model overlays as ONE enum, not eight booleans

**mailypoppins:** `ui/mod.rs` ends with a cascade of independent flags:
`confirm_dialog`, `show_help`, `show_activity_overlay`, `show_search_overlay`,
`compose_wizard`, `attachment_picker`, `dir_picker`, `persistent_error` — 8 possibly
overlapping overlays, z-order implicit in render order, and every key handler must
check the right combination.

**herdr:** a single `Mode` enum (`Terminal | Prefix | Copy | Navigate | Settings |
ConfirmClose | KeybindHelp | ContextMenu | …`) with one `match` in `render()`
(`src/ui.rs` bottom) and one dispatcher in input. Exactly one overlay at a time,
by construction; the hint bar (item 1) also just matches on it.

This refactor makes items 1, 4 and 5 much easier and removes a class of
"two overlays at once" bugs.

## 3. Shared modal/panel primitives instead of per-overlay layout code

**herdr:** `src/ui/widgets.rs` (278 lines) is the whole design system:
- `render_panel_shell(frame, area, border_color, bg)` → Clear + bordered block, returns inner Rect
- `centered_popup_rect` / `render_modal_shell(w, h)` → centered popup with clamping
- `modal_stack_areas(inner, header_h, footer_h, actions_h, gap)` → header/content/footer/actions Rects
- `render_modal_header`, `render_modal_description`, `render_modal_choice_list`
- `render_action_button(frame, rect, Some("esc"), "close", style)` → the little
  ` esc close ` chips you see in herdr modals — they double as **mouse hit targets**
  (`action_button_row_rects` returns the rects; input hit-tests them)
- `panel_contrast_fg(palette)` → readable fg on accent bg (mailypoppins hardcodes
  `fg(theme.bg)` on accent, which breaks on `Color::Reset` terminals)

**mailypoppins:** every overlay (`overlays.rs`, `search.rs`, `activity.rs`, `compose.rs`)
re-implements the `Flex::Center` horizontal+vertical layout dance and its own border/title
style. Result: subtly inconsistent margins, borders, and footer styles between overlays —
which reads as "less polished" even when each one is individually fine.

**Port:** extract a `src/tui/ui/widgets.rs` with the herdr five (panel shell, modal shell,
stack areas, action button, contrast fg), then migrate overlays one by one. Pure refactor,
snapshot-testable.

## 4. `dim_background` under modals — 10 lines, huge perceived polish

`herdr src/ui.rs::dim_background`: before drawing any modal, walk the buffer and add
`Modifier::DIM` to every cell. The popup then visually floats. mailypoppins only `Clear`s
the popup rect, so the help/confirm dialogs sit flat on a fully-bright background.
This is probably the single cheapest "wow" item on this list.

## 5. Mouse support: the hit-area architecture (biggest functional gap)

mailypoppins has **zero** mouse handling: `src/tui/event.rs` drops `Event::Mouse`,
`init_terminal` never enables `EnableMouseCapture`. herdr is fully mouse-operable while
staying keyboard-first, and the way they did it is the part worth copying:

**Pattern (herdr `src/ui.rs::compute_view` + `app/input/mouse.rs`):**
1. Split the frame pipeline into `compute_view(&mut app, area)` → `render(&app, frame)`.
   `compute_view` computes *all* geometry once per frame and stores every clickable
   Rect in `app.view: ViewState` — `tab_hit_areas`, `new_tab_hit_area`, scroll-button
   rects, `workspace_card_areas`, `toast_hit_area`, per-modal button rects.
2. Render reads `app.view` (render becomes `&App`, no `&mut` — mailypoppins' `view()`
   currently takes `&mut App`).
3. The mouse handler is then trivial: `rect_contains(rect, col, row)` against the stored
   rects → semantic `MouseAction` enum (`FocusTab`, `ConfirmCloseAccept`, …) that reuses
   the exact same action paths as the keyboard.

For an email TUI the payoff list is short and sweet: wheel-scroll list & preview,
click a row to select / double-click to open, click a sidebar mailbox, click account
tabs in the status bar, click `[y]/[n]` in confirm dialogs, click ` esc close ` chips.
Keyboard stays primary; the mouse just hits the same actions.

(Also note herdr's chrome is *adaptive*: the `+` new-tab button and `<`/`>` scroll
buttons only render `if app.mouse_capture` — chrome appears only when it's usable.)

## 6. Toasts instead of (only) status-bar messages

herdr `src/ui/status.rs`: a small bordered toast in a configurable corner, with a
kind-colored dot (`●` red=attention, blue=finished), title + dim context line,
overlap-resolution against other notifications, and a click target ("open the thing
that finished"). mailypoppins funnels everything through the one-line status bar,
where "Sent." fights with the mailbox counters and disappears on the next tick.

**Port:** toast for async events — *new mail arrived (click to open)*, *send complete*,
*sync failed* — keep the status bar for state, not events. `persistent_error` would also
look better as a red-dot toast than the current full overlay.

## 7. Keybinds as data = help, hints, and docs from one source of truth

herdr `src/ui/keybind_help.rs` builds the help overlay *from the runtime keybind config*
(groups, computed key-column alignment, `unset` markers, collapsing `alt+1..alt+9` runs).
mailypoppins' `help_sections()` in `overlays.rs` is a hardcoded parallel list that must be
kept in sync with `keys.rs` by hand — and your AGENTS.md says the website key-reference
pages are *also* hand-derived from it (third copy!).

**Port:** an `Action` enum + a static `KEYMAP: &[(KeyPattern, Action, Group, &str)]` table;
`keys.rs` matches through it, help renders from it, the hint bar (item 1) filters it by
mode, and a tiny `email --dump-keys` could even generate the website table. This also
paves the way for user-configurable keys later, like herdr's.

Small herdr touches worth stealing while in there: help modal has a clickable
` esc close ` chip in the header, a scrollbar, and a footer hint line
(`scroll wheel ↑↓ · jump pgup/pgdn · close esc/enter`). Your help filter (typing to
filter entries) is actually *nicer* than herdr's — keep it.

## 8. Scrollbars

herdr `src/ui/scrollbar.rs`: `ScrollMetrics` (offset-from-bottom, max, viewport) +
`should_show_scrollbar` (auto-hide) + row↔offset mapping used for click & drag.
mailypoppins has no scrollbars anywhere — the email list, preview, help and activity
overlays give no position feedback. Add the thin `▐` track/thumb to list + preview;
it becomes a mouse drag target for free once item 5 lands.

## 9. Correctness details that read as "polish"

- **Unicode width:** herdr measures everything with `display_width_u16` (unicode-width).
  mailypoppins' `truncate()` in `ui/util.rs` counts *chars*, so CJK/emoji subjects and
  contact names will misalign the list columns and truncate wrongly. Real bug; herdr even
  has regression tests for CJK tab labels.
- **TestBackend UI tests:** herdr renders into `ratatui::backend::TestBackend` and asserts
  buffer rows, styles, and hit-area geometry (dozens of tests in `ui.rs`/`tabs.rs`).
  mailypoppins' TUI render layer has no tests. Start with: status-bar composition,
  list truncation, overlay geometry clamping on tiny terminals — then hit-areas when
  mouse lands. These are fast, offline, and fit your <0.5s `cargo test` budget.
- **Tiny-terminal clamps:** every herdr renderer bails or clamps (`if inner.height < 6 …`).
  Several mailypoppins overlays assume the popup fits.
- **Spinner:** both use the same braille spinner; herdr centralizes it
  (`spinner_frame(tick)`, one place controls speed). Trivial cleanup.

## 10. Not worth porting

- Herdr's sidebar cards / workspace drag-reorder / drop indicators — wrong shape for a
  mail sidebar with ~6 static mailboxes.
- Mobile layout mode — mailypoppins' existing width breakpoints (40/80 cols) already
  cover this proportionally to need.
- Their `Palette` vs your `Theme`: yours is arguably *better* (semantic slot names vs
  herdr's leaked catppuccin names like `mauve`). Just add `panel_bg` (a darker
  overlay-background distinct from `bg`) and the `panel_contrast_fg` helper.

---

## Suggested sequencing (when you're ready to touch the repo)

| Phase | Items | Effort | What you get |
|---|---|---|---|
| A | 4 (dim bg), 1 (hint bar incl. `g`-pending), 9-spinner | small | Immediate herdr feel |
| B | 3 (widget primitives), 2 (overlay enum) | medium refactor | Consistency + foundation |
| C | 7 (keymap as data → help/hints/docs) | medium | Kills the 3-copy sync problem |
| D | 5 (compute_view/ViewState + mouse), 8 (scrollbars) | largest | Full mouse support, herdr-style |
| E | 6 (toasts), 9 (unicode-width, TestBackend tests) | small each | Polish + safety net |

Each phase ships independently; A alone is probably a weekend-sized change and closes
most of the perceived gap. When picking this up, convert each phase into a ticket via the
`ticket` fish function and index it in [BACKLOG.md](../BACKLOG.md).

Note: phase C (keymap as data) subsumes/enables existing ticket
[#0019 Configurable keybindings](tickets/0019-configurable-keybindings.md) — the `Action`
enum + keymap table is exactly the groundwork that ticket needs.
