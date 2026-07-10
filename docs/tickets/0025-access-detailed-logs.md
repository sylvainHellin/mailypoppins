---
id: 0025
title: access detailed logs
type: feature
priority: next
status: done
created: 2026-05-06
---

When using the TUI, the status is visible in a small pane in the main
view and its "details" in a dedicated overlay (`L`), but that overlay
does not contain all the details. For example, on a partial success
when sending an email, it does not show what worked and what didn't.
The detailed debug logs live in
`<data_dir>/logs/mailypoppins-YYYY-MM-DD.log` but there was no way to
reach them without leaving the TUI and hunting for the path.

## Fix

New global keybinding `Ctrl+l` opens the newest log file in `$EDITOR`,
using the same suspend/restore terminal mechanism as draft editing
(`suspend_terminal` / `edit_file` / `resume_terminal`).

- `latest_log_file()` in `src/config.rs` picks the newest
  `mailypoppins-*.log` in `logs_dir()` by filename (daily-dated names,
  so lexicographic order equals date order).
- If no log file exists, a warning status message is shown instead of
  crashing.
- `Ctrl+l` was chosen because plain `l` conflicts with the dir-picker
  navigation binding and `L` already opens the activity log overlay;
  Ctrl+l groups naturally next to `L` ("logs").

Files changed:
- `src/config.rs` -- `latest_log_file()` + unit test
- `src/tui/app/types.rs` -- `Action::OpenLogFile`
- `src/tui/app/keys.rs` -- global `Ctrl+l` binding
- `src/tui/actions.rs` -- suspend/edit/resume handler
- `src/tui/ui/overlays.rs` -- help overlay entry
- `website/src/pages/commands.astro` -- key binding docs
- `CHANGELOG.md`, `BACKLOG.md`
