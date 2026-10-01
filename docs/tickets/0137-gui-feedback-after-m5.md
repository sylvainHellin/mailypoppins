---
id: 0137
title: Feedback after M5, the embedded editor in use
type: feature
priority: now
status: in-progress
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

## Follow-ups

- Filled in as the units land.
