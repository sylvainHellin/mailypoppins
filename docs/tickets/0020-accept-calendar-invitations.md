---
id: 0020
title: Accept / tentative / decline calendar invitations
type: feature
priority: later
status: open
created: 2026-05-01
---

Calendar invitations (RFC 5546 / iCalendar / `text/calendar` MIME parts) arrive regularly and other clients let the user accept / tentative / reject them. Currently mailypoppins shows them as opaque attachments.

## Notes

- Detect `text/calendar` MIME parts (or `.ics` attachments).
- Parse with an iCalendar crate (e.g. `icalendar`).
- Show event details (title, time, location, organiser) inline in the preview pane.
- Add bindings to send a `REPLY` iCalendar message back: accept (`A` already taken -- pick something else), tentative, decline.
- For Microsoft 365 / Graph accounts there is a richer Graph endpoint (`/me/events/{id}/accept`) -- prefer it when available.
