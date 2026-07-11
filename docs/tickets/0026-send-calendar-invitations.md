---
id: 0026
title: Send calendar invitations (iMIP REQUEST)
type: feature
priority: next
status: dropped
created: 2026-06-24
---

> **Refined 2026-07-11 → superseded by [#0028 iMIP send](0028-imip-send-invite.md)** against
> [docs/plans/calendar-invites.md](../plans/calendar-invites.md). The Graph note below is **not**
> v1: Graph is a future backend ([#0035](0035-graph-admin-approval.md)/[#0036](0036-graph-sync-backend.md)).
> Exchange organizer-matching risk moves to the Graph-future section of the design doc.
> Original notes retained below for record.

Sending an `.ics` file as a normal attachment does **not** produce a real calendar invitation: the recipient sees a file to open, with no Accept / Tentative / Decline buttons and no RSVP tracking. A proper invitation is an iMIP message (RFC 6047 / RFC 5546): the email body carries an inline `text/calendar; method=REQUEST` MIME part, which is what Outlook / Apple Mail / Gmail render as an actionable invite. Today the only workaround is to compose the invite from a real calendar client (Outlook), so mailypoppins cannot be used to send invitations.

This is the sending counterpart to [#0020 accept/decline invitations](0020-accept-calendar-invitations.md) (receiving side).

## Proposed approach

- Build a VEVENT with an iCalendar crate (`icalendar`), `METHOD:REQUEST`.
  - Required props: `UID`, `DTSTAMP`, `DTSTART`/`DTEND` (or `DURATION`), `ORGANIZER` (= sender), `ATTENDEE` per invitee with `RSVP=TRUE`, `SUMMARY`. Optional: `LOCATION`, `DESCRIPTION`, `SEQUENCE` (0 for new).
  - `ORGANIZER` address must match the sending account's primary address or servers (esp. Exchange) drop the invite.
- Emit the message as `multipart/alternative` with:
  1. `text/plain` (and/or `text/html`) human-readable body, and
  2. `text/calendar; method=REQUEST; charset=UTF-8` carrying the VEVENT.
  - Also attach the same payload as an `application/ics` file part for clients that prefer the attachment form (common belt-and-suspenders pattern).
- CLI surface: extend the send path, e.g. `email send ... --invite` with flags for `--start`, `--end`/`--duration`, `--location`, `--summary`, attendees taken from the existing `--to`/`--cc`.
- For Microsoft 365 / Graph accounts, prefer the Graph events endpoint (create event with attendees) over hand-rolled iMIP when available, mirroring the #0020 note.

## Acceptance criteria

- An invite sent from the CLI to an Outlook and a Gmail recipient renders with working Accept / Tentative / Decline.
- The organiser receives RSVP replies that map back to the event `UID`.
- Cancellations (`METHOD:CANCEL`, bumped `SEQUENCE`) and updates are out of scope for the first cut; note as follow-up.

## Motivation

Real use case (2026-06): inviting LOC members to the LOC Day required falling back to Outlook because a CLI-attached `.ics` produced no RSVP behaviour.
