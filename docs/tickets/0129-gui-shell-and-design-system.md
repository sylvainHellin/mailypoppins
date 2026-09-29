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

- The connect, bootstrap, call and settle sequence is `mp_client::operation` (`Connection::open`, `subscribe_within`, `run_operation`, `settle`, `await_operation`); the socket path, the on-demand start and the exit-4 diagnostic stay the binary's (`src/daemon/client.rs`).
- The per-call timeout is `mp_client::Connection::call_within` and `Session::call_within` (default 30 s on a session); the budget per method is the caller's, and the CLI's are `DAEMON_TIMEOUT` and its two 300 s siblings in `src/main.rs`.
- `mp_client::StateTracker` applies any revision above the watermark and poisons only on `state.resync_required`, which is what a coalesced stream needs; the TUI watermarks through it, so the GUI inherits a tracker already in use.
- `Outbound::rebootstrap` drops lifecycle events (`config.changed`, `daemon.shutting_down`) at or below the snapshot revision; keeping them needs a protocol decision.
- The TUI announces itself as `ClientKind::Cli`, so the daemon cannot tell clients apart, which matters for the last-client-exits rule of the undo-send hold once a GUI joins.

## Exit gate

- The shell renders every responsive layout from fixtures.
- No component bypasses the semantic tokens.
- Keyboard and screen-reader focus order pass.
- Connection and resync states are testable without a live mail server.
