---
id: 0017
title: Jump-to-date in mailbox list
type: feature
priority: later
status: open
created: 2026-05-01
---

Quickly jump to a specific date range in large mailboxes. Useful for archives with thousands of emails where `j`/`k` and `gg`/`G` are not enough.

## Notes

- Trigger via a key (e.g. `D`).
- Date input UI: small inline prompt; accept `YYYY-MM-DD`, `last week`, `2 months ago`, etc.
- Implementation: binary-search the list (already date-sorted) and move the cursor; do not filter.
