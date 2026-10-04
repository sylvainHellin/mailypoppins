---
id: 0140
title: All-day invitations and updates of a sent invitation in mp send --invite
type: feature
priority: now
status: done
created: 2026-10-04
---

## Problem

`mp send --invite` resolved `--start`/`--end` to UTC instants, so every invitation was timed.
Of 33 kindergarten dates sent to a Gmail calendar, 25 were closures, holidays or multi-day periods that went out as 00:00 to 00:00 timed events, which Google Calendar shows as timed blocks instead of in the all-day row.
Every invitation also went out with a fresh UID and `SEQUENCE:0`, so none of them could be corrected by re-sending.

## What shipped

- `InviteSpec` carries an `EventSpan` (`crates/mp-core/src/invite.rs`): `Timed { start, end }` as UTC instants, or `AllDay { first, last }` as calendar dates with `last` inclusive.
- `resolve_span` replaces `resolve_times`.
  A bare `YYYY-MM-DD` `--start` makes an all-day event; `--end` is then the last day, inclusive; a `--duration` of whole days (`P3D`, `3d`) counts days; neither means the one day.
  A bare date beside a time, a sub-day duration and a last day before the first are refused.
- The `VEVENT` carries `DTSTART;VALUE=DATE` and the RFC 5545 exclusive `DTEND;VALUE=DATE` (the day after the last one).
- The preview prints the dates of an all-day event (`2026-12-24 (all day)`, `2026-12-24  →  2027-01-08 (all day, 16 days)`); a timed preview is byte-identical to before.
- `mp send --invite --uid <UID> --sequence <N>` re-sends an invitation as an update: the same UID and a higher `SEQUENCE`, which is what calendar clients need to replace the event.
  `--sequence` requires `--uid`; without `--uid` a UID is minted as before.
- `send.invite` takes an optional `sequence` (a `u32`), refused after the Graph refusal so that one stays first.
  The CLI sends `sequence` only for an update, so a new invitation's call is the same as before on the wire.
- The desktop's New invitation form sends its start string to `send.invite` unchanged, so a bare date there is an all-day event too; its form and fixture still describe `end`/`duration` as required.

## Readers checked

- `calendar::parse_ics` reads a `VALUE=DATE` start as a midnight wall-clock (`2026-12-24T00:00:00`), which the agenda's `normalize_stamp` already treats as all-day and displays as the date alone.
- The RSVP reply echoes `DTSTART`/`DTEND` verbatim, `VALUE=DATE` included.
- `tests/imip_integration.rs` `an_all_day_update_replaces_the_timed_original_on_the_agenda` ingests a timed original and an all-day update with the same UID into `sent`: one agenda row, the update's, shown as `2026-12-24`.

## Left open

- The TUI event card and the desktop's `eventWhen` print the stored end as is, so an all-day range shows the exclusive day (`2027-01-09T00:00:00`) as its end, inbound and outbound alike.
- A sent copy built by `build_invite_mime_body` lands with two `invite.ics` blobs (the inline part and the `application/ics` part), so `list_invites` yields its row twice and `mp calendar rebuild` counts every sent invitation twice in `invites_seen`.
