---
id: 0136
title: GUI feedback after M4
type: feature
priority: now
status: in-progress
created: 2026-10-01
---

Sylvain's first feedback on the desktop client after M4 of the [native GUI plan](../plans/native-gui.md) (#0131), worked straight on `main` on 2026-10-01 in commits tagged `(#0136)`.
Four of its five items have landed; the logo is open.

## Key help filter landed

Commits `26a58be6` and `9f6d5a37`.

- The `?` key help opens with a filter field focused and empty on every open.
- The filter is a case-insensitive substring match on section, key and description, as in the TUI.
- Matching sections and rows render in source order, so DESKTOP stays last and a cleared filter shows the full list in its original order.
- A section left empty is hidden, and no match says "No matching key".
- Rows run nothing, and Escape closes.
- 490 vitest tests after it.

## Terminal editors landed

Commits `a51f80b3`, `8698834b`, `ff9bede3`, `12005208` and `28cd59e2`.

- A terminal editor in `$VISUAL` or `$EDITOR` (nvim, vim, hx and the like) runs inside the first terminal found as `EditorSource::Terminal`: Ghostty through `open -na`, kitty, Alacritty, WezTerm, then Terminal.app through `osascript` or `x-terminal-emulator`.
- With no terminal found the editor is skipped as before, and `MP_DESKTOP_EDITOR` and the `editor` setting are still used verbatim.
- Terminal.app's script quotes each word with every backslash outside the single quotes, so fish, bash, zsh and sh all read a path containing `\'` as one word; an ignored live test reads the words back in all four shells.
- `EditorLaunch` carries `fixture`, and in fixture mode an editor handoff says "Fixture mode: the editor was not launched" and names the command it would have run, for compose, signatures, `sc`, `sf` and the `invite.ics`.
- `docs/lessons-learned.md` records that Ghostty on macOS starts through `open -na`, that Alacritty's binary was killed on the development Mac, and that Terminal.app takes the editor as `osascript` argv.
- 214 Rust and 492 vitest tests after it.

## Light theme and desktop settings landed

Commits `a724f50c`, `612f844e` and `8d093791`.

- `src-tauri/src/settings.rs` owns `desktop.json`: `setting_get` and `setting_set` read and write one `SettingKey` (`editor`, `theme`, `reader_mode`), keep every other key, remove a key on `null` and refuse an unknown key with `not_found`.
- `editor_setting_get` and `editor_setting_set` delegate to it, and `SettingKey` reaches TypeScript through ts-rs.
- `src/index.css` has a light palette as `:root.light`, from Basenord Palette D: a cream canvas, off-white surfaces, Prussian text and tints, a pale Ocean selection with a darker Ocean ring and link, and Pumpkin for primary actions.
- The light `--warning` is `#B34700`, because Pumpkin reaches only 2.6:1 on cream, short of 4.5:1.
- `pnpm contrast` and the contrast test check both palettes, and `clients/desktop/docs/design-tokens.md` documents the 35 contrast pairs.
- `src/app/theme.ts` paints dark, light or system on `<html>`, with system following `prefers-color-scheme`; the default is dark.
- The stored theme is read at startup before the first render and kept in the store.
- Settings has Dark, Light and System buttons, and the palette has "Theme: dark", "Theme: light" and "Theme: system", with no key.
- 219 Rust and 506 vitest tests after it.

## Reader text mode landed

Commits `0d0134c5`, `e24933c2`, `7ded670e` and `ad02d064`.

- `reader_mode` takes `html` or `text` and refuses anything else, as `theme` does.
- The reader's plain-text fallback reads `message_text_on`, so the `mpmsg://` scheme and `message_text` answer the same body.
- `tt` from the list or the reader toggles between HTML and text, and the choice is stored as `reader_mode`.
- `ReaderText` renders the stored plain body in a mono `pre` on the app's surface, with line breaks kept and `>`-quoted lines muted, and says "No text body" when a message has none.
- The reader toolbar has a "Reader mode" group with HTML and Text, Settings has a "Reader" field, and the palette has "Toggle reader text mode", "Reader: HTML" and "Reader: text".
- The text cache is keyed by the daemon instance, the account, the row id, the Message-ID and the reader load, so a restarted daemon that gave a row id to another message does not show the old text.
- `tt` was the TUI's thread key; the desktop's thread row now lists no key, and the thread view stays "later".
- 221 Rust and 521 vitest tests after it.

## Open: the logo

- Sylvain picks one of the candidates in `assets/logo-candidates/`, which are not committed yet.
- The pick gets its colourways, the app icons through `pnpm tauri icon`, the website's logo and favicon, and a wordmark.
- The ticket closes when the logo ships.

## Follow-ups

- A bundled app launched from Finder may hand a bare `nvim` to the terminal when `$EDITOR` carries no path, and the terminal will not find it, since nvim lives in `~/.local/share/bob/nvim-bin` for Sylvain; running the command through `$SHELL -lc` would fix it.
- A bundled build needs an Apple Events usage string in `Info.plist` for the Terminal.app path.
- A light-theme user sees a dark flash at launch until `setting_get("theme")` answers; a copy of the theme in `localStorage`, read by `index.html`, would remove it.
- The Theme and Reader controls in Settings stay hidden until `config_get` loads.
- A `tt` pressed or a theme chosen before the startup `setting_get` answers can be overwritten by the stored value.
- Text mode shows the raw text, while the TUI strips signature markers and styles headings, rules and code blocks.
- The desktop thread view (`LST-10`), when built, needs a key other than `tt`.
- The website pages do not describe the desktop client yet.
