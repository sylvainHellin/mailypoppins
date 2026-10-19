---
id: 0144
title: Print the bare selector when stdout is not a terminal
type: bug
priority: next
status: done
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

## Resolution (2026-10-19, PERSO-145)

`hand_back` in `src/main.rs` prints the decorated lines on a terminal and the bare value otherwise.
It covers `new`, `edit`, `reply`, `forward`, `mark-approved` and `mark-draft` (both the `✓` and the `ℹ … is already …` line), `archive` (the pipe gets the selector the message moved to) and `save` (the pipe gets one path per file).
`open` and `delete` keep their lines: a handle path the viewer holds and a selector that no longer resolves are not values to reuse.
The parity suites rewrite the pre-daemon oracle's stdout into the piped form (`support::parity::piped_since_0144`), the way `pin_font_size` takes #0127 out of the parity rows.
Tests: `tests/cli_selector_contract.rs` pins the piped `new`, `mark-approved`, `reply` and `forward`, and, through `script`, the unchanged terminal output of `new` and `reply` (Linux only).
The website's Selectors section and the README say what a pipe receives.
