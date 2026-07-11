---
id: 0018
title: Quick-move between mailboxes
type: feature
priority: later
status: done
created: 2026-05-01
---

Move an email back from archive to inbox (or to any configured mailbox) without leaving the list view.

## Notes

- Trigger via `M` (capital, distinct from `m` which is read/unread toggle).
- Show a small overlay listing target mailboxes; numeric or fuzzy selection.
- Implementation: IMAP `MOVE` (or `COPY` + `EXPUNGE` fallback for servers without MOVE); Graph `move` action. Local file is moved to match.
- Works on the selection if any, otherwise the cursor.

## Shipped (2026-07-11)

- `M` in the email list opens a fuzzy picker (case-insensitive
  subsequence match) of destination mailboxes; Enter moves, Esc cancels.
- Reuses the archive machinery: `move_email_on_server` /
  `move_email_locally` (IMAP, COPY+EXPUNGE with rollback on failure) and
  `move_email_graph` (Graph `/move`) generalize the archive helpers,
  which are now wrappers.
- Batch: works on the selection if any, otherwise the cursor.
- Same-mailbox moves are impossible (the active mailbox is excluded from
  candidates); local-only mailboxes (Drafts) are excluded as
  destinations and refuse as source.
