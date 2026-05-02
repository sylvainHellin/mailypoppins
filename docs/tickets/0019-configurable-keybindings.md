---
id: 0019
title: Configurable keybindings
type: feature
priority: later
status: open
created: 2026-05-01
---

Allow remapping keys via `~/.config/email/config.toml`. The current bindings are vim-flavoured and hardcoded.

## Notes

- Define an `Action` enum (already exists in `tui/app/types.rs`) and let users map keys -> actions:

  ```toml
  [keybindings]
  archive = "a"
  delete = "d"
  reply = "r"
  ```

- Validate at config load: every action must be either explicitly bound or fall back to the default.
- Conflict detection: error if two actions claim the same key.
