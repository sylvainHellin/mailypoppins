---
id: 0147
title: Account picker on ga in both clients, and other accounts collapsed in the sidebar
type: feature
priority: next
status: open
created: 2026-10-22
---

## Problem

With four accounts, `ga` cycling to the next account is slow: reaching the fourth takes three chords.
Sylvain asked (2026-10-22) for `ga` to open a list of the accounts like the attachment picker, with `j`/`k`, Enter to pick and Esc to cancel.
He also asked for the accounts other than the selected one to start collapsed in the sidebar tree, "especially for the GUI, but same for the TUI".
Today the desktop sidebar draws every account expanded; the TUI sidebar already draws only the active account.

## Plan

[docs/plans/account-picker.md](../plans/account-picker.md) holds the design, six owner decisions with a recommendation each, four work units, the docs to change and the risks.

Research:

- [.agents/research/2026-10-22-account-picker-scouting.md](../../.agents/research/2026-10-22-account-picker-scouting.md)
- [.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md)
- [.agents/research/2026-10-22-chord-timeout-scouting.md](../../.agents/research/2026-10-22-chord-timeout-scouting.md)

## Acceptance criteria, in short

- `ga` and the palette's "Switch account" open a list of every account in both clients, the current one preselected; `j`/`k` move, Enter switches, `1` to `9` jump, Esc and `q` cancel.
- With one account, `ga` does nothing and the which-key popup leaves it out.
- The desktop sidebar starts with only the selected account expanded; switching expands the new account, and a header click toggles an account for the session.
- The TUI sidebar is unchanged, on the recommendation of the plan's Decision 6; the unreachable `KeyAction::JumpAccount` goes.
- `cargo test --workspace`, `pnpm test` and `pnpm build` pass, with the help and palette golden frames and `tui-keys.json` unchanged.

## Status

Implementation has not started.
The worker starts from the plan's first work unit, Unit 1 (the TUI picker), proceeding on the plan's recommendations unless Sylvain has overruled one in this ticket.
