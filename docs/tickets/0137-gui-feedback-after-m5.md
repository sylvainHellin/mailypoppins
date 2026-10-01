---
id: 0137
title: Feedback after M5, the embedded editor in use
type: feature
priority: now
status: done
created: 2026-10-01
---

Second feedback round of the [native GUI plan](../plans/native-gui.md), after #0130 landed and Sylvain ran the embedded editor on the Mac.
Commits carry `(#0137)`.

## Units

### U1, key repeat in the webview (`src-tauri/src/lib.rs`)

- A held `j` moves the cursor once: macOS's press-and-hold accent popup (`ApplePressAndHoldEnabled`) suppresses key repeat for every key in WKWebView, as it does in VS Code until `defaults write com.microsoft.VSCode ApplePressAndHoldEnabled -bool false`.
- Register `ApplePressAndHoldEnabled = false` in the app's `NSUserDefaults` registration domain at startup on macOS (`objc2-foundation` is already in the dependency tree through tao and wry), so a held key repeats everywhere in the app and a user who set the key per bundle keeps their value.
- A lessons-learned entry and a row in `shell.md` "Keyboard".

### U2, the editor follows the app's theme (`src-tauri/src/terminal.rs`, `resources/`, settings)

- Entering the editor breaks the theme: Neovim applies the user's colorscheme over the pane, whose ANSI palette already comes from the design tokens.
- Ship `mailypoppins.vim`, a colorscheme for Neovim and Vim built on the design tokens of both palettes (`guifg`/`guibg` hex values from `index.css` and the matching `ctermfg`/`ctermbg` of the 16 `--terminal-*` slots), selected by `&background`; a test keeps its hex values equal to `index.css`.
- `terminal_spawn` takes the app's current theme (`dark` or `light`); for `nvim` and `vim` it prepends the resource directory to the runtime path (`--cmd`) and, when the editor-colours setting is "follow the app", appends `-c "set background=<theme>" -c "colorscheme mailypoppins"`; `hx` and a user whose setting is "the editor's own" get nothing.
- `MP_DESKTOP_THEME=<theme>` is set on the child either way, so a user's own config can react to it.
- A desktop setting `editor.colors: "app" | "editor"` (default `app`) with a row in the Settings view; a theme change while the editor runs applies at the next spawn, which `shell.md` says.

### U3, the pending-prefix overlay (`src/keymap/useKeymap.ts`, `src/components/palette/`)

- After `c` in Mail nothing shows the continuations; the TUI draws a which-key popup (`clients/tui/src/ui/mod.rs` `render_prefix_popup`, rows from `prefix_popup_rows`) titled by the family (`compose`, `go`, `find`, `thread`, `system`, `view`).
- The desktop keymap arms a prefix in `prefix.current` and renders nothing; lift the armed prefix into state the shell can read, and render a small popup near the bottom of the window listing the continuations the current view and focus accept, from `keymap.json` through the catalog, with the key in the accent colour and the action beside it, titled by the family as the TUI does.
- It disappears on the next key, on the 1200 ms timeout (so a timer now clears the prefix, which also fixes a stale prefix firing after the timeout check), and on blur.
- Tests: the popup for `c` in Mail lists the compose continuations, `g` in a full-pane view lists only that view's `g` combos, the timeout hides it, and the next key hides it whether it resolves or not.

### U4, recipient completion in the compose dialogs (`src/components/compose/ComposeWizard.tsx`)

- The desktop's to/cc/bcc fields are plain inputs; the TUI completes contacts as you type (`clients/tui/src/app/keys.rs` `recompute_compose_suggestions`, `accept_suggestion`): the query is the text after the last comma, at least one character, up to 12 candidates ranked by `sent_to` and `sent_cc`, accepted as `Name <addr>` plus `", "`.
- Give the three fields the same: `contact_search(account, query, 12)` debounced as the user types, a list under the field (cmdk `Command` from `src/components/ui/command.tsx` in a popover-like panel anchored to the input) showing name and address, arrow keys and Enter or Tab to accept, Escape closes the list and leaves the text; accepted as the TUI formats it; an empty cache shows the TUI's hint to rebuild the contacts.
- Enter without an open list keeps moving to the next field as today.
- Tests: typing opens the list with the fake command's rows, accepting rewrites only the text after the last comma, Escape leaves the text, Enter with no list moves on, the empty cache hint.

## Landed

- U1 and U2 in `49f03ffa` (branch `gui-0137-a`): `key_repeat()` in `lib.rs` registers `ApplePressAndHoldEnabled = false` in the registration domain before the window exists, so a per-bundle or global `defaults write` still wins; `objc2-foundation` 0.3.2 is a macOS-only direct dependency.
- `resources/nvim/colors/mailypoppins.vim` works in Neovim and Vim, both `gui*` and `cterm*`, and `src/design/colorscheme.test.ts` keeps its 24 hex values equal to `index.css`.
- `terminal_spawn` takes `theme`; for a program whose file name (or link target) is `nvim` or `vim` the argv gets `--cmd "set runtimepath^=<resources>/nvim"`, the same again as the first `-c` (lazy.nvim resets the runtime path during startup), then with the setting `editor_colors = app` (the default) `-c "set background=<theme>"` and `-c "colorscheme mailypoppins"`, all before the template's arguments and the draft path; every editor gets `MP_DESKTOP_THEME`.
- The setting is a flat `editor_colors` key in `desktop.json` through `setting_get|set`, with the Settings row "Editor colours" ("Follow the app" / "The editor's own"); a theme change applies at the next spawn.
- A debug build falls back to the source tree's `resources/nvim` since `resource_dir()` only resolves under a directory named `target`.
- U3 in `d91e94f6` (branch `gui-0137-b`): `src/keymap/pendingPrefix.ts` publishes the armed prefix through `useSyncExternalStore`; a timer clears it after `PREFIX_TIMEOUT_MS`, and so do window blur, focus into a field or the terminal and unmount; the next non-modifier key consumes it whether it resolves or not.
- The big `switch` in `useKeymap.ts` became data (`MAIL_COMBOS`, `COMPOSE_ROW_KEYS`) behind `resolvePrefix`, and `prefixRuns` filters the catalog rows for `PrefixPopup.tsx`, fixed at the bottom centre above the notice line, titled by the family as the TUI's `prefix_family_name`, two columns past 8 rows.
- U4 in `608d8b51` (branch `gui-0137-c`): `RecipientsInput.tsx` wraps the To, Cc and Bcc inputs of every compose dialog with a cmdk list under the field, `contact_search(account, query, 12)` debounced 120 ms with a sequence counter against stale answers; ArrowUp/Down, Enter or Tab or a click accept as `Name <addr>, ` (`acceptRecipient` copies the TUI's `accept_suggestion`), Escape closes the list and keeps the text; an empty index shows "No contacts yet: rebuild the index in Contacts" once per focus.
- Counts on main: 256 cargo (3 ignored), 603 vitest, tsc clean.

## Follow-ups

- Key repeat and the two themes were verified outside the app (headless nvim and vim with the exact argv, a Rust test for the registration), not in the running window: Sylvain's run.
- A resource path containing a backslash, `$`, `'`, a backtick, `[...]` or `{...}` breaks the runtimepath glob in both editors (E185 and an "Error in command line" prompt, the editor runs in its own colours); `Launch::dressed` could leave the colorscheme words out in that case.
- A plugin that sets a colorscheme on `VimEnter` or lazily still overrides the app's.
- Without `termguicolors` the light palette's surfaces are the nearest ANSI slot and `CursorLine` has no background.
- The TUI's status-bar hint for a pending prefix (`clients/tui/src/ui/status.rs:48`) has no desktop counterpart.
- The highlighted cmdk row was `muted`, barely visible in the dark theme; it is now `selection`, the cursor-row token, in the palette, the key help and the recipient completion alike.
- `src/components/outbox/outbox.test.tsx` ("the palette's actions on the selection" and "Clear selection") timed out under a loaded full run: typing each action's name re-filtered the palette's whole catalog per key, so the tests now paste the name in one input event (836 ms to 409 ms for the first alone).
- `ActivityLogDialog.tsx` imports `PREFIX_TIMEOUT_MS` from `useKeymap.ts` instead of keeping its own copy.
- A terminal editor in `MP_DESKTOP_EDITOR` or the setting, the one that runs drafts embedded, ran with no terminal for `config.toml`, the daemon's log, a signature and an `invite.ics`; it now opens them in a window of the first terminal emulator found, as `$EDITOR` does.
