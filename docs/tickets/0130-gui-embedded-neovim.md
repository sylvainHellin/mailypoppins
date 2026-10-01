---
id: 0130
title: M5 of the native GUI, embedded Neovim composition
type: feature
priority: now
status: in-progress
created: 2026-09-25
---

Fourth ticket of the [native GUI plan](../plans/native-gui.md), milestone "M5: embedded Neovim", designed in the section "Embedded Neovim".
M1 to M4 landed under #0129 and #0131, and #0136 closed the first feedback round, so M5 started on 2026-10-01.

## Work

- Land the PTY and terminal dependencies #0128 validated.
- One real Neovim process per composition session, launched against the canonical draft path the daemon returns, with the user's own configuration and plugins.
- Composition replaces the reader pane; Neovim exit returns to a draft summary or the previous message.
- Draft creation, path handoff, watcher updates while Neovim is open, process exit, crash recovery with a reopen action, and explicit discard through `draft.discard`; `:q!` never deletes the canonical draft.
- Navigating away from an active session asks to keep it, terminate it while preserving the draft, or stay.
- Resolve the Neovim executable under a Finder launch, with an explicit editor-path setting and a probe of the common Homebrew and system locations.
- Keyboard-focus and resize tests.

## Exit gate

- New, reply, reply-all, forward and existing-draft edit workflows pass with a real Neovim process.
- User configuration and plugins load.
- Drafts survive every exit and crash path.

## Dependencies

The stack is the one [2026-09-30-tauri-stack.md](../baselines/decisions/2026-09-30-tauri-stack.md) sections 3 and 4 record, re-checked on 2026-10-01 and unchanged: `portable-pty` 0.9.0 on the Rust side; `@xterm/xterm` 6.0.0 with `@xterm/addon-fit` 0.11.0, `@xterm/addon-webgl` 0.19.0, `@xterm/addon-unicode11` 0.9.0 and `@xterm/addon-clipboard` 0.2.0 on the npm side.
`pty-process` 0.5.3 and `tauri-plugin-pty` 0.3.1 stay out: the first has no Windows path and the second hides the reader thread the batching needs.
`libghostty-vt` stays out: it is the VT state machine alone, without a renderer, and the full libghostty is not published for embedding.
The unicode11 addon gives xterm the width table Neovim assumes for CJK and emoji, and the clipboard addon answers the OSC 52 writes Neovim's `+` register makes, which is the only clipboard path a PTY has.

## Units

The compose flows in `src/app/compose.ts` and the sessions in `state.compose` stay the entry points; what changes is what `openInEditor` does when the resolved editor is a terminal editor.
A GUI editor (`code -w`, `zed`, `subl`) keeps the external route of M3.

### U1, the PTY in Rust (`src-tauri/src/terminal.rs`)

- A session table keyed by a session id: the `portable-pty` master, the child, the reader thread, and the draft it edits.
- `terminal_spawn(account, id, path, cols, rows, output: Channel)` resolves the editor as `editor.rs` does, refuses a GUI editor with a `setup` error naming it, and starts it with the user's login-shell `PATH` (`$SHELL -lc 'printf %s "$PATH"'`, read once per process), `TERM=xterm-256color`, `COLORTERM=truecolor`, the draft's directory as the working directory, and the draft path as the one argument.
- Output goes on the channel as raw bytes (`InvokeResponseBody::Raw`), coalesced in the reader thread to at most 64 KiB or 4 ms per message, which is what the spike's release build was missing.
- `terminal_write(session, bytes)`, `terminal_resize(session, cols, rows)` and `terminal_kill(session)`.
- The channel's last message is the exit: a tagged frame carrying the status, so ordering against the last output holds without a second event stream.
- Every live child is killed when its window closes, when the app exits, and when the session table drops; a child that outlives a crashed webview is not a goal.
- Fixture mode spawns nothing, journals the command, and `fixture_simulate("editor_save")` keeps working against it.
- The Finder `PATH` gap of #0136 closes here: a bare `nvim` in `$EDITOR` is found through the login shell, then in `PROBE_DIRS` and `~/.local/share/bob/nvim-bin`, and the setting can still name a full path.
- Tests: the coalescer against a scripted reader, the resolver's refusal of a GUI editor, and an end-to-end spawn of `sh -c` through a real PTY that echoes, resizes and exits with a known status; these run on Linux too.

### U2, the terminal pane in the webview (`src/lib/terminal.ts`, `components/compose/TerminalPane.tsx`)

- `src/lib/terminal.ts` wraps the four commands and the channel behind a `TerminalBridge` interface, so the pane and its tests do not touch Tauri.
- The pane mounts `@xterm/xterm` with fit, webgl (DOM renderer when WebGL refuses), unicode11 and clipboard, themed from the design tokens for both palettes, with the mono font the app uses.
- `onData` goes to `terminal_write`, the channel's bytes to `term.write`, a `ResizeObserver` plus fit to `terminal_resize`, and the exit frame to the store.
- Focus: the pane takes the focus when it mounts and when the user clicks it; while it has it, every key reaches Neovim except the native menu accelerators, which macOS handles before the webview and which are the "minimal documented set" of the design.
  Tab and Escape go to Neovim; leaving the pane is a click or a menu key.
- The pane is hidden, not unmounted, while another draft or a message is shown, so a background session keeps its buffer.
- Tests: vitest with a fake bridge for mount, data both ways, resize, focus and the exit frame.

### U3, the session in the shell (`compose.ts`, `reducer.ts`, `state.ts`, `Shell.tsx`, `EditingBanner.tsx`, `DraftPreview.tsx`)

- `ComposeSession` gains `kind: "embedded" | "external"` and, for embedded, the session id and a status of `running`, `exited` with its code, or `crashed`.
- `openInEditor` asks `editor_setting_get` which route applies and starts an embedded session in place of `editor_open` for a terminal editor.
- The reader pane shows the terminal pane for the session of the selected draft; the list keeps working beside it.
- Exit with status 0 returns the pane to the draft's preview (`DraftPreview.tsx`), or to the previous message when the draft is gone; `:q!` touches no file, so the canonical draft stays.
- A nonzero exit or a signal keeps the draft, shows the status in the banner, and offers "Reopen", which spawns again on the same path.
- Navigating away from a running session, by selecting another row, another mailbox, a view or a search, asks "Keep editing in the background", "Close the editor" (kills the child, keeps the draft) or "Stay"; a backgrounded session is listed in the editing banner and selecting its draft brings it back.
- The explicit discard (`cD` is demote; the discard is the existing `D` on a draft) confirms as it does today, then kills the session before `draft_discard`.
- Window close with a running session asks the same question with "Close the editor" as the default; the child is killed either way, since the window is the terminal.
- The watcher's `draft.changed` while the session runs already updates the list; the preview after exit reads the file fresh through `draft_preview`.
- Tests: reducer and flow tests with the fake bridge for every exit path, the navigate-away dialog, and the discard order.

### U4, the untested rows, the docs and the exit gate

- On the Mac, by hand, with a real Neovim and Sylvain's config: resize while typing, yank to the system clipboard and paste from it, IME (German umlauts through the dead keys, and a CJK input source), mouse (click to move, wheel to scroll, visual selection), CJK and emoji width, keyboard routing (Escape, Tab, Ctrl keys, the menu accelerators), close the window with the child running, kill the child from outside, the save reaching the list, a Finder launch of the dev bundle, and a launch of an ad-hoc signed bundle.
  Each row is ticked in this ticket with its date and what it showed; a row that fails becomes a follow-up, not a blocker, unless it loses a draft.
- `docs/shell.md` "Compose" describes the embedded route and the external one it keeps, `rust-layer.md` gets the four commands and the channel frame, `design-tokens.md` the terminal theme, the website's compose page follows, and the lessons go to `docs/lessons-learned.md`.
- The decision record gets a dated "M5 refresh" section with the versions above.
- The exit gate runs on the Mac: new, reply, reply all, forward and edit of an existing draft, each ending in a send through the outbox, plus the crash and the `:q!` paths.

### Order

U1 and U2 run in parallel once the bridge interface and the exit frame are agreed in writing (this ticket is that agreement).
U3 waits for both, and U4 for U3.

