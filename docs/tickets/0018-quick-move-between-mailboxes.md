---
id: 0018
title: Quick-move between mailboxes
type: feature
priority: later
status: open
created: 2026-05-01
---

Move an email back from archive to inbox (or to any configured mailbox) without leaving the list view.

## Notes

- Trigger via `M` (capital, distinct from `m` which is read/unread toggle).
- Show a small overlay listing target mailboxes; numeric or fuzzy selection.
- Implementation: IMAP `MOVE` (or `COPY` + `EXPUNGE` fallback for servers without MOVE); Graph `move` action. Local file is moved to match.
- Works on the selection if any, otherwise the cursor.
