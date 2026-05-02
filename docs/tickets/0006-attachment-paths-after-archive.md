---
id: 0006
title: Attachment paths break when source email is archived
type: bug
priority: next
status: open
created: 2026-05-01
---

`create_forward_draft` (and reply scaffolding) resolves attachment paths to absolute paths at draft creation time (`src/draft.rs:262-278`). If the source email is subsequently archived (moved from `inbox/` to `archive/`), the hardcoded paths in the draft frontmatter become stale and `send` fails with "Failed to read attachment".

## Approaches

1. **Relative paths with lookup.** Store attachment references relative to the email root (e.g. `tum/inbox/...`) and resolve at send time by searching across `inbox/`, `archive/`, etc. for the matching `_attachments/` directory.
2. **Stable attachment storage.** Copy or hardlink attachments into a dedicated `attachments/` directory (content-addressed or keyed by message-id) at sync time. Drafts reference the stable location, unaffected by inbox/archive moves.
3. **Send-time validation with auto-repair.** At send time, if an attachment path doesn't exist, search sibling mailbox directories for the same `_attachments/` folder and rewrite the path before sending.

Option 2 is the most robust (decouples attachment storage from mailbox layout). Option 3 is the cheapest fix and a reasonable interim.

## Acceptance

- Forwarding an inbox email, archiving the source, and then sending the draft succeeds.
- No regression for the common case where the source is still in inbox.
