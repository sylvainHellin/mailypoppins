---
id: 0016
title: Open attachments for drafts (`o`)
type: feature
priority: later
status: open
created: 2026-05-01
---

`o` currently opens attachments only on inbox / archive emails. For drafts, `o` should open the files referenced in `attachments:` frontmatter so the user can verify what's about to be sent.

## Notes

- Reuse `parse::list_attachments()` and `parse::open_file_with_system()`.
- For drafts, attachments are absolute paths in frontmatter, not in a `_attachments/` directory next to the `.md`. The picker already handles multi-file selection.
- Available in List, Headers, Preview focus, same as the existing `o` flow.
