---
id: 0004
title: Fix read/unread sync behaviour
type: bug
priority: now
status: done
created: 2026-05-01
---

Read status resets after each fetch. Bidirectional sync with the IMAP `\Seen` flag is not working correctly: emails marked read in another client revert to unread locally, or local read state gets clobbered on next sync.

## References

- [Handoff 2026-04-05](../../.claude/handoff/2026-04-05_23-34-46_read-unread-sync-debug.md)

## Acceptance

- Marking an email read in webmail and then quick-syncing keeps the local file in `read = true`.
- Marking a local email read via `m` propagates to the server and survives a sync round-trip.
- No regression on Graph accounts (`mark_read_graph` path).

## Resolution (2026-07-10)

Note: the referenced handoff file no longer exists on any machine or in git
history; root causes were re-derived from the current code. Three bugs:

1. **Snapshot clobber (IMAP + Graph).** Sync captured server flags in pass 1
   and applied them to local frontmatter later; local marks made during that
   window were reverted. Fixed with a snapshot cutoff (captured before the
   server read) in `sync_local_read_flags{,_with_index}`: files modified
   at-or-after `cutoff - 1s` are skipped, local state wins, next sync converges.
2. **Probe fast path shrank server->local coverage (IMAP).** The adaptive
   probe returned early when the 10 newest UIDs were known, so with no new
   mail, webmail read changes on older messages never synced. Removed; pass 1
   (headers+FLAGS) always covers the full window, pass 2 (bodies) still skips.
3. **Graph substring match.** `content.contains("read: true")` matched body
   text; Graph now shares the IMAP path's frontmatter-aware helper.

Regression tests in `tests/sync_integration.rs` (`test_read_flags_*`) and
`src/imap_client/sync.rs` cover both backends through the shared helper.
See `docs/lessons-learned.md` ("Bidirectional read-status sync") for the
design notes, including the deliberate choice of a staleness guard over a
3-way merge and the upgrade path if both-sides-changed conflicts ever matter.
