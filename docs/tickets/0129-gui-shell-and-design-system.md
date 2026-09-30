---
id: 0129
title: M1 of the native GUI, the read-only shell and the design system
type: feature
priority: next
status: open
created: 2026-09-25
---

Second ticket of the [native GUI plan](../plans/native-gui.md), milestone "M1: read-only shell", designed in the sections "The GUI is a third daemon client", "Visual system", "Layout" and "Interaction".
The read slices of #0131 land in the same milestone.
#0128, the M0 spike, closed on 2026-09-30; its numbers and the reader-route decisions are in [docs/baselines/gui-spike-m0.md](../baselines/gui-spike-m0.md) and the plan.

## Work

- Record the dependency due diligence #0128 did not write under `docs/baselines/decisions/`, starting from the stack the spike ran, and install nothing before Sylvain approves it.
- Scaffold `clients/desktop/` with Tauri 2, React, TypeScript, Vite, Tailwind and the selected shadcn components, using the approved dependencies.
- Verify link handling against an intercepted-URL log, never by opening real browser tabs; the opener plugin is called only behind an explicit user click in a shipped build, and automated scenarios stub it.
- The Tauri Rust layer talks to the daemon through `mp-client` only, announcing `ClientKind::Gui`, and holds one connection and one ordered subscription.
- Generate the TypeScript protocol types from `mp-protocol`; the `schemars` question #0119 deferred reopens here, with dependency due diligence before adoption.
- Generate key help and command-palette data from the same `KEYMAP` source as `mp dump-keys --json`.
- Connection, handshake, bootstrap, reconnect, resync and version-mismatch restart screens.
- Dark semantic tokens from the inverse of Basenord Palette D, with a contrast pass; no palette value hardcoded in a component.
- The inset-sidebar shell, adaptive panes for wide, medium and narrow windows, command palette, keyboard routing, native macOS menus, accessibility primitives, and fixture screens for visual iteration.

## Carried from the daemon phases

Each of these makes the GUI the second consumer of something only the CLI or the TUI uses today, so each is settled here rather than reinvented in `clients/desktop/`:

- The connect, bootstrap, call and settle sequence is `mp_client::operation` (`Connection::open`, `subscribe_within`, `run_operation`, `settle`, `await_operation`); the socket path, the on-demand start and the exit-4 diagnostic stay the binary's (`src/daemon/client.rs`).
- The per-call timeout is `mp_client::Connection::call_within` and `Session::call_within` (default 30 s on a session); the budget per method is the caller's, and the CLI's are `DAEMON_TIMEOUT` and its two 300 s siblings in `src/main.rs`.
- `mp_client::StateTracker` applies any revision above the watermark and poisons only on `state.resync_required`, which is what a coalesced stream needs; the TUI watermarks through it, so the GUI inherits a tracker already in use.
- `Outbound::rebootstrap` drops lifecycle events (`config.changed`, `daemon.shutting_down`) at or below the snapshot revision; keeping them needs a protocol decision.
- The TUI announces itself as `ClientKind::Cli`, so the daemon cannot tell clients apart, which matters for the last-client-exits rule of the undo-send hold once a GUI joins.

## Landed

The read-only shell landed as `clients/desktop/` on the `gui-m1` branch on 2026-09-30, in the commits from `40db682c` on, each tagged `(#0129)`.
The plan's M1 section lists what it covers, and `clients/desktop/docs/` documents the Rust layer, the shell, the reader and the design tokens.
The desktop client runs 79 vitest tests (`pnpm test`) and 57 Rust tests (`cargo test` in `src-tauri`), plus one ignored test against a live daemon.

The TypeScript protocol types are generated with ts-rs 12 into `clients/desktop/src/protocol/generated/`: the wire types from `mp-protocol` behind its `ts` feature, and the Tauri layer's result, argument and event types into `gui/` under `cfg(test)`.
`pnpm gen:types` regenerates them, and a test on each side fails when the committed files are stale.
`schemars` is not adopted: ts-rs reads the serde attributes directly, and no consumer needs JSON Schema.
The hand-written set had two wire mismatches, both in `ServerSearchHit`: `message_id` is nullable and `body_text` is never null.

## Open

The ticket stays open for the first two items; the rest are known limits of M1.

- The three guard cases that need a real click inside the reader frame (a plain link, a `target=_blank` link, a form submit) are unverified; the manual steps are in `clients/desktop/docs/reader.md`.
- The live launchd check below is not taken.
- `mp dump-keys --json` carries no action ids, so the palette matches keymap rows by their description; an `id` per row in the dump would replace that match.
- App keys stop at the cross-origin reader frame: with focus in a message body, no app key works until a click returns focus to the app.
- List windowing is off, so every row of a mailbox is mounted.
- The performance targets have not been measured on the M1 build.
- The light theme stays deferred.

## Deferred follow-ups for the client crates

The M1 reviews found these, and none of them blocks the shell:

- The socket, runtime and log path derivation moves into `mp-core`, so `clients/desktop/src-tauri/src/paths.rs` stops repeating the binary's.
- A fallible `Connector::open` in `mp-client`: it answers a `Connection` or ends the process today, so the desktop layer runs its own connect sequence before it hands `Session::connect` a connector.
- `Serialize` on `mp_client::queries::SearchHit`, so the desktop layer can hand a hit to the frontend without its own copy of the type.
- An `mp_protocol` type for the `message.get` record, which the desktop layer decodes by hand today.
- The daemon's meta-CSP strip regex in `crates/mp-core/src/parse.rs` (line 143) misses a meta whose `content` comes before its `http-equiv`; the desktop reader is safe because it always sends its own header.
- Drop the unused `tauri-plugin-shell` crate and `@tauri-apps/plugin-shell` package, and the capability's `opener:allow-open-url` permission for the frontend, since `open_external` calls the opener from Rust.

## Owner action carried here

Carried from #0128, which did not take it: the live launchd check from #0125 is NOT TAKEN and needs the Mac: `mp daemon install-service`, log out and back in, `mp daemon status`, `mp daemon uninstall-service`.
Two questions ride on it: whether `bootstrap` refuses a label it has already loaded, and what `current_exe()` resolves to under a Homebrew `mp`.

## Exit gate

- The shell renders every responsive layout from fixtures.
- No component bypasses the semantic tokens.
- Keyboard and screen-reader focus order pass.
- Connection and resync states are testable without a live mail server.
