---
id: 0009
title: Desktop notifications for new mail
type: feature
priority: next
status: done
created: 2026-05-01
---

The IDLE watcher detects new mail but gives no notification outside the TUI. Integrate `notify-rust` (cross-platform) or shell out to `terminal-notifier` on macOS.

## Notes

- Trigger only when the TUI is *not* focused on the receiving account, or when the TUI is backgrounded entirely.
- Make it opt-in via config: `[notifications] enabled = true`.
- Group multiple arrivals into a single notification within a short window to avoid spam.

## Resolution (2026-07-11)

Shipped with zero new dependencies: `src/notify.rs` shells out to
`osascript` (macOS) / `notify-send` (Linux), degrading silently when the
tool is missing. Opt-in via a top-level `notifications = true` key
(simpler than the `[notifications]` table suggested above -- consistent
with the top-level `theme` key; default off). Notifies only for
genuinely new *inbox* saves reported by a completed quick-sync
(`SyncResult::new_inbox_mail`, threaded through `BgResult::Fetch`);
read-flag updates, dedup, and reconciliation never notify. Grouping is
per sync run rather than per time window: one email shows
"sender: subject", several collapse into "N new emails" -- IDLE fires
one fetch per arrival burst, so this bounds notification volume without
a timer. The "only when unfocused/backgrounded" idea was dropped:
terminal apps cannot portably detect focus, and the OS notification
center already deduplicates noise. Sender/subject text is
attacker-controlled, so it is passed as separate argv entries (macOS:
AppleScript `on run argv`), control-stripped, and length-capped.
