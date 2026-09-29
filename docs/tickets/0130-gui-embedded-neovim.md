---
id: 0130
title: M5 of the native GUI, embedded Neovim composition
type: feature
priority: next
status: open
created: 2026-09-25
---

Fourth ticket of the [native GUI plan](../plans/native-gui.md), milestone "M5: embedded Neovim", designed in the section "Embedded Neovim".
Blocked on #0131: the milestones run in order, so M5 starts after the read-only shell (M1), the mutations (M2), the external-editor compose it replaces (M3), and M4.

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
