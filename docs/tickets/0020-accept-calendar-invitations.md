---
id: 0020
title: Accept / tentative / decline calendar invitations
type: feature
priority: next
status: dropped
created: 2026-05-01
---

> **Refined 2026-07-11 → superseded.** This umbrella ticket is split into implementation-ready
> tickets against [docs/plans/calendar-invites.md](../plans/calendar-invites.md):
> [#0027 receive/parse](0027-imip-receive-parse.md),
> [#0029 RSVP accept/tentative/decline + TUI](0029-imip-rsvp-reply.md),
> [#0030 REPLY reconciliation](0030-imip-reply-reconciliation.md).
> The Microsoft 365 / Graph note below is **not** v1: Graph is a future backend
> ([#0035](0035-graph-admin-approval.md)/[#0036](0036-graph-sync-backend.md)); v1 is iMIP-only.
> Original notes retained below for record.

Calendar invitations (RFC 5546 / iCalendar / `text/calendar` MIME parts) arrive regularly and other clients let the user accept / tentative / reject them. Currently mailypoppins shows them as opaque attachments.

## Notes

- Detect `text/calendar` MIME parts (or `.ics` attachments).
- Parse with an iCalendar crate (e.g. `icalendar`).
- Show event details (title, time, location, organiser) inline in the preview pane.
- Add bindings to send a `REPLY` iCalendar message back: accept (`A` already taken -- pick something else), tentative, decline.
- For Microsoft 365 / Graph accounts there is a richer Graph endpoint (`/me/events/{id}/accept`) -- prefer it when available.
