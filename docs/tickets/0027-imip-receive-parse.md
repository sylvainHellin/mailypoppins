---
id: 0027
title: iMIP receive - detect, save .ics, parse VEVENT into frontmatter
type: feature
priority: next
status: open
created: 2026-07-11
---

Receiving side of iMIP calendar invitations. Design: [calendar-invites](../plans/calendar-invites.md) (D2, D5, D6). Splits the receiving half out of [#0020](0020-accept-calendar-invitations.md).

Today `text/calendar` parts are silently dropped by `src/parse.rs` (`extract_body_parts` keeps only text/plain+text/html; `is_attachment_part` needs a filename) — neither body nor attachment. This ticket makes invites first-class on disk.

## Scope

- Add a `text/calendar` branch to the part traversal in `src/parse.rs`.
- **Save the raw `.ics` as a sidecar** colocated with the email's attachments (source of truth for `UID`/`SEQUENCE`; mind [#0006](0006-attachment-paths-after-archive.md) path handling on archive/move).
- Parse `VCALENDAR`/`VEVENT` with the `icalendar` crate: `METHOD`, `UID`, `SUMMARY`, `DTSTART`/`DTEND`(or `DURATION`), `LOCATION`, `ORGANIZER`, `ATTENDEE[]` + `PARTSTAT`, `SEQUENCE`, `RRULE` (rendered to a short human string — display only, D6).
- Populate the `event:` frontmatter block on the email `.md` (schema in the design doc).
- Branch by `METHOD`: `REQUEST` → invite card; `REPLY` → hand to reconciliation ([#0030](0030-imip-reply-reconciliation.md)), not rendered as an invite; `CANCEL` → tag `method: CANCEL`, no action (v1, [#0031](0031-imip-cancel-update.md)).
- Best-effort parsing: a malformed `.ics` still saves the sidecar and marks the email "invite (unparsed)" rather than failing sync.

## Acceptance criteria

- A received Outlook/Gmail/Apple invite is saved as a sidecar `.ics` and its `event:` frontmatter is populated.
- Fixtures cover: inline `REQUEST`, `.ics`-attachment `REQUEST`, `REPLY`, malformed `.ics`, and a plain multipart email (no calendar) — the last must be unchanged (no attachment/body regression).
- Recurrence shows a human-readable summary; no per-occurrence handling.

## Related

- Adds the `icalendar` dependency (shared with [#0028](0028-imip-send-invite.md)).
- TUI card/badge rendering: [#0029](0029-imip-rsvp-reply.md) (RSVP) and calendar-invites design D3.
