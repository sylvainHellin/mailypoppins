---
id: 0007
title: Flagging / starring
type: feature
priority: next
status: open
created: 2026-05-01
---

Flag important emails on the server, display a flag icon in the list, support filtering for flagged.

## Notes

- IMAP: use the `\Flagged` system flag.
- Graph: use the `flag.flagStatus` property on the message resource.
- TUI: add a key binding (e.g. `*`), add a flag column / icon to the email list.
- Persist flag state in frontmatter (`flagged: bool`) so local-only filtering works without a server roundtrip.
