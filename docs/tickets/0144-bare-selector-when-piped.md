---
id: 0144
title: Print the bare selector when stdout is not a terminal
type: bug
priority: next
status: open
created: 2026-10-14
---

`mp new`, `mp edit`, `mp reply` and `mp forward` print `✓ <selector>` on stdout (`src/main.rs:3837`, `src/main.rs:3869`, and the shared helper at `src/main.rs:1819`).
A script that captures the output, `sel=$(mp new name)`, gets the check mark too, and the next call fails: `mp path "$sel"` answers `selector ✓ mp://tum/drafts/… has an empty segment`.
An agent hit this on 2026-10-14 and had to filter with `rg -o 'mp://\S+'`.

## Proposed approach

When stdout is not a terminal, print only the selector; on a terminal, keep `✓ <selector>`.
Check the other commands that print a selector or a path after a check mark (`rg '"\\u\{2713\}"' src/main.rs`) and apply the same rule where the value is meant to be reused.

## Acceptance criteria

- `sel=$(mp new x) && mp path "$sel"` works.
- Interactive output is unchanged.
- A test covers the piped case for `new` and `reply`.
- `docs/` and the website pages that show the output say what a pipe receives.
