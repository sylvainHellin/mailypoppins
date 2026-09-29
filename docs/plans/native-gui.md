# Native GUI plan

## Status

The daemon the GUI needs has shipped.
Phases 0 to 6 of the daemon migration landed as tickets #0118 to #0126 in release 0.10.0: one daemon owns every store, network session and durable operation, and the CLI and the TUI are its clients.
On 2026-09-29 and 2026-09-30 the client kernel moved from the TUI into `crates/mp-client`, and the daemon gained the `message.html` query for a webview reader.
The GUI itself is open, as tickets #0128 to #0132, and the first open ticket is #0128, milestone M0 below.
The work needs a macOS host, since the first GUI release is macOS-only and the Tauri toolchain, signing and a real Neovim under Finder cannot be exercised on the headless Linux server.

The wire contract is [daemon-protocol.md](../daemon-protocol.md), the crate shape is [architecture.md](../architecture.md), and the capability list the GUI has to cover is [parity-matrix.md](../parity-matrix.md).

## Goal

Ship a macOS GUI using Tauri 2, React, shadcn/ui, the approved dark Basenord palette, and later a real embedded Neovim process for draft composition.
The GUI provides every user-facing capability present in the TUI, as the parity matrix classifies it.
Capabilities classified as CLI automation, diagnostics and maintenance, daemon administration, or migration-only carry no GUI-parity obligation.
A capability classified as GUI parity may be deferred only when a settled decision names it and `BACKLOG.md` records the deferral.

## Settled decisions

### Stack

- A Tauri 2 application with a React and TypeScript frontend built with Vite.
- shadcn/ui components on Radix primitives.
- Tailwind using semantic CSS tokens.
- A narrow Tauri command layer backed by `mp-client`.
- Generated TypeScript protocol types and generated keymap data.
- No SSR, server actions, or web-hosted backend assumptions.
- The first GUI release supports macOS; the daemon, CLI and TUI keep supporting macOS and Linux, including WSL.

### The GUI is a third daemon client

- The GUI is a separate Tauri executable, distinct from `mp`, and the bundle includes the matching `mp` executable.
- The Tauri Rust layer depends on the client crates only and has no direct engine access: it never links the root `mailypoppins` crate, which Cargo's resolver enforces.
- React communicates with the Tauri Rust layer through narrow commands and ordered channels.
- The React code cannot open the database, read secrets, or call the mail engine directly.
- Presentation components never invoke raw method strings.
- The GUI process holds one daemon connection and one ordered subscription, and announces itself as `ClientKind::Gui`.
- The GUI never falls back to direct store or engine access when the daemon is unavailable.
- An incompatible daemon shows a blocking daemon-restart action, and no client restarts the daemon without explicit user confirmation.
- Closing the GUI exits the GUI client while leaving the daemon running.
- The first GUI has no status-item menu-bar mode, while standard macOS application menus are present.

### Visual system

The GUI is dark-only, and all styling uses semantic tokens from the dark inverse of Basenord Palette D:

- Prussian blue `#0C1B33` as the outer canvas and dominant background.
- Cream `#F4F1E8` as the primary foreground.
- Ocean blue `#2E86AB` and its accessible UI variants for selection, focus, progress, links, and framing.
- Pumpkin `#FF6700` only for primary actions, warnings that merit attention, charts, and deltas.
- Inter for UI and body text.
- Lucide outline icons at stroke weight 1.75.
- The approved shadcn radius and inset-shell geometry.

Card, popover, muted, input, border, sidebar, destructive, and focus tokens are derived through a dedicated contrast pass.
No component hardcodes a palette value.
The first release exposes one dark theme while retaining shadcn's semantic token structure for a later light theme.
Text, focus rings, disabled states, selection states, error states, and orange accents are checked against WCAG contrast requirements.

### Layout

The shell uses shadcn's `Sidebar` with `variant="inset"` and `SidebarInset`, in an adaptive three-pane layout.

Wide windows show:

- An inset navigation sidebar for accounts, mailboxes, contacts, calendar, settings, activity, and outbox state.
- A center list for messages, conversations, contacts, calendar entries, drafts, or search results.
- A right content pane for message reading, headers, attachments, invitation actions, details, or composition.

Medium windows collapse the sidebar to an icon rail while preserving list and content panes.
Narrow windows navigate between sidebar, list, and content as separate views while retaining keyboard history.
Pane sizes and collapsed state are presentation preferences stored by the GUI rather than daemon domain state.
Composition replaces the reader pane.

### Interaction

- The GUI preserves current TUI keybindings and the command palette alongside mouse controls and native menus.
- GUI key help and the command palette are generated from the same `KEYMAP` source that feeds `mp dump-keys` and the website, never from a second hand-written table.
- Every runnable GUI action appears in the command palette.
- Mouse targets, context menus, toolbar actions and macOS menu items never remove a keyboard path.
- Selection, focus, scroll, and expanded-header state survive query invalidation when their stable resource still exists.
- A fresh bootstrap restores presentation state by stable identifiers and clears references to removed resources.

### Composition

- Until the embedded-Neovim ticket (#0130) lands, compose opens the draft file in the user's external editor, mirroring the TUI.
- Once #0130 lands, each composition session starts one real Neovim process in a PTY-backed terminal, using the user's configuration and plugins.
- Exiting the editor preserves the draft, and draft deletion requires an explicit discard action through `draft.discard`.

## Location and build

The GUI lives at `clients/desktop/`, beside the TUI at `clients/tui`.
The root `Cargo.toml` lists `clients/desktop` in its workspace `exclude` list, so the desktop client carries its own `Cargo.lock` and `cargo test --workspace` never builds Tauri or asks for webkit2gtk.
It links the client crates by path.
`mp` stays free of GUI dependencies, and the Tauri stack never reaches the standalone Linux or headless CLI builds.

## What the client crates give the GUI

Every client links `crates/mp-client`, which carries everything a client needs that is not rendering:

- The session: a thread with its own tokio runtime, the event stream, and reconnect with backoff (`mp_client::session`).
- Typed reads over protocol types (`mp_client::queries`): message lists and row deltas, search hits, mailboxes, bodies, `message_html`, threads, invitations, calendar events, draft listings and paths.
- The `StateTracker` watermark over one daemon instance's revision stream (`mp_client::state`).
- Per-call timeouts through `call_within`, with a 30 s default; a timed-out call closes the connection and a long-lived client reopens.
- The connect, subscribe and settle helper for one-shot sequences (`mp_client::operation`).

`mp-client` links `mp-protocol` alone, and the rest stays with each client.
The GUI reimplements what stays outside `mp-client`:

- The Drafts branch of the message listing and the draft body parse, which need `mp-core` (`DRAFTS_MAILBOX` and `mp_core::draft::parse_email_draft`), so the Tauri layer links `mp-core` as the TUI does.
- A pending-operations tracker, the TUI's `Awaited` table, and the re-query of awaited operations after a re-bootstrap.
- The application of events onto its own view model.
- Display derivations such as date formatting and the `MailboxInfo` projection of a mailbox row.
- A `session::Connector`: the socket path and the on-demand daemon start are the `mp` binary's, so the GUI builds its own, starting the bundled `mp` and showing a screen where the TUI's connector would exit the process.

## Reading HTML bodies

The reader fetches a message's markup with `message.html`, a query answering `{account, row_id, html, bytes}` inline.
The string is byte-identical to the file `message.materialise_html` writes: charset forced to UTF-8, the CSP meta tag prepended, `<meta http-equiv="refresh">` stripped, and `cid:` images inlined as `data:` URIs.
The CSP is `default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'`.
A rendition above 8 MiB is refused with `-32004` and `data.fallback = "message.materialise_html"`, and the GUI then falls back to the file handle and releases it with `message.release_handle` once the view closes.
A message without markup is `-32602`, and the reader shows the stored plain text from `message.get` instead.

The rendering rule:

- The string loads as its own document, either a sandboxed `<iframe srcdoc>` without `allow-scripts` or a custom scheme that also sends the CSP as a response header.
- It never goes through `innerHTML` into the application's own document.
- The GUI intercepts every navigation out of the frame and opens links in the external browser.

### What the CSP does not cover

- `<a href>` navigation, including `target=_blank`, `mailto:`, `file:`, custom schemes and `download`, because CSP has no navigation directive; the host frame has to intercept them.
- Meta refresh, which relies on `strip_meta_refresh` in `mp-core`, rewritten on 2026-09-30 as a tokenizer-style scan.
- `<link rel=dns-prefetch>`, `<link rel=preconnect>` and `<a ping>`, which the policy does not reliably govern.
- `sandbox`, `frame-ancestors` and `report-uri`, which cannot be set from a meta tag, so sandboxing comes from the host frame.
- Inline CSS, which is allowed, so a message can imitate client UI.

The CSP does block scripts, remote images, styles, fonts and media, `object` and `embed`, remote or `data:` iframes, form submission, and `<base>`.

## Events, reconnect and re-bootstrap

The GUI bootstraps with `state.bootstrap`, applies events above the watermark, and calls `state.bootstrap` again on `state.resync_required`, on reconnect, and on a daemon restart.
Lifecycle events do not survive a re-bootstrap, a rule the protocol documents.
After every `state.bootstrap` the GUI re-queries what it still awaits, as the TUI does in `requery_operations`: the undo-send holds come back in the snapshot's `holds`, and each awaited operation comes back through `operation.status`.
After a daemon restart every awaited operation id is dropped, since ids belong to the instance that issued them.

A sleeping GUI costs the daemon bounded memory: its queue overflows at 512 events or 4 MiB, and the GUI answers the resulting `state.resync_required` with a fresh bootstrap.

## Mutations and the undo hold

Archive, delete, move, flag and read go out with `settle: false`, so the GUI never waits on a server per keystroke; the daemon drains once the burst has been quiet for 1.5 s.
A rolled-back move arrives as `mutations.rolled_back` and the GUI surfaces it in the activity area.
Walking the list marks nothing read, and an explicit open marks exactly once, as in the TUI.

The undo-send hold is the daemon's scheduler.
The GUI renders a countdown from the snapshot's `holds` and the four `send.hold_*` events, whichever client armed it, and cancels through `send.cancel_hold`.
The remaining seconds travel on each tick, so the GUI never derives them from its own clock.

## Compose with the external editor

Creating, replying, replying-all, or forwarding asks the daemon to create the canonical draft (`draft.create`, `draft.reply`, `draft.forward`, `draft.create_from_message`).
The GUI then opens the returned path in the user's external editor, as the TUI does, and the daemon's draft watcher publishes each save as a `draft.changed` event.
Approval, validation, preview and sending go through `draft.*` and `send.*`.
Concurrent edits by an agent and an editor keep the editor's own changed-file warning, and the daemon never overwrites external content.

The editor handoff runs from the GUI process, and a Finder launch does not carry the interactive shell's `PATH`.
The GUI therefore resolves the editor through an explicit editor-path setting, then the common Homebrew and system locations, before it reports a setup error.

## Embedded Neovim

This section is the design for #0130, which follows the read-only shell and the mutations.

### Architecture

- One real Neovim process per composition session, started through a native PTY.
- The PTY renders in the webview through xterm.js.
- The Tauri Rust layer owns PTY creation, resize, input, output, termination, and process cleanup.
- PTY output travels on ordered Tauri channels rather than general broadcast events.
- Neovim opens the canonical draft path the daemon returns, with the user's normal configuration and plugins.
- No second Neovim distribution ships in the first release.

### Composition lifecycle

- The GUI replaces the reader pane with the PTY view for the draft it just created.
- Daemon file watching publishes saved changes while Neovim remains open.
- Neovim exit returns the content pane to a draft summary or the previous message context.
- `:q!` discards only unsaved buffer changes and never deletes the canonical draft.
- An explicit GUI discard action confirms and invokes `draft.discard`.
- Navigating away from an active process asks to keep it open, terminate it while preserving the draft, or stay in the editor.
- A process crash preserves the draft and shows the exit status plus a reopen action.
- Window and pane resizing updates the PTY dimensions.
- When the terminal has focus, keys go to Neovim except for a minimal documented set of application-level accelerators.

## Milestones

The milestones run in order.
Each one names the parity-matrix identifiers it closes, and replaces the milestone their `GUI location` column plans today with the surface it built.

### M0: spike (#0128)

- A disposable spike in `spikes/gui-neovim/`, outside the product tree and excluded from the workspace by the `spikes/*` entry.
- Before any install, a web due diligence on the current Tauri 2.x release and its plugin versions, plus the PTY crate, xterm.js and the frontend stack, recorded as a decision record under `docs/baselines/decisions/`; nothing is installed before Sylvain approves it.
- A Tauri 2 scaffold linking `mp-client` by path.
- A connection that runs `state.bootstrap` and lists accounts and mailboxes.
- `message.html` rendered in a sandboxed iframe, with link interception.
- xterm.js on a PTY running a real Neovim against a draft path.
- Timeboxed: a row that fails inside the box is recorded as a finding rather than chased.

The Neovim half validates startup from a signed app, user config and plugin loading, Finder `PATH` resolution, PTY input, output, resize, clipboard, Unicode, IME, mouse and colour, keyboard routing between app and terminal, clean child termination on window close and after a crash, a save reaching the draft watcher and coming back through the subscription, and cold-start and typing latency.
No spike code is carried into `clients/desktop/`.

### M1: read-only shell (#0129, and the read slices of #0131)

- Scaffold `clients/desktop/` with the dependencies M0 recorded.
- The connector, handshake, bootstrap, reconnect, resync and version-mismatch restart screens.
- The dark semantic tokens and the inset-sidebar shell with adaptive panes.
- Accounts, the mailbox hierarchy with counts and sync health, the message list, the reader with `message.html`, and local and server search.
- Live events and reconnect, restoring presentation state by stable identifiers.
- The command palette, keyboard routing, native menus, and accessibility primitives, with key help generated from the keymap data.
- TypeScript protocol types generated from `mp-protocol`, after dependency due diligence on the generator.
- Fixture screens for visual iteration, testable without a live mail server.

### M2: mutations with the undo hold (#0131)

- Archive, delete, move, read, unread and flag, single and batch, with pending-operation feedback and the rollback event.
- The undo-send countdown and its cancellation, for holds any client armed.

### M3: compose through the external editor (#0131)

- Draft listing, creation, reply, reply-all, forward and existing-draft editing, through `draft.*` and the external editor.
- Recipients, signatures, approval states, validation and explicit discard.
- Attachment add, open, save and remove; a save goes to a directory chosen in a native file picker, sent as an absolute path.
- Send confirmation, approve-and-send, partial-recipient outcomes and outbox state, through `send.*`.

### M4: calendar, contacts, signatures and config (#0131)

- Calendar agenda, invitation rendering, RSVP, organizer reconciliation, updates and cancellation; iMIP send on a Graph account shows as disabled with its reason.
- Contacts, ranking, copy actions, vCard send, rebuild, and handoff into composition.
- Signature management and per-account defaults.
- Settings, account setup, authentication and secret updates through `config.*`.
- Activity, logs, notifications and help.

### M5: embedded Neovim (#0130)

- Land the PTY and terminal dependencies M0 validated.
- Replace the external-editor handoff with the embedded session described above, for new, reply, reply-all, forward and existing-draft editing.
- Keyboard-focus, resize and crash-recovery tests with a real Neovim process.

### M6: signing, notarisation and bundling (#0132)

- Signed and notarised macOS app bundles and DMGs in the release workflow; the Developer ID account and CI secrets are #0012.
- The matching `mp` executable bundled inside `Mailypoppins.app`, with a supported way to expose it on `PATH`.
- Standalone macOS and Linux CLI archives and the Homebrew installation keep working.
- Clean-install, upgrade, restart after a version mismatch, uninstall and quarantine smoke tests on the signed bundle.
- `docs/release-process.md`, the website and the installation instructions updated.

## Parallel work

GUI work runs beside TUI and daemon work on `main` under one rule.
The GUI never edits `clients/tui`.
It only adds to `mp-protocol` and `mp-client`, additively and with a protocol changelog entry where a wire shape is new.
A conflict in `BACKLOG.md` or `CHANGELOG.md` is resolved by rebasing.

## Tests

- Component tests use protocol fixtures and deterministic state snapshots.
- Interaction tests cover keyboard and mouse paths for every GUI-parity capability.
- Accessibility tests cover focus order, labels, contrast, reduced motion, and keyboard-only operation.
- Responsive tests cover wide, medium, and narrow layouts.
- Reader tests load hostile HTML fixtures and assert that no script runs, no remote request leaves, and no navigation escapes the frame.
- PTY tests cover Neovim startup, Unicode, resize, save, quit, crash, and an unavailable executable.
- Packaged-app smoke tests use the signed bundle rather than only development mode.
- Cross-client scenarios mutate through the TUI or CLI and observe through the GUI, and the reverse.
- TUI parity stays green throughout.

## Risks

### Hidden direct access

A convenient engine call in the Tauri layer would make the daemon stop being authoritative.
The crate graph refuses it, since no client crate links `mailypoppins`.

### Finder environment

A signed GUI may not find the user's editor, Neovim or shell-dependent paths.
The mitigation is a configurable executable path, known installation paths, and a setup diagnostic.

### Hostile HTML

A message renders inside the GUI's own webview, where a mistake reaches the application rather than a disposable browser tab.
The mitigation is the rendering rule above, the CSP gaps listed with it, and the hostile-fixture tests.

### Materialised file lifetime

A handle lives ten minutes and pins its blob against the retention sweep until release or expiry.
The GUI releases what it materialised for its own view, and leaves alone a file an external viewer still holds, as the TUI does.

### Packaging two entry points

A bundled and a standalone `mp` could start incompatible daemons.
The socket singleton, the handshake, the explicit restart and the packaging tests covering both launch paths answer it.

## Deferred

- Light theme using the existing semantic tokens, which is the deferral behind `OBS-07`.
- Status-item menu-bar mode.
- Separate composition windows.
- Outbox retry and discard as GUI actions rather than CLI-only operator commands.
- Bundled Neovim distribution.
- Native Windows support.

## Open questions

- When the light theme deferred behind `OBS-07` lands, and what it needs beyond a second token set.
- Message-list paging against whole-list transfer with row deltas for large mailboxes, where the TUI's whole-list model may not suit a webview list.
- A custom scheme against `srcdoc` for the reader, where the scheme can send the CSP as a header and `srcdoc` needs no protocol handler.
- How CI exercises the Tauri build on Linux without webkit2gtk.
