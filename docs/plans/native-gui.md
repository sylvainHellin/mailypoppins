# Native GUI plan

## Status

The daemon the GUI needs has shipped.
Phases 0 to 6 of the daemon migration landed as tickets #0118 to #0126 in release 0.10.0: one daemon owns every store, network session and durable operation, and the CLI and the TUI are its clients.
On 2026-09-29 and 2026-09-30 the client kernel moved from the TUI into `crates/mp-client`, and the daemon gained the `message.html` query for a webview reader.
The GUI itself is open, as tickets #0129 to #0132.
M0, the risk spike (#0128), closed on 2026-09-30 with its numbers in [gui-spike-m0.md](../baselines/gui-spike-m0.md).
M1, the read-only shell (#0129), landed on the `gui-m1` branch on 2026-09-30 as `clients/desktop/`.
#0129 stays open for the live launchd check carried from #0128.
M2, mutations with the undo hold (#0131), landed on the `gui-m2` branch on 2026-09-30.
M3, compose through the external editor (#0131), landed on the `gui-m3` branch on 2026-09-30.
M4, calendar, contacts, signatures and config (#0131), landed on the `gui-m4` branch on 2026-09-30; #0131 stays open until M2 to M4 have run in a real window and every parity row that ships in part has a path or a recorded deferral.
#0136, the feedback after M4, landed on `main` on 2026-10-01.
M5, embedded Neovim (#0130), landed on `main` on 2026-10-01, was verified by hand on the Mac the same day, and is done; #0137, the feedback after M5, followed the same day.
M6, distribution and release (#0132), is what remains.
The work needs a macOS host, since the first GUI release is macOS-only and the Tauri toolchain, signing and a real Neovim under Finder cannot be exercised on the headless Linux server.

The wire contract is [daemon-protocol.md](../daemon-protocol.md), the crate shape is [architecture.md](../architecture.md), and the capability list the GUI has to cover is [parity-matrix.md](../parity-matrix.md).

## Goal

Ship a macOS GUI using Tauri 2, React, shadcn/ui, the approved Basenord palette in dark and light, and later a real embedded Neovim process for draft composition.
The GUI provides every user-facing capability present in the TUI, as the parity matrix classifies it.
Capabilities classified as CLI automation, diagnostics and maintenance, daemon administration, or migration-only carry no GUI-parity obligation.
A capability classified as GUI parity may be deferred only when a settled decision names it and `BACKLOG.md` records the deferral.

## Settled decisions

### Stack

- A Tauri 2 application with a React and TypeScript frontend built with Vite.
- shadcn/ui components on Base UI primitives (the shadcn default since mid-2026).
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

The GUI has a dark and a light palette, dark by default, and all styling uses semantic tokens; the dark palette is the dark inverse of Basenord Palette D:

- Prussian blue `#0C1B33` as the outer canvas and dominant background.
- Cream `#F4F1E8` as the primary foreground.
- Ocean blue `#2E86AB` and its accessible UI variants for selection, focus, progress, links, and framing.
- Pumpkin `#FF6700` only for primary actions, warnings that merit attention, charts, and deltas.
- Inter for UI and body text.
- Lucide outline icons at stroke weight 1.75.
- The approved shadcn radius and inset-shell geometry.

Card, popover, muted, input, border, sidebar, destructive, and focus tokens are derived through a dedicated contrast pass.
No component hardcodes a palette value.
The light palette derives from Palette D itself, cream canvas and Prussian text, and redeclares every token but the reader canvas, which stays white in both (#0136).
The theme is a desktop setting, dark, light or system, in Settings and the palette, and system follows the operating system's appearance.
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

- A terminal editor such as Neovim runs each composition session as one real process in a PTY-backed terminal over the reader pane, with the user's configuration and plugins (M5, #0130).
- A GUI editor named in the setting or the environment opens the draft file in its own window instead, as M3 did.
- Exiting the editor preserves the draft, and draft deletion requires an explicit discard action through `draft.discard`.

## Performance targets

The GUI has to feel snappy, and these numbers make that testable on Sylvain's Mac.
Each target carries the M0 baseline, measured on 2026-09-30 with the spike's release build on macOS 26.6.2 ([gui-spike-m0.md](../baselines/gui-spike-m0.md)):

- Cold start to a painted message list takes under 1 s, and a warm start under 500 ms.
  M0 painted the list at 364 ms median over five back-to-back launches, and at 531 ms on the first launch after a build.
- Keyboard navigation in the list and between panes responds within one frame, 16 ms.
  M0 measured 0 ms median and 2 ms p90 from keydown to the next animation frame on a 500-row list.
- A plain-text message opens in under 100 ms and an HTML message in under 250 ms, excluding the first webview warm-up.
  M0 opened plain text in 9 to 27 ms, and a 320 KB HTML message in 13 ms median over the custom scheme and 19 ms over `srcdoc`.
- Local search returns results in under 200 ms on a 50k-message account.
  M0 did not measure it.
- Memory stays under 300 MB of physical footprint, webview processes included, with one account open and the reader showing HTML.
  M0 measured 161 to 181 MB.
- Neovim in the embedded terminal (M5) starts cold in under 300 ms and echoes a keystroke in under 30 ms.
  M0 reached the statusline 219 ms after spawn with Sylvain's own plugin configuration, and echoed a keystroke to the rendered terminal in 4 ms median, 18 ms worst.

Three rules fix how the targets are measured:

- Memory is the physical footprint that `footprint` and Activity Monitor report, summed over the app process and its WebKit processes, never RSS; RSS double-counts the shared WebKit pages and read 368 to 393 MB for the state M0 measured at 161 to 181 MB.
- The Neovim target includes the user's own configuration and plugins, never `nvim --clean`; Sylvain's configuration used 219 ms of the 300, so a heavier plugin set can cross it.
- Latency is measured with the window in front, because WebKit throttles a background or occluded window: in M0 xterm stopped rendering and a 10 s typing run took 125 s.

Every target M0 measured was met, so none was revised.
A target still missed by more than a factor of two at the end of M1 becomes a plan decision recorded here, and never slips silently.

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
A rendition above 8 MiB is refused with `-32004` and `data.fallback = "message.materialise_html"`; the GUI's scheme handler then materialises the file, reads it, and releases the handle with `message.release_handle` as soon as the read is done.
A message without markup is `-32602`, and the reader shows the stored plain text from `message.get` instead.

The rendering rule, decided on 2026-09-30 from the M0 findings:

- The reader loads the rendition as its own document from the custom URI scheme, `mpmsg://localhost/<account>/<row_id>` on macOS.
- A Rust scheme handler answers that URL with the `message.html` string and the daemon's policy above as a `Content-Security-Policy` response header, a constant in the handler and never a value read out of the message, since a header may carry `report-uri` and a sender can hide a meta-looking policy inside the doctype.
- The app CSP stays strict: the scheme document ignores it, whereas a `srcdoc` document inherits it, and M0 lost the `data:` images of a `srcdoc` message under `img-src 'self'`.
- Tauri issue #12767 did not reproduce on macOS 26.6, where the scheme iframe loads, renders and fires `load`; it is retested on macOS 15 before M6.
- The string never goes through `innerHTML` into the application's own document.
- The iframe carries `sandbox="allow-popups"` and no `allow-scripts`, so a `target=_blank` link reaches Rust; without `allow-popups` the sandbox drops it and nothing reaches Rust.
  M0 saw it arrive at `on_navigation`; M1 refuses it at either hook.
- The app CSP's `frame-src` admits `mpmsg:`, `https:` and `http:`; limited to `mpmsg:`, it blocks a clicked link's frame navigation before `on_navigation` sees it, and the link is silently dead.
- `on_navigation` sees subframe navigations as well as the main frame, so its allowlist admits the `mpmsg` scheme; it denies every other URL, logs it as intercepted and shows it in a notice under the message.
- `window.open` reaches `on_new_window`, which denies it and logs it the same way.
- Neither hook opens a browser: only the notice's "Open in browser" button does, on the user's click, for http, https and mailto.
- A `<meta http-equiv="refresh">` does nothing inside the sandbox, and no navigation from it reaches `on_navigation`.

M0 saw the `target=_blank` path with `allow-scripts allow-popups`, because its probe needed a script to click.
M1 drove the plain-link and `window.open` paths from the app document, and the owner then clicked a plain link, a `target=_blank` link and a form submit in the script-free frame by hand on 2026-09-30: all three were refused, recorded with the steps in `clients/desktop/docs/reader.md`.
Admitting `https:` and `http:` in `frame-src` leaves `on_navigation` as the only guard against a web page loading in the reader frame, a risk listed below.

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

This section is the design #0130 implemented; what shipped is in [shell.md](../../clients/desktop/docs/shell.md), "The embedded editor".

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
- The spike runs on the Mac only, since the headless Linux server cannot answer the webview or latency questions.

The Neovim half validates startup from a signed app, user config and plugin loading, Finder `PATH` resolution, PTY input, output, resize, clipboard, Unicode, IME, mouse and colour, keyboard routing between app and terminal, clean child termination on window close and after a crash, a save reaching the draft watcher and coming back through the subscription, and cold-start and keystroke-echo latency against the performance targets.
No spike code is carried into `clients/desktop/`.

M0 closed on 2026-09-30, and [gui-spike-m0.md](../baselines/gui-spike-m0.md) holds its numbers and findings.
It did not test PTY resize, clipboard, IME, mouse, Unicode width, keyboard routing between app and terminal, child cleanup on window close or crash, the draft-watcher round trip, Finder `PATH` resolution or a signed-app launch; those rows move to M5.
It wrote no dependency due-diligence record, which moves to M1.

### M1: read-only shell (#0129, and the read slices of #0131)

- Record the dependency due diligence M0 did not write under `docs/baselines/decisions/`, starting from the stack the spike ran (Tauri 2.12, tauri-plugin-opener 2.7, React 19, Tailwind 4, and for M5 portable-pty 0.9 and xterm.js 6), and install nothing before Sylvain approves it.
- Scaffold `clients/desktop/` with the approved dependencies.
- The connector, handshake, bootstrap, reconnect, resync and version-mismatch restart screens.
- The dark semantic tokens and the inset-sidebar shell with adaptive panes.
- Accounts, the mailbox hierarchy with counts and sync health, the message list, the reader with `message.html` on the custom scheme, and local and server search.
- Link handling is verified against an intercepted-URL log, never by opening real browser tabs on the developer's machine.
  The opener plugin is called only behind an explicit user click in a shipped build, and any automated scenario stubs it.
- Live events and reconnect, restoring presentation state by stable identifiers.
- The command palette, keyboard routing, native menus, and accessibility primitives, with key help generated from the keymap data.
- TypeScript protocol types generated from `mp-protocol`, after dependency due diligence on the generator.
- Fixture screens for visual iteration, testable without a live mail server.

M1 landed on the `gui-m1` branch on 2026-09-30, in the commits from `40db682c` on, each tagged `(#0129)`.
What landed:

- The dependency record, [2026-09-30-tauri-stack.md](../baselines/decisions/2026-09-30-tauri-stack.md), and the scaffold on Tauri 2.12, React 19, Vite, Tailwind 4 and shadcn on Base UI.
- The Rust layer, crate `mp-desktop`: the connector as `ClientKind::Gui` with the on-demand `mp daemon start`, one session and one subscription over the `StateTracker` watermark, re-bootstrap on resync and reconnect, typed commands, the `mpmsg` reader scheme with its own CSP header, the navigation allowlist with its intercepted-URL log, and fixture mode.
- The dark tokens with a computed contrast table, the inset-sidebar shell in wide, medium and narrow layouts, and the connecting, daemon-unavailable, version-mismatch, reconnecting and resync screens.
- Keyboard routing, the command palette with milestone badges and the key help, all read from `src/keymap/keymap.json`, which `pnpm gen:keymap` generates from `mp dump-keys --json`, plus the native menus.
- The reader frame with `sandbox="allow-popups"`, the refused-link notice with an explicit "Open in browser", and local and server search.
- TypeScript types generated with ts-rs 12 into `src/protocol/generated/`, the wire types from `mp-protocol` behind its `ts` feature and the Tauri layer's own types into `gui/`, with `pnpm gen:types` and a stale check on each side; `schemars` is not adopted.
- 79 vitest tests and 57 Rust tests, plus one ignored test against a live daemon.

What is open:

- The live launchd check carried from #0128 is half taken: the log-out and log-in half is pinned for the owner in the #0129 ticket.
- The three guard cases that need a real click inside the frame were verified by hand on 2026-09-30 (`clients/desktop/docs/reader.md`) and are repeated after any change to the reader's guards.
- `mp dump-keys --json` carries no action ids, so the palette matches keymap rows by their description; an `id` per row in the dump would replace that match.
- App keys stop at the cross-origin reader frame: with focus in a message body, no app key works until a click returns focus to the app.
- List windowing is off, so every row of a mailbox is mounted.
- The performance targets have not been measured on the M1 build.

### M2: mutations with the undo hold (#0131)

- Archive, delete, move, read, unread and flag, single and batch, with pending-operation feedback and the rollback event.
- The undo-send countdown and its cancellation, for holds any client armed.

M2 landed on the `gui-m2` branch on 2026-09-30, in the commits after `a127ce3a`, each tagged `(#0131)`.
It closes `MSG-01` to `MSG-09`, `SND-04`, `SYN-06` and the archive half of `LST-09`.
What landed:

- Tauri commands for archive, delete, move, flag, read, draft discard, the hold status, the hold cancel and a quick or full sync; each message and draft command takes a batch, sends one daemon call per row with `settle: false`, fails a row alone on a `-32602` refusal and stops on an error about the whole batch.
- Fixture handlers with a mutation journal, the drain after 1.5 s of quiet, the `rollback`, `rollback:<n>` and `hold` simulations, and a seeded 60 s hold.
- A frontend model of unconfirmed changes, one saved state per axis of a row, with a list generation guard, the pending overlay on every list answer, marks, holds, syncs and activity notices capped at 20.
- The TUI's keys `a`, `d`, `u`, `*`, `M`, `v`, `Ctrl+a`, `ss` and `sS`, Escape for the marks, the desktop's `X` for a notice, and a palette entry for each action.
- The archive and delete confirmation, which mirrors the TUI's, the move picker, row controls, the "N marked" count and the reader toolbar.
- The activity area with live regions, and the send-hold countdown with Cancel; `u` cancels a live hold, as in the TUI.
- Mark read on an explicit open: Enter, a double-click, or Tab into the reader.
- 168 vitest tests and 78 Rust tests, plus two ignored: the live-daemon test and the ts-rs export `pnpm gen:types` runs.

What is open:

- There is no undo for archive, delete or move, since the daemon has none; the rollback notice is the only undo surface.
- App keys still stop at the cross-origin reader frame, and list windowing is still off.
- Search hits streamed from the server are not laid under the pending changes.
- A row moved into a mailbox the window shows appears there only after the drain.

### M3: compose through the external editor (#0131)

- Draft listing, creation, reply, reply-all, forward and existing-draft editing, through `draft.*` and the external editor.
- Recipients, signatures, approval states, validation and explicit discard.
- Attachment add, open, save and remove; a save goes to a directory chosen in a native file picker, sent as an absolute path.
- Send confirmation, approve-and-send, partial-recipient outcomes and outbox state, through `send.*`.

M3 landed on the `gui-m3` branch on 2026-09-30, in the commits `b765c620` to `efe865b5`, each tagged `(#0131)`.
What shipped per unit, the keys, the fixture simulations, the decisions and what stays open are in the ticket's section ["M3 landed"](../tickets/0131-gui-full-parity.md#m3-landed), and the parity rows it closes in [parity-matrix.md](../parity-matrix.md).
The attachment save and attach take a typed path, and since 2026-10-01 a Browse button fills it from the native picker of `tauri-plugin-dialog`.

### M4: calendar, contacts, signatures and config (#0131)

- Calendar agenda, invitation rendering, RSVP, organizer reconciliation, updates and cancellation; iMIP send on a Graph account shows as disabled with its reason.
- Contacts, ranking, copy actions, vCard send, rebuild, and handoff into composition.
- Signature management and per-account defaults.
- Settings, account setup, authentication and secret updates through `config.*`.
- Clipboard copies of the sender address, the message link as the row's `mp://` selector, and the subject, through the Tauri clipboard-manager plugin, with its permission declared in the app capability file.
- Activity, logs and help.

M4 landed on the `gui-m4` branch on 2026-09-30, in the commits `f888afce` to `41e3b73f`, each tagged `(#0131)`.
What shipped per unit, the keys, the fixture simulations, the decisions and what stays open are in the ticket's section ["M4 landed"](../tickets/0131-gui-full-parity.md#m4-landed), and the parity rows it closes in [parity-matrix.md](../parity-matrix.md).
No plugin was installed: the copies go through `navigator.clipboard.writeText` from the key or click handler instead of the clipboard-manager plugin; the dialog plugin came after M5, for M3's path fields.
The signatures and the vCard are client-side over `mp-core`, since the daemon serves no `signature.*` method and no `contact.vcard`.

### M5: embedded Neovim (#0130)

M5 landed on `main` on 2026-10-01 in the commits tagged `(#0130)`, from `5e167a27` to `630b9051`, with 245 Rust and 573 vitest tests, and was verified by hand on the Mac the same day.
The feedback round that followed is #0137.

- Land the PTY and terminal dependencies M0 validated.
- Batch PTY reads in Rust before sending them on the Channel: the spike's release build delivered 1.2 MB/s in sub-KiB chunks against 7.2 MB/s in dev, because each chunk costs Tauri a separate `webview.eval`.
  `Channel` ordering held under a 200k-line burst (`seq 1 200000`, 1.49 MB) in both builds.
- Replace the external-editor handoff with the embedded session described above, for new, reply, reply-all, forward and existing-draft editing.
- Keyboard-focus, resize and crash-recovery tests with a real Neovim process.
- Validate the Neovim rows M0 left untested: resize, clipboard, IME, mouse, Unicode width, keyboard routing, child cleanup on window close and crash, the draft-watcher round trip, Finder `PATH` resolution and a launch from a signed app.

### M6: signing, notarisation and bundling (#0132)

- Signed and notarised macOS app bundles and DMGs in the release workflow; the Developer ID account and CI secrets are #0012.
- The matching `mp` executable bundled inside `Mailypoppins.app`, with a supported way to expose it on `PATH`.
- Desktop notifications for new mail and for finished or failed sends, through the Tauri notification plugin; macOS delivers them only from a signed bundle, so they ship with signing.
- Standalone macOS and Linux CLI archives and the Homebrew installation keep working.
- Clean-install, upgrade, restart after a version mismatch, uninstall and quarantine smoke tests on the signed bundle.
- `docs/release-process.md`, the website and the installation instructions updated.

## Parallel work

GUI work runs beside TUI and daemon work on `main` under one rule.
The GUI never edits `clients/tui`.
It only adds to `mp-protocol` and `mp-client`, additively and with a protocol changelog entry where a wire shape is new.
A conflict in `BACKLOG.md` or `CHANGELOG.md` is resolved by rebasing.
Rust-only GUI work, such as the connector, the pending-operations tracker and the `mp-client` and `mp-protocol` additions, can run on the Linux server in a separate session.

## Tests

- Component tests use protocol fixtures and deterministic state snapshots.
- Interaction tests cover keyboard and mouse paths for every GUI-parity capability.
- Accessibility tests cover focus order, labels, contrast, reduced motion, and keyboard-only operation.
- Responsive tests cover wide, medium, and narrow layouts.
- Reader tests load hostile HTML fixtures and assert that no script runs, no remote request leaves, and no navigation escapes the frame.
- Link handling is verified against an intercepted-URL log, never by opening real browser tabs on the developer's machine; the opener plugin is called only behind an explicit user click in a shipped build, and any automated scenario stubs it.
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

### The reader frame's navigation guard

The app CSP admits `https:` and `http:` in `frame-src` so that link clicks reach `on_navigation`, which leaves `on_navigation` as the only guard against a web page loading in the reader frame.
A hook that allows a URL by mistake, or a navigation WebKit does not report to it, puts a live web page inside the application's webview.
The mitigation is a deny-by-default allowlist and a reader test per link form that asserts the intercepted-URL log and the frame's final URL.
The fallback, if the guard proves leaky, is to rewrite every `<a href>` before rendering and close `frame-src` to `mpmsg:` again.

### Materialised file lifetime

A handle lives ten minutes and pins its blob against the retention sweep until release or expiry.
The GUI releases what it materialised for its own view, and leaves alone a file an external viewer still holds, as the TUI does.

### Packaging two entry points

A bundled and a standalone `mp` could start incompatible daemons.
The socket singleton, the handshake, the explicit restart and the packaging tests covering both launch paths answer it.

## Deferred

- Status-item menu-bar mode.
- Separate composition windows.
- Outbox retry and discard as GUI actions rather than CLI-only operator commands.
- Bundled Neovim distribution.
- Native Windows support.

## Open questions

- Message-list paging against whole-list transfer with row deltas for large mailboxes, where the TUI's whole-list model may not suit a webview list.
- How CI exercises the Tauri build on Linux without webkit2gtk.
