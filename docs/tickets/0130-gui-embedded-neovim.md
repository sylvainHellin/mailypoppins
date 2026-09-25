---
id: 0130
title: Phase 8 of the native GUI, embedded Neovim composition
type: feature
priority: next
status: open
created: 2026-09-25
---

Third ticket of the GUI half of the [native GUI plan](../plans/native-gui.md), sections "Embedded Neovim" and "Phase 8: Embedded Neovim".
Blocked on #0129, the shell.

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
