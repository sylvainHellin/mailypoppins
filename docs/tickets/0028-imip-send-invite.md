---
id: 0028
title: iMIP send - REQUEST invite MIME + CLI
type: feature
priority: next
status: done
created: 2026-07-11
---

Shipped in daf9629.

Sending side of iMIP calendar invitations. Design: [calendar-invites](../plans/calendar-invites.md) (D1, D7). Refines [#0026](0026-send-calendar-invitations.md) (send half).

A real invitation is an iMIP message, not an attached `.ics`: the recipient must see Accept / Tentative / Decline. v1 transport is iMIP MIME over SMTP for **all** accounts (no Graph — see [#0035](0035-graph-admin-approval.md)/[#0036](0036-graph-sync-backend.md)). Universal recipient compatibility (Gmail/Outlook/Apple render `method=REQUEST`) is the requirement.

## Scope

- New MIME branch in `src/send.rs` (today: fixed `alternative(plain, html)` wrapped in `mixed()`):

  ```
  multipart/mixed
  ├── multipart/alternative           ← top-level method=REQUEST applies here
  │   ├── text/plain                   (human body)
  │   ├── text/html                    (optional)
  │   └── text/calendar; method=REQUEST; charset=UTF-8   ← the contract
  └── application/ics; name="invite.ics"   ← optional hardening, same bytes
  ```

  The inline `text/calendar` part is the contract (Outlook + Gmail act on it). The `application/ics` attachment is **optional hardening, not a requirement**: it helps long-tail clients (manual `.ics` import, clients that ignore inline parts, gateway MIME mangling; Gmail sends both), but some older Outlook versions render it as a confusing duplicate. **Ship it in v1, and drop it if live testing shows duplicate rendering.**

- Build the `VEVENT` with `icalendar`, `METHOD:REQUEST`, `SEQUENCE:0`. Required: `UID` (generate + persist), `DTSTAMP`, `DTSTART`/`DTEND`-or-`DURATION`, `ORGANIZER` (= sending account primary address), `ATTENDEE` per recipient with `RSVP=TRUE`, `SUMMARY`. Optional: `LOCATION`, `DESCRIPTION`.
- **Validate `ORGANIZER == account primary address`** before send (Exchange drops mismatches).
- Persist the sent invite locally (frontmatter + sidecar `.ics` in the Sent copy) so REPLY reconciliation ([#0030](0030-imip-reply-reconciliation.md)) has a match target.
- CLI: `mp send --invite --to <addr> [--cc <addr>] --start <rfc3339> (--end <rfc3339> | --duration <dur>) --summary <text> [--location <text>] [--description <text>]`. Attendees from `--to`/`--cc`.
- TUI wraps the library send function (no subprocess).

## Acceptance criteria

- An invite sent from the CLI to an Outlook and a Gmail recipient renders with working Accept / Tentative / Decline (ticket 0026 acceptance).
- The sent invite is stored locally with `event:` frontmatter + sidecar `.ics`.
- `mp --help`, in-TUI `?` overlay, and the website command pages are updated.

## Related

- Recurring-invite creation is v2 (design D6). Cancellations/updates: [#0031](0031-imip-cancel-update.md).
