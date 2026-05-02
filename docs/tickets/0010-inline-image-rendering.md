---
id: 0010
title: Inline image rendering in preview
type: feature
priority: later
status: open
created: 2026-05-01
---

Render inline images in the terminal preview pane using sixel, the iTerm2 inline-image protocol, or the Kitty graphics protocol.

## Notes

- Detect terminal capability at startup (env vars / responses to query sequences).
- Fall back to a `[image: filename]` placeholder when the terminal does not support graphics.
- Only render image attachments referenced inline (`Content-ID` matched in the HTML body); do not auto-render every attached image.
