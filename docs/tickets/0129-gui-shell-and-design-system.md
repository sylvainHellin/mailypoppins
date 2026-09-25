---
id: 0129
title: Phase 7 of the native GUI, the shell and the design system
type: feature
priority: next
status: open
created: 2026-09-25
---

Second ticket of the GUI half of the [native GUI plan](../plans/native-gui.md), sections "GUI architecture" and "Phase 7: GUI shell and design system".
Blocked on #0128, the Phase 1b spikes.

## Work

- Scaffold `desktop/` with Tauri 2, React, TypeScript, Vite, Tailwind and the selected shadcn components, using the dependencies #0128 recorded.
- The Tauri Rust layer talks to the daemon through `mp-client` only, announcing `ClientKind::Gui`, and holds one connection and one ordered subscription.
- Generate the TypeScript protocol types from `mp-protocol`; the `schemars` question #0119 deferred reopens here, with dependency due diligence before adoption.
- Generate key help and command-palette data from the same `KEYMAP` source as `mp dump-keys --json`.
- Connection, handshake, bootstrap, reconnect, resync and version-mismatch restart screens.
- Dark semantic tokens from the inverse of Basenord Palette D, with a contrast pass; no palette value hardcoded in a component.
- The inset-sidebar shell, adaptive panes for wide, medium and narrow windows, command palette, keyboard routing, native macOS menus, accessibility primitives, and fixture screens for visual iteration.

## Carried from the daemon phases

Each of these makes the GUI the second consumer of something only the CLI or the TUI uses today, so each is settled here rather than reinvented in `desktop/`:

- The connect, bootstrap, call and settle sequence lives in `src/main.rs` (`daemon_connection`, `operation_session`, `run_admin_operation`, `settle`); it belongs in an `mp-client` session helper.
- The per-call daemon timeout (`DAEMON_TIMEOUT`) lives in `src/main.rs` rather than in `mp-client`.
- `mp_client::StateTracker` treats any revision above `watermark + 1` as a gap, which is wrong on a coalesced stream; the TUI sidesteps it by applying the watermark itself, and the GUI would be the first client to rely on the tracker.
- `Outbound::rebootstrap` drops lifecycle events (`config.changed`, `daemon.shutting_down`) at or below the snapshot revision; keeping them needs a protocol decision.
- The TUI announces itself as `ClientKind::Cli`, so the daemon cannot tell clients apart, which matters for the last-client-exits rule of the undo-send hold once a GUI joins.

## Exit gate

- The shell renders every responsive layout from fixtures.
- No component bypasses the semantic tokens.
- Keyboard and screen-reader focus order pass.
- Connection and resync states are testable without a live mail server.
