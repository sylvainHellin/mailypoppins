---
id: 0004
title: Fix read/unread sync behaviour
type: bug
priority: now
status: open
created: 2026-05-01
---

Read status resets after each fetch. Bidirectional sync with the IMAP `\Seen` flag is not working correctly: emails marked read in another client revert to unread locally, or local read state gets clobbered on next sync.

## References

- [Handoff 2026-04-05](../../.claude/handoff/2026-04-05_23-34-46_read-unread-sync-debug.md)

## Acceptance

- Marking an email read in webmail and then quick-syncing keeps the local file in `read = true`.
- Marking a local email read via `m` propagates to the server and survives a sync round-trip.
- No regression on Graph accounts (`mark_read_graph` path).
