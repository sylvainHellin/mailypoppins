# Design: TUI foundation + multi-view restructure

> Status: design, nothing implemented (2026-07-11). Implementer-facing.
> Inspiration: [docs/herdr-tui-inspiration.md] (phases A–E).
> Scout: [.agents/research/2026-07-11-tui-restructure-scout.md].
> Tickets: [#0032](../tickets/0032-tui-foundation-package.md) (foundation),
> [#0033](../tickets/0033-view-switcher-contacts-view.md) (switcher + contacts),
> [#0034](../tickets/0034-local-calendar-view.md) (calendar view).
> Subsumes: [#0019](../tickets/0019-configurable-keybindings.md).

## Why this doc

The scout brief is blunt about the starting point:
- **No view/screen abstraction exists.** `ui/mod.rs::view` and the `Focus`/key dispatch are
  hardcoded to the email domain. A multi-view switch re-shapes the render entry point, key
  dispatch, and `App` — it is not a localized change.
- **`App` is a flat ~90-field mail struct** (`src/tui/app/mod.rs`), mixing global and
  active-account-proxied mail fields.
- **Overlays are ~8 independent booleans/options** rendered in an `if` cascade and guarded
  individually in `handle_key`.
- **Keymap is a 3-copy problem:** `keys.rs`, `help_sections()` (`overlays.rs:484`), and the
  website key pages are hand-kept in sync.

The consequence for sequencing: **foundation before views.** Building three views on top of the
8-boolean overlay model and the 3-copy keymap multiplies existing debt by three. Do the herdr
foundation refactor first, then add views on the clean base.

## Decision summary (settled — do not reopen)

- **D9 — Foundation first.** Land the herdr **B + C + cheap-A** package before any multi-view
  work:
  - single **`Mode` overlay enum** replacing the ~8 booleans (one `match` in render, one
    dispatcher in input);
  - **keymap-as-data**: an `Action` enum + a single `KEYMAP` source feeding key dispatch, the
    help overlay, the hint bar, and (via a dump) the website — **subsumes [#0019]**;
  - **leader-key / prefix support** so the user gets nvim-like leader combos to relieve keymap
    crowding (generalizes today's invisible `g` leader);
  - **mode/hint bar**, **dim-background under modals**, and **shared modal primitives**.
- **D10 — Then multi-view.** A top-level `enum View { Mail, Contacts, Calendar }`, herdr-style
  left column (mailboxes top, view switcher bottom-left). Ship **Mail + Contacts first**;
  Calendar later. View switching is via the **leader key**; **digits 1–9 stay = mailbox jump**.
  Carve mail state into a `MailView` sub-struct mirroring the existing `AccountState` proxy.
- **D11 — Contacts view v1.** Read-only list + fuzzy search + detail pane over the existing
  local contacts index (`src/contacts/`), a compose-to-contact action, **and** send-contact-
  as-vCard: export a contact to `.vcf` and attach it to a new email or an existing draft.
- **D12 — Calendar view v1.** **Local-first**: events are files on disk derived from iMIP
  traffic (invites received / accepted / sent); agenda/list rendering; **blind to
  Outlook-created events** until the Graph backend lands (approved option c). The Graph backend
  is a separate future ticket gated on admin approval.

## Stage 1 — Foundation package (#0032) [D9]

Delivers no new user-visible feature by itself; it is the base every view + the invite UI
builds on. Corresponds to herdr phases **B + C** plus the cheap **A** items.

1. **Overlay `Mode` enum.** Replace `confirm_dialog`, `show_help`, `show_activity_overlay`,
   `show_search_overlay`, `compose_wizard`, `attachment_picker`, `dir_picker`,
   `mailbox_picker`, `persistent_error` with one `enum Mode { Normal, Help, Confirm(..),
   Compose(..), … }`. One `match` in `view`, one dispatcher at the top of `handle_key`. Exactly
   one overlay at a time by construction.
2. **Shared modal/panel primitives** — a `src/tui/ui/widgets.rs` with the herdr five: panel
   shell, centered modal shell, stack-areas, action-button chip, `panel_contrast_fg`. Migrate
   existing overlays onto them.
3. **Keymap-as-data** — an `Action` enum + a static `KEYMAP: &[(KeyPattern, Action, Group,
   &str)]`. `keys.rs` matches through it; `help_sections()` renders from it (kills copy #2);
   add `mp --dump-keys` so the website table (copy #3) is generated, not hand-kept.
4. **Leader / prefix support** — generalize the `g` leader into first-class prefix chords in
   the keymap model, so future leader combos (e.g. `<leader>` + view digit) are data, not
   special-cased. Feeds the hint bar directly.
5. **Mode/hint bar** — `render_hint_bar` switching on the current `Mode` (and pending prefix),
   showing the next valid keystrokes from `KEYMAP`. Makes leaders visible.
6. **dim_background under modals** — add `Modifier::DIM` to the buffer before drawing any
   overlay.

**Unblocks:** the invite RSVP overlay (calendar-invites design, D3) plugs into the `Mode` enum
and `KEYMAP` cleanly; Stage 2's view switcher becomes a keymap `Action` + one render slot
rather than a new boolean; #0019 is delivered as a byproduct of keymap-as-data.

**Non-goals here:** mouse / `compute_view` split (herdr D), scrollbars, toasts, unicode-width
fixes, TestBackend tests — those are herdr phases D/E, tracked separately if/when wanted, not
required for views.

## Stage 2 — View switcher + Contacts view (#0033) [D10, D11]

Built on the Stage-1 base.

1. **`enum View { Mail, Contacts, Calendar }`** threaded through `view()` and key dispatch.
   `Mail` and `Contacts` are implemented now; `Calendar` renders a "coming soon" placeholder
   until Stage 3.
2. **State carve-out** — extract the mail-specific fields of `App` into a `MailView`
   sub-struct, mirroring the existing `AccountState` proxy pattern (scout brief's recommended
   disciplined path). `App` keeps global state + the active `View` + per-view sub-structs.
3. **herdr-style left column** — mailboxes on top (existing sidebar), a **view switcher
   region bottom-left** (mail | contacts | calendar chips), reusing the activity-log bottom
   slot pattern. Switch via the **leader key**; **digits 1–9 remain mailbox jump** (no
   collision).
4. **Contacts view (v1, D11):**
   - read-only **list + fuzzy search + detail pane** over `src/contacts/`
     (`build_index_for_account` / `search` already exist and back the compose wizard);
   - **compose-to-contact** action → opens the existing compose wizard seeded with the
     selected contact;
   - **send-contact-as-vCard** → export the selected `Contact` to a `.vcf` and attach it to a
     **new email or an existing draft** (reuses the attachment plumbing; needs a small
     `Contact → vCard` serializer in the library).

**Unblocks:** a real screen abstraction, so the Calendar view (Stage 3) is "add a `View` arm +
content", not another restructure. Contacts ships value immediately with near-zero new backend.

## Stage 3 — Local Calendar view (#0034) [D12]

Depends on Stage 2 (the `View` abstraction) and on the iMIP receive/parse work
([calendar-invites design](calendar-invites.md), #0027) that produces the on-disk events.

- **Local-first data source.** Events are the files already produced by iMIP traffic —
  received invites, accepted/sent invites — read from frontmatter + sidecar `.ics`. No new
  fetch backend in v1.
- **Rendering.** Agenda / list view of upcoming events with the same event-card detail used in
  the mail preview (title, time, location, organizer, RSVP, attendee statuses).
- **Explicitly blind** to Outlook-created events (events that never came through email). That
  gap is closed only by the Graph sync backend
  ([#0036](../tickets/0036-graph-sync-backend.md)), gated on admin approval
  ([#0035](../tickets/0035-graph-admin-approval.md)) — **approved option c**.

## Sequencing at a glance

| Stage | Ticket | Depends on | Ships | Unblocks |
|---|---|---|---|---|
| 1 Foundation | [#0032] | — | overlay enum, keymap-as-data (=#0019), leaders, hint bar, dim bg, widgets | invite RSVP overlay; view switcher as data |
| 2 Switcher + Contacts | [#0033] | #0032 | `View` enum, `MailView` carve-out, switcher, Contacts view + vCard send | Calendar view as a `View` arm |
| 3 Calendar view | [#0034] | #0033, #0027 | local-first agenda/list from iMIP files | (Graph backend later) |

Priorities: **invites next**, **restructure after**, **calendar view later** — reflected in the
ticket `priority` fields and BACKLOG.md ordering.

## Risks

- **Restructure is not localized** (high). Do it on top of Stage 1's `Mode` enum + keymap, or
  the `if`-cascade/3-copy debt multiplies across three views.
- **`MailView` carve-out touches many call sites** (medium). The `AccountState` proxy is the
  precedent; keep the proxy field names stable to minimize churn.
- **Keybinding collisions** (low–medium). Leader owns view switching; digits stay mailbox jump;
  the invite key must be chosen against the `KEYMAP` table, not ad hoc.
- **Calendar view has no independent backend** (by design). It renders only what iMIP produced;
  do not let it grow a fetch path — that is the Graph ticket's job.
