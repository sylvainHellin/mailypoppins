---
id: 0009
title: Desktop notifications for new mail
type: feature
priority: next
status: open
created: 2026-05-01
---

The IDLE watcher detects new mail but gives no notification outside the TUI. Integrate `notify-rust` (cross-platform) or shell out to `terminal-notifier` on macOS.

## Notes

- Trigger only when the TUI is *not* focused on the receiving account, or when the TUI is backgrounded entirely.
- Make it opt-in via config: `[notifications] enabled = true`.
- Group multiple arrivals into a single notification within a short window to avoid spam.
