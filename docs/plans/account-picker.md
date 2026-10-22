# Design: an account picker on `ga`, and other accounts collapsed in the desktop sidebar

> Status: proposed (2026-10-22), implementation not started. Ticket [#0147](../tickets/0147-account-picker.md). Implementer-facing.
> The plan proceeds on the recommendation of every decision under "Decisions for the owner", so a worker can start at Unit 1 without waiting.
> Citations name a symbol and its file; line numbers are hints from 2026-10-22 and move with the code.
> Research: [account picker scouting](../../.agents/research/2026-10-22-account-picker-scouting.md), [chord timeout scouting](../../.agents/research/2026-10-22-chord-timeout-scouting.md), [prior art](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md).

## Goal

Sylvain has four accounts now.
His request (2026-10-22): `ga` cycling to the next account "was fine with two" and is cumbersome with four; `ga` should open "a list like the attachment picker", navigated with `j`/`k`, Enter picks, Esc cancels.
In the sidebar tree, accounts other than the selected one should be collapsed by default, "especially for the GUI, but same for the TUI".
Both clients change: the TUI in `clients/tui` and the desktop app in `clients/desktop`.

The keybinding redesign already described `ga` as "Switch account (picker)" ([keybinding-redesign.md](keybinding-redesign.md), the `g` family); the shipped code cycles instead, so this plan delivers what that design named.

## Current state

### TUI

- `ga` is the KEYMAP row `pg("ga", Chord::Char('a'), 'g', KeyCtx::Global, Guard::MultiAccount, KeyAction::SwitchAccount, "Switch account", false)` in `KEYMAP` (`clients/tui/src/app/keymap.rs`, around line 757).
- `Guard::MultiAccount` is true when `self.accounts.len() > 1` (`App::guard_satisfied`, `clients/tui/src/app/keys.rs`, around line 197), so with one account `ga` resolves to nothing and the strict-prefix fallthrough in `App::handle_key` clears the chord.
- The executor arm `A::SwitchAccount` in `App::execute` (`keys.rs`, around line 266) computes `(self.active_account + 1) % self.accounts.len()` and calls `App::switch_account`.
- `App::switch_account` (`clients/tui/src/app/mod.rs`, around line 995) is a no-op for the active index; otherwise it parks the outgoing account (`save_to_account`), clears the incoming account's `has_unseen`, loads it (`load_from_account`), resets the Contacts and Calendar views, restores the parked cursor through `cursor_ref`, and focuses the list.
- The executor also has an `A::JumpAccount` arm for `Ctrl+1..9` (`keys.rs`, around line 271), but no `KEYMAP` row produces `KeyAction::JumpAccount`: the redesign dropped `Ctrl+1-9`, so the arm and the enum variant are unreachable.
- The command palette runs `SwitchAccount` through the same executor: `App::handle_command_palette_key` closes the palette and then calls `execute` (`keys.rs`, around line 2465), so a picker opened from the palette is not overwritten.
- The which-key rows come from `App::prefix_popup_rows` (`mod.rs`, around line 859) over `prefix_continuations` (`keymap.rs`, around line 1740), which filters on context and prefix but not on guards, so the popup lists `ga` even with one account.
- The attachment picker is a bespoke overlay: `AttachmentPicker { files, selected, mode, selected_set }` and `Overlay::Attachment` (`clients/tui/src/app/types.rs`), keys in `App::handle_attachment_picker_key` (`keys.rs`, around line 1864: `j`/`k`/arrows clamp, Enter consumes and closes, Esc or `q` closes), drawing in `render_attachment_picker` (`clients/tui/src/ui/overlays.rs`, around line 111) through `render_modal_shell` with an `esc close` chip.
- Overlays dispatch through one `match &self.overlay` at the top of `App::handle_key` (`keys.rs`), one in `render_overlays` (`clients/tui/src/ui/mod.rs`, around line 231), and one in `App::key_context` (`mod.rs`, around line 791), which returns `None` for modal pickers so the hint bar defers to their own chips.
- The overlay's own keys are hand-dispatched and have no `KEYMAP` rows, as the attachment picker's do not, so the `?` help overlay and the generated key data list only `ga` itself.
- The sidebar (`render_sidebar`, `clients/tui/src/ui/sidebar.rs`, around line 115) draws only the active account's mailboxes from `app.mailboxes`, titled with the active account's name in capitals when more than one account exists; other accounts are not drawn at all.
- Per-account state is `AccountState` (`types.rs`, around line 899): `mailbox_counts` holds totals (`decode_counts` over `totals_by_slug`, `clients/tui/src/queries.rs`), not unread counts, and `has_unseen` is set by `Message::MailboxChanged` for a non-active account (`App::update`, `mod.rs`, around line 1074) and cleared on switch.
- Tests that pin `ga`: `context_guards_are_modeled` in `keymap.rs` (the guard), the golden frames `golden_help_overlay` and `golden_command_palette` in `clients/tui/src/ui/golden_frames.rs` (the label "Switch account"), and the palette catalogue test in `keymap.rs` (around line 2253) that lists `JumpAccount` among the held-back actions.

### Desktop

- `ga` maps to the ActionId `next_account` in `MAIL_COMBOS` (`clients/desktop/src/keymap/useKeymap.ts`, around line 113); it is a Mail key, not one of `VIEW_AGNOSTIC_COMBOS` (`src/keymap/viewKeys.ts`).
- `catalog.ts` declares `next_account` in `ActionId` and maps the generated label "Switch account" to it (`src/keymap/catalog.ts`, around lines 23 and 146), which is what the palette and the which-key popup show.
- `runAction` dispatches `{ type: "next_account" }` (`src/app/actions.ts`, around line 105).
- The reducer case `next_account` (`src/app/reducer.ts`, around line 1752) takes the next name of `accountNames(s)` and re-enters the reducer with `select_account`; it has no guard, and with one account it selects the same account, which `select_account` returns unchanged unless a search is open.
- `next_account` is in `LEAVES_EDITOR` and `USER_SELECTION` (`reducer.ts`, around lines 1102 and 1522).
- `select_account` (`reducer.ts`, around line 1691) ends a search and retargets to the account's `defaultMailbox`, its inbox.
- `accountNames` (`src/app/state.ts`, around line 1050) is `list_accounts` order once loaded, else the bootstrap snapshot's, both in `config.toml` order.
- `Overlay` (`state.ts`, around line 176) is a string union of open dialogs, each mounted in `AppShell` (`src/components/shell/AppShell.tsx`, around lines 105 to 129) with `open={s.overlay === "..."}`.
- `AttachmentsDialog` (`src/components/attachments/AttachmentsDialog.tsx`, `OpenList`) handles `j`/`k`/arrows and `q` with a capture-phase window listener; `MovePicker` (`src/components/mutations/MovePicker.tsx`) is a cmdk `CommandDialog` with a filter field, where `j` and `k` would type into the filter.
- The sidebar is `AppSidebar` (`src/components/sidebar/AppSidebar.tsx`): `sidebarModel(s)` builds one entry per account with its mailboxes, each carrying `unread` and `total`, from the account's listing once read, else from the bootstrap snapshot, so every account's inbox unread count is already on the client.
- `AccountGroup` (`src/components/sidebar/AccountGroup.tsx`) renders a `SidebarGroupLabel` (name, `SyncHealthBadge`, runtime state when not ready) and always renders the mailboxes and the `OutboxState` line; there is no collapse state anywhere in `components/sidebar`.
- The sidebar keyboard cursor walks `sidebarEntries(s)` (`reducer.ts`, around line 899), every mailbox of every account in order, through `move_sidebar_cursor`; digits (`jump_mailbox`) address only the selected account.
- The exported `reducer` (`reducer.ts`, around line 1565) wraps `reduce` in `followContacts(followCalendar(...))`, the post-step where state that follows the selection's account is kept in line.
- Prefs (`Prefs` in `state.ts`, `loadPrefs` and `savePrefs` in `src/app/prefs.ts`) persist presentation choices in localStorage; `store.tsx` saves them on every change.
- Tests that use `next_account` or type `ga`: `src/app/reducer.test.ts` (around lines 226 and 1014), `src/app/calendar.test.ts` (around line 174), `src/components/reader/invite.test.tsx` (around line 218), `src/components/calendar/invitation.test.tsx` (around line 114); `src/components/palette/prefixPopup.test.tsx` (around line 72) expects `ga` among the `g` rows.
- The test fixtures have two accounts, `work` (default) and `home` (`clients/desktop/fixtures/accounts.json`).

### The daemon

- `account.list` (`src/daemon/methods/account.rs`, `AccountEntry`) answers `name`, `default`, `backend` and `state` only, with no counts.
- The desktop's `AccountInfo` (`clients/desktop/src/protocol/generated/gui/AccountInfo.ts`) adds runtime state, sync health and outbox counts, also without unread counts.
- Per-mailbox `unread` travels in `MailboxRow` (`crates/mp-protocol/src/state.rs`) through `state.bootstrap` and `mailbox.list`; nothing in this plan changes the daemon or the protocol.

## Design

### The picker, in both clients

- `ga` opens a modal list of the configured accounts in `config.toml` order, titled "Switch account".
- The selected account is preselected and marked as current.
- `j`/`k` and the arrow keys move the cursor and clamp at the ends, as the attachment picker does.
- Enter switches to the cursor's account and closes the picker; Enter on the current account only closes it.
- `1` to `9` switch straight to the nth account and close the picker; a digit past the last account does nothing.
- Esc, and `q` as in the attachment picker, close it with nothing changed.
- Each row shows the digit, the account name, a marker for the current account, and a mail indicator: the inbox unread count in the desktop, the `has_unseen` dot in the TUI (Decision 2).
- No type-to-filter field: at four accounts the digits are faster, and a filter field would take `j` and `k` as text (Decision 1).
- The palette's "Switch account" opens the same picker in both clients.
- The KEYMAP label stays "Switch account", so `mp dump-keys`, `website/src/data/tui-keys.json`, the desktop's generated `keymap.json` and the help and palette golden frames do not change.
- With one account, `ga` does nothing in both clients and the which-key popup leaves it out; the palette entry opens nothing.

### TUI picker

- A new `AccountPicker { selected: usize }` in `clients/tui/src/app/types.rs` and an `Overlay::Account(AccountPicker)` variant beside `Overlay::Attachment`.
- `A::SwitchAccount` in `App::execute` opens the overlay with `selected = self.active_account` when there is more than one account; from the palette with one account it sets nothing.
- `App::handle_account_picker_key` in `keys.rs`, modelled on the `AttachmentPickerMode::Open` arm of `handle_attachment_picker_key`, takes the picker out with `std::mem::replace` before calling `switch_account`, so the switch runs with no overlay up.
- `render_account_picker` in `clients/tui/src/ui/overlays.rs`, modelled on `render_attachment_picker`: `render_modal_shell`, the title, the `esc close` chip, one line per account as `> 1  work      ●`, the cursor row in the selection style.
- The new variant joins the `None` arm of `App::key_context`, the dispatcher in `App::handle_key` and `render_overlays`; the compiler's exhaustiveness check names every other match.
- `App::prefix_popup_rows` drops a row whose guard is `Guard::MultiAccount` while `self.accounts.len() < 2`, so the popup and the resolver agree; the other guards keep their current advisory display.
- The unreachable `A::JumpAccount` arm and `KeyAction::JumpAccount` go, with their mentions in `KeyAction::palette_runnable` and the palette catalogue test (Decision 5).

### Desktop picker

- `Overlay` gains `"account_picker"`; the cursor lives in the component, as the key help's does, so no other state field is needed.
- `ActionId` `next_account` is renamed `open_account_picker` in `catalog.ts`, `useKeymap.ts` (`MAIL_COMBOS.ga`), `actions.ts` and the reducer.
- The reducer case `open_account_picker` returns `s` when `accountNames(s).length < 2`, else sets `overlay: "account_picker"`; it leaves `LEAVES_EDITOR` and `USER_SELECTION`, since opening a list navigates nowhere, and `select_account` already sits in both.
- `prefixRuns` in `useKeymap.ts` returns null for `open_account_picker` with fewer than two accounts, so the popup leaves `ga` out.
- A new `AccountPicker` dialog in `src/components/sidebar/AccountPicker.tsx`, mounted in `AppShell` beside `AttachmentsDialog`: a shadcn `Dialog` with a list of buttons, one per `sidebarModel(s)` entry, and a capture-phase window key listener for `j`/`k`/arrows/Enter/`q`/digits copied from `OpenList`; the dialog's own Escape closes it.
- Enter, a digit or a click dispatches `select_account` for another account and closes the overlay; the current account only closes it, so an open search survives a picker opened by mistake.
- The inbox unread count is the `unread` of the entry's mailbox whose `kind` is `inbox`; a runtime state other than `ready` shows muted beside the name, as `AccountGroup` shows it.
- The dialog's description reads "j and k move, Enter switches, 1 to 9 jump, Escape cancels", the way `MovePicker` describes its keys.

### Desktop sidebar: other accounts collapsed

- An account's mailboxes show when `accountExpanded(s, name)` is true: `s.sidebarExpanded[name] ?? name === s.selection.account`.
- `sidebarExpanded: Record<string, boolean>` is a new `AppState` field, empty at start and never saved (Decision 3).
- A new reducer action `toggle_account_expanded { account }` stores the opposite of the current value as an override.
- A new post-step `followExpansion` in the exported `reducer`, beside `followCalendar` and `followContacts`, deletes the override of the account that just became selected, so a switch always opens the new account even if it was collapsed by hand, and the account left behind falls back to collapsed unless the user had expanded it by hand.
- `AccountGroup` gets `expanded` and `onToggle` props and an `inboxUnread` number; its label becomes a button with a chevron, `aria-expanded` and `aria-controls`, and a click toggles the group without switching account.
- A collapsed group draws only its label row with the inbox unread count, plus the `OutboxState` line when it has something to say, so a failed send in a collapsed account stays visible.
- `sidebarEntries` skips the mailboxes of a collapsed account, so `j`/`k` in the sidebar walk only what is drawn; the keyboard path to another account is `ga` (Decision 4).
- `followExpansion` also clears a `sidebarCursor` whose account is now collapsed, so the cursor falls back to the selected mailbox (`s.sidebarCursor ?? selection` in `move_sidebar_cursor` and `AppSidebar`); left alone, the `cur < 0` branch of `move_sidebar_cursor` would jump to the first entry instead.

### TUI sidebar

The TUI already meets the collapse request, and more strictly: its sidebar draws only the active account and no row of any other account (`render_sidebar`).
This plan changes nothing there (Decision 6).

## Decisions for the owner

### 1. A plain list or a filterable list

The desktop already has a filterable picker (`MovePicker`, cmdk) and the TUI has a fuzzy one (`MailboxPicker`).
A filter field takes `j` and `k` as text, so it would contradict the requested `j`/`k` navigation, and with four accounts typing saves nothing over a digit.
A plain list matches the attachment picker the request names and stays one key per action.
Recommendation: a plain list with digits, no filter; a filter can join later if the account count grows past nine.

### 2. What each picker row shows about new mail

The desktop has every account's inbox unread count on the client already (`sidebarModel`), so it costs nothing to show.
The TUI keeps only mailbox totals per account and a `has_unseen` flag that `MailboxChanged` sets for a background account.
Showing unread counts in the TUI would mean keeping the `unread` of `mailbox.list` per account and refreshing it on background events, which is new state for a cosmetic gain.
Recommendation: the inbox unread count in the desktop and the `has_unseen` dot in the TUI; a TUI count can follow if the dot proves too coarse.

### 3. Whether manual expand and collapse survive a restart

Session-only state recomputes the default from the selection at every start, which is exactly "collapsed by default", and an override never outlives an account rename or removal.
Persisting the overrides in `Prefs` (localStorage) would keep a two-account working set open across restarts, at the cost of a stale list after a rename and a sidebar that no longer starts collapsed.
The prior-art research found no client that documents a remembered expand state.
Recommendation: session-only, held in `AppState` and not in `Prefs`; persisting is a small follow-up if Sylvain finds himself re-expanding the same account each morning.

### 4. How the keyboard reaches a collapsed account in the desktop sidebar

Option one keeps the sidebar cursor on mailboxes only and skips collapsed accounts; `ga` is then the keyboard path to another account, and the cursor model (`sidebarCursor: { account, slug }`) does not change.
Option two adds a cursor stop on every account header, with Enter expanding or collapsing it, which needs a nullable `slug` in the cursor and changes `sidebar_enter` and the cursor drawing.
Recommendation: option one, because the picker is the requested way to change account and option two adds a second keyboard model for the same job.

### 5. The unreachable `Ctrl+1..9` account jump in the TUI

`KeyAction::JumpAccount` and its executor arm survive from before the keybinding redesign, which dropped `Ctrl+1-9`; no `KEYMAP` row reaches them.
Restoring a row would give two digit paths to an account; removing them leaves the picker's digits as the only one.
Recommendation: remove the variant and the arm in Unit 1, since `ga` then a digit is the replacement and dead code invites a wrong reading of the keymap.

### 6. Whether the TUI sidebar should list other accounts as collapsed rows

The TUI sidebar shows only the active account, which already hides the others completely.
Adding a collapsed row per other account would show their new-mail dot without opening the picker, at the cost of sidebar height in a forty-column pane and a second account switcher.
Recommendation: no TUI sidebar change; the picker carries the account list and the dot.

## Work units

Each unit lands as its own commit with its tests and passes before the next starts.
After any Rust change run `cargo install --path .` before a manual check.

### Unit 1: the TUI picker

Files:

- `clients/tui/src/app/types.rs`: `AccountPicker` and `Overlay::Account`.
- `clients/tui/src/app/keys.rs`: the `Overlay::Account` arm in `handle_key`, `handle_account_picker_key`, the `A::SwitchAccount` arm opening the overlay, the `A::JumpAccount` arm removed.
- `clients/tui/src/app/keymap.rs`: `KeyAction::JumpAccount` removed with its `palette_runnable` mention; the palette catalogue test's held-back list updated.
- `clients/tui/src/app/mod.rs`: `key_context` covers the variant; `prefix_popup_rows` drops `Guard::MultiAccount` rows with one account; an accessor for the picker if the renderer needs one, as `attachment_picker_mut` exists.
- `clients/tui/src/ui/overlays.rs` and `clients/tui/src/ui/mod.rs`: `render_account_picker` and its arm in `render_overlays`.

Tests to add, beside the attachment picker's tests in `keys.rs`:

- `ga` with three accounts opens the picker with the active account selected and leaves the account unchanged.
- `j` then Enter switches to the next account and closes the picker; `k` at the top stays at the top.
- Esc and `q` close the picker and leave `active_account` unchanged.
- `3` switches to the third account; `9` with three accounts does nothing and keeps the picker open.
- Enter on the active account closes the picker without a switch.
- With one account `ga` opens nothing, the palette's "Switch account" opens nothing, and `prefix_popup_rows` after `g` has no `ga` row.
- A golden frame `golden_account_picker` in `clients/tui/src/ui/golden_frames.rs` with three accounts, one of them with `has_unseen`, approved with `cargo insta review`.

Tests that must stay green unchanged: `context_guards_are_modeled`, `golden_help_overlay`, `golden_command_palette`.

Verify:

- `cargo test -p mp-tui`, then `cargo test --workspace`.
- `cargo install --path .`, then a run against a config with at least three accounts: `ga`, `j`, Enter; `ga`, `2`; `ga`, Esc.

### Unit 2: the desktop picker

Files:

- `clients/desktop/src/app/state.ts`: `"account_picker"` in `Overlay`.
- `clients/desktop/src/app/reducer.ts`: `open_account_picker` replaces `next_account` in the `Action` union, the reducer and the two sets.
- `clients/desktop/src/app/actions.ts`, `src/keymap/catalog.ts`, `src/keymap/useKeymap.ts`: the rename, and the one-account rule in `prefixRuns`.
- `clients/desktop/src/components/sidebar/AccountPicker.tsx`: the dialog.
- `clients/desktop/src/components/shell/AppShell.tsx`: the mount.

Tests to add or update:

- A new `src/components/sidebar/accountPicker.test.tsx`: `ga` opens a dialog named "Switch account" with `work` current and selected; `j` then Enter selects `home` and closes it; `2` selects `home`; Escape and `q` leave `work` selected; a click on `home` selects it; Enter on `work` with a search open keeps the search.
- `src/app/reducer.test.ts`: `open_account_picker` is a no-op with one account and opens the overlay with two; the two existing `next_account` cases (around lines 226 and 1014) move to `select_account`, since they test what follows a switch.
- `src/app/calendar.test.ts` (around line 174): the same move to `select_account`.
- `src/components/reader/invite.test.tsx` (around line 218) and `src/components/calendar/invitation.test.tsx` (around line 114): `ga` becomes `ga` then `2`.
- `src/components/palette/prefixPopup.test.tsx`: a one-account case where the `g` rows leave out `ga`; the existing two-account expectation (around line 72) stays.
- `src/components/palette/palette.test.tsx`: "Switch account" from the palette opens the picker.

Verify:

- `cd clients/desktop && pnpm vitest run src/components/sidebar src/app/reducer.test.ts src/components/palette`, then `pnpm test` and `pnpm build` (type-check and bundle; the `prebuild` hook keeps the committed `keymap.json` without `MP_BIN`).
- The Tauri crate is untouched, so nothing here needs the Mac to build; the real-window check runs on the Mac after `scripts/install-local.sh`.

### Unit 3: collapsed accounts in the desktop sidebar

Files:

- `clients/desktop/src/app/state.ts`: `sidebarExpanded` in `AppState` and `initialState`, and `accountExpanded(s, name)`.
- `clients/desktop/src/app/reducer.ts`: `toggle_account_expanded`, `followExpansion` in `reducer`, `sidebarEntries` skipping collapsed accounts.
- `clients/desktop/src/components/sidebar/AppSidebar.tsx`: `expanded`, `onToggle` and `inboxUnread` passed per account.
- `clients/desktop/src/components/sidebar/AccountGroup.tsx`: the toggle button, the collapsed rendering, the outbox line kept.

Tests to add:

- Reducer: at start only the selected account is expanded; `toggle_account_expanded` on `home` expands it; `select_account` to `home` then back to `work` leaves `home` expanded only if it had been expanded by hand; a hand-collapsed `home` expands when selected; `move_sidebar_cursor` never lands on a mailbox of a collapsed account.
- Component, in a new `src/components/sidebar/sidebar.test.tsx` or beside the sidebar cases of `src/components/shell/a11y.test.tsx`: `home` renders with `aria-expanded="false"`, no mailbox rows and its inbox unread count; a click on its label expands it without changing the selected account; switching to `home` through the picker collapses `work`.

Existing tests at risk: any test that clicks a mailbox of `home` in the sidebar without selecting `home` first now finds no such row.
Before changing code, list them with `rg -n '"home"' clients/desktop/src --glob '*.test.*'` and give each an explicit expand or `ga` step rather than weakening the default.

Verify:

- `pnpm vitest run src/app/reducer.test.ts src/components/sidebar src/components/shell`, then `pnpm test` and `pnpm build`.
- On the Mac, in a real window with four accounts: start, check that only the selected account is open, `ga` to another, click a collapsed header, restart and check that the default is back.

### Unit 4: docs at ship time

The list under "Docs to change at ship time" below, in the same commit as the last code unit or right after it.

## Docs to change at ship time

- `README.md`, the key table row `ga`/`gm` (around line 286): "Account picker / Go to the sidebar".
- `website/src/pages/config.astro` (around line 315): `ga` opens a list of the accounts, in the TUI and the desktop app.
- `website/src/pages/faq.astro` (around line 184): the same sentence for the multiple-accounts answer.
- `website/src/data/tui-keys.json`: regenerated with `scripts/regen-website-keys.sh`; it should come out unchanged because the label stays, and a diff there means the label moved by accident.
- `docs/parity-matrix.md`: `MBX-04` (the TUI anchor, stale at `keymap.rs:591`, and the GUI location, which says "`ga` selects the next account"), `MBX-02` (the desktop sidebar shows only the selected account's mailboxes until another is expanded), `ACC-09` (around line 151, "the sidebar lists every account, and `ga` ... selects"), and the stale `MBX-04` line in "Anchor corrections" (around line 1479).
- `clients/desktop/docs/shell.md`: the Keys list (around line 1074, "`ga`: next account"), the Calendar note on the palette's "Switch account" (around line 653), and the Model section for `sidebarExpanded` and the picker overlay.
- The in-app help needs no edit: the TUI `?` overlay and the desktop key help read the unchanged "Switch account" row, and the picker shows its own keys in its chip and description.
- `CHANGELOG.md` under `[Unreleased]` at ship, in the ticket's words; `BACKLOG.md` loses the #0147 line; the ticket goes to `status: done`.
- `docs/plans/keybinding-redesign.md` stays as written: its `ga` row already says picker.

## Acceptance criteria

- In both clients with two or more accounts, `ga` opens a list of every account in `config.toml` order with the current one preselected.
- `j`/`k` and the arrows move, Enter switches, `1` to `9` switch directly, Esc and `q` cancel with nothing changed.
- The palette's "Switch account" opens the same list.
- With one account, `ga` and the palette entry do nothing and the which-key popup does not list `ga`.
- The desktop rows show each account's inbox unread count; the TUI rows show the new-mail dot.
- The desktop sidebar starts with only the selected account expanded; other accounts show one header row with the inbox unread count and any outbox line.
- Switching account expands the new account and collapses the old one unless the user expanded it by hand; a header click toggles an account without switching.
- The TUI sidebar is unchanged.
- `cargo test --workspace`, `pnpm test` and `pnpm build` pass; the help and palette golden frames and `tui-keys.json` are unchanged.

## Risks

- Desktop tests that reach a `home` mailbox through the sidebar break once `home` starts collapsed; Unit 3 lists them first.
- `select_account` with the current account ends an open search; the picker must not dispatch it for the current account.
- The TUI picker must take itself out of `self.overlay` before `switch_account`, which sets the focus and may raise a status; leaving it up would draw the picker over the new account.
- A `Guard::MultiAccount` filter in `prefix_popup_rows` must not hide the `DraftsOnly` and `NonEmptyList` rows, which the popup shows on purpose today.
- An account added by a config reload while the desktop picker is open: the dialog reads `sidebarModel(s)` on every render, so it shows the new row; the TUI picker clamps `selected` against `self.accounts.len()` on every key.
- The desktop opens the account's inbox on a switch while the TUI restores the account's parked mailbox; this divergence predates the plan and stays.

## Research

- [.agents/research/2026-10-22-account-picker-scouting.md](../../.agents/research/2026-10-22-account-picker-scouting.md)
- [.agents/research/2026-10-22-chord-timeout-scouting.md](../../.agents/research/2026-10-22-chord-timeout-scouting.md)
- [.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md](../../.agents/research/2026-10-22-chord-timeout-and-account-picker-prior-art.md)
