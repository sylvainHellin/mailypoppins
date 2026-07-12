---
id: 0029
title: iMIP RSVP - accept/tentative/decline (REPLY) + TUI card/overlay
type: feature
priority: next
status: done
created: 2026-07-11
---

Shipped in e058e46.

Attendee-side RSVP to a received invite. Design: [calendar-invites](../plans/calendar-invites.md) (D3, D6, D7). Splits the accept/decline half out of [#0020](0020-accept-calendar-invitations.md). Depends on [#0027](0027-imip-receive-parse.md).

## Scope

- Library: build a `method=REPLY` VEVENT from the received sidecar `.ics` (source of truth for `UID`/`SEQUENCE`), set the account's `ATTENDEE` `PARTSTAT` to ACCEPTED / TENTATIVE / DECLINED, email it to the `ORGANIZER`, and update local `event.rsvp`. Accept/decline the **whole series only** (D6). No RSVP note text (v1).
- CLI: `mp invite accept|tentative|decline <email-path>`.
- TUI (D3):
  - invite **badge/glyph** in the email list row (distinct from the attachment paperclip);
  - **event summary card** at the top of the preview pane (title, time, location, organizer, own RSVP state, per-attendee statuses);
  - RSVP via a **single key** opening a small three-choice overlay (Accept / Tentative / Decline) — do **not** burn three top-level keys;
  - card footer surfaces the honest caveat: RSVP is not synced to the Exchange server calendar (no Graph in v1).
- TUI calls the same library functions as the CLI (no subprocess).

## Acceptance criteria

- Accepting/declining a received invite emails a valid `method=REPLY` to the organizer and flips the local `event.rsvp`.
- Preview card and list badge render for invites; RSVP overlay opens on one key.
- `mp --help`, `?` overlay, and website pages updated.

## Related

- RSVP key + overlay should register through the keymap table once the TUI foundation ([#0032](0032-tui-foundation-package.md)) lands; until then follow the `keys.rs`/`help_sections()`/website triple-update rule.
