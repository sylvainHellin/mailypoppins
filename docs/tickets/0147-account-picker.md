---
id: 0147
title: Account picker on ga in both clients, and other accounts collapsed in the desktop sidebar
type: feature
priority: next
status: done
created: 2026-10-22
---

## Problem

With four accounts, `ga` cycling to the next account was slow: reaching the fourth took three chords.
Sylvain asked (2026-10-22) for `ga` to open a list of the accounts like the attachment picker, with `j`/`k`, Enter to pick and Esc to cancel.
He also asked for the accounts other than the selected one to start collapsed in the sidebar tree, "especially for the GUI, but same for the TUI".
The desktop sidebar drew every account expanded; the TUI sidebar already drew only the active account.

## Decisions

Sylvain settled all six of the plan's decisions on 2026-10-22 (decision log, same date):

- A plain list with digits `1` to `9`, no filter field.
- New mail shows as the inbox unread count in the desktop picker and as a dot in the TUI picker.
- Accounts expanded by hand in the desktop sidebar stay so until quit; each start expands only the selected account.
- In the desktop sidebar `j`/`k` skip collapsed accounts, and `ga` is the keyboard path to another account.
- The unreachable `Ctrl+1..9` account jump in the TUI is removed.
- The TUI sidebar stays as it is.

[docs/plans/account-picker.md](../plans/account-picker.md) holds the design.

Research:

- [.agents/research/2026-10-22-account-picker-scouting.md](../../.agents/research/2026-10-22-account-picker-scouting.md)
- [.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md)

## Acceptance criteria, in short

- `ga` and the palette's "Switch account" open a list of every account in both clients, the current one preselected; `j`/`k` move, Enter switches, `1` to `9` jump, Esc and `q` cancel.
- With one account, `ga` does nothing and the which-key popup leaves it out.
- The desktop sidebar starts with only the selected account expanded; switching expands the new account, and a header click toggles an account for the session; an account expanded by hand stays expanded after it is left.
- The TUI sidebar is unchanged; `KeyAction::JumpAccount` and `Chord::CtrlDigit` are gone.
- `cargo test --workspace`, `pnpm test` and `pnpm build` pass, with the help and palette golden frames and `tui-keys.json` unchanged.

## Status

Done in both clients: the TUI's `Overlay::Account` with its golden frame `golden_account_picker`, and the desktop's `AccountPicker` dialog with the collapsed sidebar accounts.
