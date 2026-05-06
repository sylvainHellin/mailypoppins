---
id: 0024
title: sorting email inbox
type: bug
priority: now
status: done
created: 2026-05-06
---

The emails are sorted per date (most recent first), which is good. however, it seems to sort only by day as the latest, not by hours/minutes/etc. so emails from a given day are sorted randomly.

## Root cause

`resolve_date` in `src/tui/app/types.rs` parsed each email's RFC2822
`Date:` header (or RFC3339 `sent_at`) into a `chrono::DateTime<FixedOffset>`,
then formatted the sort key as `"%Y-%m-%dT%H:%M:%S"` directly off that
`DateTime` -- which renders the time in the **sender's local offset**, not
UTC.

So two emails delivered on the same UTC day from different timezones
ordered by sender-wallclock, e.g.:

- `Mon, 06 May 2026 10:00:00 +0200` (= 08:00 UTC) -> sort key `2026-05-06T10:00:00`
- `Mon, 06 May 2026 09:30:00 +0000` (= 09:30 UTC, **later instant**) -> sort key `2026-05-06T09:30:00`

Lexicographic compare puts the earlier email above the later one.
Visible effect: same-day ordering looks random.

## Fix

`with_timezone(&chrono::Utc)` before formatting the sort key for both
the RFC2822 and RFC3339 branches. The `%Y-%m-%dT%H:%M:%SZ` (naive UTC)
branch was already correct. Display string stays in sender-local time
(unchanged) so the calendar date the user sees still matches what other
email clients show.

Regression test: `test_resolve_date_sort_normalises_timezone` in
`src/tui/app/types.rs` exercises both the RFC2822 and RFC3339 branches
with two same-day timestamps in different offsets and asserts the later
UTC instant sorts higher.

Landed together with a tightened assertion on
`test_resolve_date_rfc3339_sent_at` (was a `starts_with` smoke check;
now asserts the exact UTC-normalised sort key).

Files changed:
- `src/tui/app/types.rs` -- fix + tests
- `CHANGELOG.md` -- Unreleased / Fixed entry
- `BACKLOG.md` -- index entry removed
