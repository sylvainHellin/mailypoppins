---
id: 0033
title: View switcher + Contacts view (with vCard send)
type: feature
priority: later
status: open
created: 2026-07-11
---

Multi-view TUI: a top-level `View` enum, herdr-style left column with a bottom-left view switcher, and the first non-mail view (Contacts). Design: [tui-restructure-views](../plans/tui-restructure-views.md) (Stage 2, D10, D11). Depends on [#0032](0032-tui-foundation-package.md).

Scout: no view/screen abstraction exists; `App` is a flat ~90-field mail struct (`.agents/research/2026-07-11-tui-restructure-scout.md`).

## Scope

1. **`enum View { Mail, Contacts, Calendar }`** threaded through `view()` and key dispatch. `Mail` + `Contacts` implemented; `Calendar` renders a placeholder until [#0034](0034-local-calendar-view.md).
2. **State carve-out** — extract mail-specific `App` fields into a `MailView` sub-struct, mirroring the existing `AccountState` proxy pattern. `App` keeps global state + active `View` + per-view sub-structs.
3. **herdr-style left column** — mailboxes on top; a **view switcher region bottom-left** (mail | contacts | calendar chips), reusing the activity-log bottom-slot pattern. Switch via the **leader key**; **digits 1–9 stay mailbox jump** (no collision).
4. **Contacts view (v1):**
   - read-only **list + fuzzy search + detail pane** over `src/contacts/` (`build_index_for_account` / `search` already exist);
   - **compose-to-contact** → opens the compose wizard seeded with the selected contact;
   - **send-contact-as-vCard** → serialize the selected `Contact` to a `.vcf` and attach it to a **new email or an existing draft** (reuses attachment plumbing; needs a small `Contact → vCard` serializer in the library).

## Acceptance criteria

- Leader-key switching between Mail and Contacts works; Calendar shows a placeholder.
- Digits 1–9 still jump mailboxes.
- Contacts view lists/searches contacts, opens a detail pane, can compose to a contact, and can export+attach a contact as `.vcf`.
- `mp --help`, `?` overlay, and website updated.

## Unblocks

- Calendar view ([#0034](0034-local-calendar-view.md)) becomes a `View` arm + content, not another restructure.
