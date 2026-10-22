# Design: an account picker on `ga`, and other accounts collapsed in the desktop sidebar

> Status: implemented (2026-10-22).
> The ticket is [#0147](../tickets/0147-account-picker.md).
> Sylvain settled every decision below on 2026-10-22 (decision log, same date).
> Citations name a symbol and its file.
> Research: [account picker scouting](../../.agents/research/2026-10-22-account-picker-scouting.md), [prior art](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md).

## Goal

Sylvain has four accounts.
His request (2026-10-22): `ga` cycling to the next account "was fine with two" and is cumbersome with four; `ga` should open "a list like the attachment picker", navigated with `j`/`k`, Enter picks, Esc cancels.
In the sidebar tree, accounts other than the selected one should be collapsed by default, "especially for the GUI, but same for the TUI".
The keybinding redesign already described `ga` as "Switch account (picker)" ([keybinding-redesign.md](keybinding-redesign.md), the `g` family).

## Design

### The picker, in both clients

- `ga` opens a modal list of the configured accounts in `config.toml` order, titled "Switch account", the selected account preselected and marked as current.
- `j`/`k` and the arrow keys move the cursor and clamp at the ends.
- Enter switches to the cursor's account and closes the picker; Enter on the current account only closes it, and in the desktop also expands it if it was collapsed by hand.
- `1` to `9` switch straight to the nth account and close the picker; a digit past the last account does nothing.
- Esc and `q` close it with nothing changed.
- Each row shows the digit, the name, the current marker and the new-mail indicator: the inbox unread count in the desktop, the `has_unseen` dot in the TUI.
- A row whose account is not serving says so: the runtime state (`opening` or `blocked`) in the desktop, `not ready` in the TUI, whose `AccountState::opening` is one flag for both.
- No type-to-filter field: a filter would take `j` and `k` as text, and at four accounts the digits are faster.
- The palette's "Switch account" opens the same picker.
- The KEYMAP label stays "Switch account", so `mp dump-keys`, `website/src/data/tui-keys.json`, the desktop's `keymap.json` and the help and palette golden frames do not change.
- With one account, `ga` and the palette entry open nothing, and the which-key popup leaves `ga` out.

### TUI

- `AccountPicker { selected }` and `Overlay::Account` in `clients/tui/src/app/types.rs`.
- `KeyAction::SwitchAccount` in `App::execute` (`clients/tui/src/app/keys.rs`) opens the overlay on `active_account` when there are two accounts or more; the palette runs the same arm without the key's guard, so the arm checks again.
- `App::handle_account_picker_key` clamps the cursor against `accounts.len()` on every key (a reload may remove accounts), and on a pick sets `Overlay::None` before `switch_account`, then promotes any pending error.
- `render_account_picker` in `clients/tui/src/ui/overlays.rs` draws it through `render_modal_shell`, with the `esc close` chip and the footer `j/k nav  1-9 jump  Enter switch  Esc cancel`.
- `App::prefix_popup_rows` leaves out a `Guard::MultiAccount` row with one account; the other guards stay advisory there.
- `KeyAction::JumpAccount`, its executor arm and `Chord::CtrlDigit` are gone: no KEYMAP row reached them since the keybinding redesign.
- `AccountState::for_tests(name)` is the minimal account the picker's tests build.
- The sidebar does not change: it draws only the active account.

### Desktop picker

- `ActionId` `open_account_picker` (`src/keymap/catalog.ts`, `MAIL_COMBOS.ga` in `src/keymap/useKeymap.ts`, `src/app/actions.ts`) dispatches the reducer action of the same name, which opens the `account_picker` overlay with two accounts or more and returns the state unchanged otherwise.
- It sits in neither `LEAVES_EDITOR` nor `USER_SELECTION`: opening a list navigates nowhere, and the pick runs `select_account`, which is in both.
- `prefixRuns` returns null for `open_account_picker` with fewer than two accounts.
- `AccountPicker` (`src/components/sidebar/AccountPicker.tsx`), mounted in `AppShell`, is a Base UI dialog with one button per `sidebarModel(s)` entry and a capture-phase window listener for `j`/`k`/arrows, Enter, `q` and the digits, in the shape of the attachment picker's `OpenList`; Enter picks the cursor's account itself, so it works before the dialog has moved the focus.
- `sidebarModel` carries each account's `inboxUnread`, read by the picker and the sidebar alike.

### Desktop sidebar

- `accountExpanded(s, name)` (`src/app/state.ts`) is `s.sidebarExpanded[name] ?? name === s.selection.account`.
- `sidebarExpanded` is session state in `AppState`, never in `Prefs`.
- `toggle_account_expanded { account }` stores the opposite of the current value.
- `followExpansion` (`src/app/reducer.ts`), a post-step of the exported `reducer`, deletes a `false` override of a newly selected account, so a switch always opens it, and clears a `sidebarCursor` whose account is now collapsed, so the cursor falls back to the selection.
- `sidebarEntries` skips collapsed accounts, so `j`/`k` walk only what is drawn.
- `expand_account { account }` deletes a `false` override; the picker dispatches it when it picks the current account, the keyboard path back to a selected account collapsed by mouse, whose header takes no key.
- `AccountGroup`'s label is a button with a chevron, `aria-expanded`, `aria-controls` and `tabindex="-1"`; a collapsed group draws its header with the inbox unread count and its `OutboxState` line, which renders only when something waits or failed.

## Decisions

1. A plain list with digits, no filter field; a filter can join if the account count passes nine.
2. New mail: the inbox unread count in the desktop, already on the client; the `has_unseen` dot in the TUI, which keeps no per-account unread counts.
3. Hand toggles in the desktop sidebar last for the session; each start expands only the selected account.
4. The desktop sidebar cursor stays on mailboxes and skips collapsed accounts; `ga` is the keyboard path to another account.
5. The unreachable `Ctrl+1..9` account jump in the TUI is removed.
6. The TUI sidebar stays as it is.

## Tests

- TUI, `clients/tui/src/app/keys.rs`: `ga` opens the picker on the active account; `j`/`k` clamp and Enter switches; Esc and `q` change nothing; a digit switches and one past the end does nothing; Enter on the active account only closes; a switch clears the new account's dot; with one account `ga` and the palette open nothing and the popup omits `ga`; with two the palette opens the picker.
- TUI render test `account_picker_lists_the_accounts_with_digit_current_and_new_mail_dot` in `clients/tui/src/ui/overlays.rs`: the rows, the current marker, the dot, the `not ready` tag, the cursor style and the blank digit past the ninth account.
  It is a render test rather than a golden frame: every golden frame needs a daemon twin (`tests/phase5_parity_gate.rs`), whose fixture has one account.
- Desktop, `src/app/reducer.test.ts`: the overlay with two accounts and a no-op with one; the expansion defaults, the toggle, a hand-expanded account kept after a switch away, a hand-collapsed account opened on selection, `expand_account`, and the cursor walk over expanded accounts.
- Desktop, `src/components/sidebar/accountPicker.test.tsx`: the picker's keys, click, Escape, `q`, digits, the search kept on the current account, the palette entry, one account; the collapsed headers, their unread count, the header toggle, the current account expanded again by `ga` Enter.
- Desktop, `src/components/palette/prefixPopup.test.tsx`: `g` without `ga` with one account.

## Risks

- The desktop opens the account's inbox on a switch while the TUI restores the account's parked mailbox; this divergence predates the picker.
