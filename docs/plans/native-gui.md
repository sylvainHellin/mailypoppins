# Daemon-first architecture and native GUI plan

## Status

Phases 0 to 6 have shipped, as tickets #0118 to #0125 plus the TUI crate move #0126, and landed on `main` with release 0.10.0.
Their executable work breakdown is `.agents/workflow/native-gui-daemon/plan.md` (local, not versioned), and each ticket records its gate evidence and deviations.
The GUI half is open: Phase 1b is #0128, Phase 7 is #0129, Phase 8 is #0130, Phase 9 is #0131 and Phase 10 is #0132.
It needs a macOS host, since the first GUI release is macOS-only and the Tauri toolchain, signing and a real Neovim under Finder cannot be exercised on the headless Linux server.
Where this plan and a shipped ticket disagree about Phases 0 to 6, the ticket describes what was built.

## Goal

Turn Mailypoppins into a daemon-owned application with CLI, TUI, and Tauri clients that share one domain API.

Preserve every current user-facing capability while making the TUI the reference client before the GUI is built.

Ship a macOS GUI using Tauri 2, React, shadcn/ui, the approved dark Basenord palette, and a real embedded Neovim process for draft composition.

## Scope boundaries

- This plan changes process ownership and client boundaries before adding the GUI.
- The daemon becomes the only owner of stores, network sessions, sync, search, mutations, sending, configuration, secrets, and runtime state.
- The one deliberate exception to that ownership is the filesystem boundary below.
- Canonical draft Markdown files stay unrestricted and remain directly editable by people, agents, Neovim, and other tools, with no lease and no daemon-imposed permission change.
- `config.toml` and signature files fall under the same boundary as watched user-editable inputs.
- Every other domain artifact, including the SQLite stores, blob stores, secrets, outbox, and pending-operation queues, is daemon-private after cutover.
- Clients contain presentation state, formatting, keyboard handling, native integration, and IPC code.
- Clients never fall back to direct store or engine access when the daemon is unavailable.
- The first GUI release supports macOS.
- The daemon, CLI, and TUI continue supporting macOS and Linux, including WSL.
- Native Windows support remains outside scope.
- The implementation preserves current data formats and account directories unless a phase explicitly identifies a required change.
- Protocol migrations before v1.0 may still use hard cutovers under the existing project invariant.

## Settled decisions

- The daemon migration lands first.
- The TUI is converted into the first complete daemon client.
- GUI work begins only after the daemon-backed TUI passes the current behavior-parity gate.
- Disposable feasibility spikes in Phase 1b are exempt from that ordering because they ship no product code and are deleted or rewritten before Phase 7.
- The local protocol is versioned JSON-RPC 2.0 over a Unix-domain socket.
- A benchmark compares representative direct library calls with socket RPC before the transport is locked.
- The transport gate is a same-machine A/B rather than an absolute number, because the #0108 baseline was waived and no recorded figure exists to compare against.
- The A/B takes N cursor-move samples on a binary built from the freeze tag and the same N on the daemon binary, on one machine, under the pinned run conditions of `docs/tickets/0108-coalesce-key-events.md`.
- JSON-RPC remains the transport while the p95 delta between the two runs stays at or below 5 ms.
- The Phase 0 measurement is a committed artifact rather than a number quoted in a report, and it is the only thing the parity gate compares against.
- Phase 0 adds a preview-query timing span so the preview cost is separable from the rest of the paint before any round trip is added to it.
- If the A/B fails, the client-side cache-and-prefetch contingency in `docs/plans/preview-latency.md` becomes required work rather than an option, and lands before the parity gate.
- The daemon runs from the existing `mp` executable, so CLI, TUI, and daemon are one installed binary in three process roles.
- `mp daemon run` is the foreground server command.
- `mp daemon start` launches a detached background process and returns after readiness is confirmed.
- `mp daemon status`, `stop`, and `restart` complete the explicit lifecycle surface.
- Start mode defaults to on-demand automatic startup, with an optional login-start mode.
- The daemon persists after every client exits.
- Concurrent first clients share a startup lock so only one daemon is created.
- The daemon binds the socket and answers `initialize` before account runtimes are ready, so connecting never waits on a store open.
- Per-account readiness is a first-class snapshot field with the states `opening`, `ready`, and `blocked`, published by event as it changes, mirroring the `AccountState::opening` flag the TUI already carries.
- `state.bootstrap` returns zeroed mailbox counts and empty draft summaries for an `opening` account, which is exactly what `App::new` presents today, and fills them by event.
- Every connection starts with an application-version, protocol-version, and capability handshake.
- Daemon identity is the pair of data directory and config directory, since `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` are independent overrides and either one alone selects a different daemon.
- A handshake whose pair does not match the daemon's own pair is refused with a typed error naming both directories, never served.
- An incompatible GUI shows a blocking daemon-restart action.
- An incompatible CLI prints the equivalent command and exits with a stable nonzero code.
- Version mismatch never enables direct-engine fallback.
- No client restarts the daemon without explicit user confirmation.
- Account runtimes stay behind an explicit opt-in until Phase 5, so a daemon under test holds no engine lock and cannot contend with the legacy TUI, a cron `mp sync`, or an older installed `mp` over the same store.
- An `mp` binary installed out of band, from another prefix or an older release, is out of scope for the exclusion this plan builds on.
- The freeze tag `pre-daemon` is the oracle every parity comparison runs against, following the `pre-dal-nuke` precedent recorded at `BACKLOG.md:20`; it points at `f8af44b` and moves only if a further fix lands on `main` before Phase 2.
- `main` is merged into the branch at every phase boundary and at least fortnightly, so the branch never accumulates a merge the parity gate has not seen.
- The commit that first creates `crates/` also moves CI and `AGENTS.md` off bare `cargo test`, because root-package selection would silently stop running the 367 `#[test]` functions under `src/tui/`.
- The daemon loads configuration and secrets exclusively.
- Configuration reload is atomic.
- Invalid configuration leaves the previous valid configuration active and publishes a diagnostic.
- A first run with no `config.toml` starts the daemon with zero accounts and serves the `config.*` family in that state, because there is no previous valid configuration to fall back on.
- `mp config init` and `mp config add-account` therefore run as ordinary daemon clients rather than joining the no-daemon list.
- `state.bootstrap` atomically registers a subscription and returns a canonical snapshot at revision R.
- Events committed after R are queued and delivered in revision order.
- Slow clients have bounded outbound queues.
- Repeated invalidations are coalesced before a queue is declared stale.
- Queue overflow produces `state.resync_required`, after which the client performs a fresh bootstrap.
- Reconnection and daemon restart also require a fresh bootstrap.
- The client absolutises every user-supplied path against its own working directory before the RPC, so the daemon never resolves a relative path and its own working directory carries no meaning; revisit if Phase 1a evidence contradicts.
- Drafts remain unrestricted files without edit leases.
- Draft changes are watched, validated, indexed, and broadcast by the daemon.
- Invalid drafts remain editable and cannot be sent until valid.
- The undo-send hold moves from the TUI into the daemon so every client observes one countdown and can cancel it.
- That move is deferred past the parity gate to Phase 6, because it is GUI-motivated and the CLI plus one TUI are the only observers before then.
- When the last client exits mid-hold the daemon cancels the hold and leaves the draft approved, which matches today's behaviour where quitting during a hold is refused and killing the TUI sends nothing; revisit if Phase 1a evidence contradicts.
- The GUI provides every user-facing capability present in the TUI.
- Capabilities classified as CLI automation, diagnostics and maintenance, daemon administration, or migration-only carry no GUI-parity obligation.
- A capability classified as GUI parity may be deferred only when a settled decision names it and `BACKLOG.md` records the deferral.
- The GUI is a separate Tauri executable, distinct from `mp`.
- The GUI bundle includes the matching `mp` executable.
- Standalone CLI packages remain available for macOS and Linux.
- Closing the GUI exits the GUI client while leaving the daemon running.
- The first GUI has no status-item menu-bar mode, while standard macOS application menus are present.
- The first GUI is dark-only and all styling uses semantic tokens from the Basenord palette.
- The main view uses an adaptive three-pane layout.
- Composition replaces the reader pane.
- The GUI preserves current TUI keybindings and the command palette alongside mouse controls and native menus.
- GUI key help and the command palette are generated from the same `KEYMAP` source that feeds `mp dump-keys` and the website, never from a second hand-written table.
- Each composition session starts one real Neovim process in a PTY-backed terminal.
- Neovim uses the user's configuration and plugins.
- Exiting Neovim preserves the draft.
- Draft deletion requires an explicit discard action.

## Current architecture constraints

- Received mail is a disposable per-account SQLite and blob cache whose remote server remains authoritative.
- Draft Markdown files are the only local user-authored truth.
- The durable outbox is preserved across store rebuilds.
- Pending message mutations are durable local operations that reconcile with the remote server.
- The per-account engine lock on `<account_dir>/store.lock` now has two production acquisitions: `drain_account` (`src/pending_ops.rs:567`) around one mutation-queue drain, and `drain_guarded_at` (`src/outbox.rs:1422`) around an outbox drain, reached through `drain_guarded` (`src/outbox.rs:1401`) from `send::drain_account` (`src/send.rs:2231`).
- `resume_outbox` (`src/send.rs:2262`) therefore runs under the lock through the `drain_account` call at its tail, which is what #0116 added after six messages reached the server up to six times each.
- A refused outbox drain returns `Ok(None)`, does nothing, and opens no IMAP session, while the holder sweeps up to four more times against a clock it re-reads per sweep so it files the row its refused peer left behind (#0116).
- `run_and_settle` (`src/pending_ops.rs:614`), the sync body (`src/sync/engine.rs`), and ingest (`src/ingest.rs`) still take no lock, so two processes can still ingest one mailbox concurrently.
- The daemon holding that lock for an account runtime's lifetime is therefore still a new exclusion property, though it now inherits exclusion on both drains rather than on one.
- A sync tick is head drain, sync body, tail drain, sequenced by `run_tick_with_drains` (`src/sync/tick.rs:25`), and the tail runs on the `Err` path too because the ticks that fail are the long ones (#0114).
- Each drain is the outbox first and the mutation queue second, in `drain_queues` (`src/tui/helpers.rs:339`) for the TUI IMAP path and `drain_queues_cli` (`src/main.rs:1322`) for `mp sync`, while the TUI Graph path drains the mutation queue only (`src/tui/helpers.rs:353`).
- TUI ticks pass `imap_config.body_fetch_deadline()` (`src/config.rs:493`) into the sync body and `mp sync` passes `None`, because the pass a user runs to make the store converge is the one pass that must not stop early (#0113).
- The TUI paints its shell before any store opens, and each first open pays a `PRAGMA integrity_check` of roughly 240 ms that was about 1.2 s serially before #0003 (`src/tui/mod.rs:121`, `src/tui/app/types.rs:731`).
- Every store call opens its own SQLite connection (`Store::open`, `src/store/mod.rs:90`), so a preview read and a background list load already overlap under WAL.
- `list_mailbox` (`src/store/read.rs:178`) materializes a whole mailbox with no limit, on every mailbox switch and after every sync.
- `mp save` defaults its output directory to the current directory (`src/main.rs:301`), and relative `attachments:` entries resolve against the process working directory in `resolve_attachment_paths` (`src/send.rs:1902`).
- `MAILYPOPPINS_CONFIG_DIR` (`src/config.rs:606`) and `MAILYPOPPINS_DATA_DIR` (`src/config.rs:1099`) are independent overrides, and the integration tests point both at temporary directories.
- `migrate_legacy_config_dir` (`src/config.rs:668`, called from `src/main.rs:1672`) is a fourth migration surface beside the cutover, signature, and schema migrations.
- Quitting the TUI during an undo-send hold is refused, and killing the process sends nothing (`src/tui/app/types.rs:2441`).
- The only continuous watcher today is INBOX-only, through `imap_watch` in `src/tui/helpers.rs`.
- The drafts index is refreshed by a one-second fingerprint poll rather than a `notify` watcher, a dependency `src/tui/mod.rs:35` defers deliberately.
- The current TUI calls library functions and stores directly from its state and action layers.
- The current CLI executes domain operations directly in its own process.
- Existing sync backends include IMAP and Microsoft Graph.
- Existing send paths include SMTP and Microsoft Graph, while iMIP invitation send is SMTP-only.
- No daemon, server mode, or IPC surface exists today, so the protocol work is greenfield.
- Current tests cover the CLI help surface, domain behavior, snapshots, and TUI golden frames.
- The TUI event loop drains queued terminal events into the model and paints once per batch, bounded by a batch cap and a 50 ms budget.
- The drain stops when the app is no longer running or an action hands the terminal to `$EDITOR` (#0108).
- A cursor move in the message list now costs one store open, one indexed lookup, one blob read, and one Markdown wrap, after #0109, #0110, and #0111 removed the inline-image read, the mark-read write, and the raw-RFC822 HTML parse from that path.
- The #0108 preview measurement was waived, so no recorded baseline exists and Phase 0 has to create one before anything is compared against it.
- The cache-and-prefetch contingency in `docs/plans/preview-latency.md` was closed as unnecessary, and the daemon migration is the one event that could reopen it.
- `cargo install --path .` remains mandatory after every Rust code change.

## Target process model

### Daemon process

The daemon is one long-lived `mp daemon run` process per operating-system user and data directory.

It owns:

- Loaded and validated configuration.
- Secrets and OAuth token access.
- Per-account SQLite stores and blob stores.
- Per-account runtime state and sync health.
- IMAP pools, IMAP IDLE watchers, Graph pollers, and Graph delta cursors.
- Pending-operation drainers and durable outbox recovery.
- Search execution and result materialization.
- Message mutation serialization.
- Send scheduling, including the undo-send hold and its countdown.
- Draft and signature file watchers.
- Notification decisions and operation progress.
- Canonical revision assignment and client subscriptions.
- Structured logs and diagnostics.

The daemon acquires every configured account's engine lock for the lifetime of the account runtime, from Phase 5 onwards.

Until then account runtimes sit behind an explicit opt-in flag and the daemon acquires nothing.

That gating is not caution for its own sake: the lock covers the two drains and not the sync body, so during Phases 3 and 4 it would not stop the legacy TUI, a cron `mp sync`, or an older installed `mp` ingesting into the same store.

Making it a real exclusion mechanism means extending it to the sync ingest path with non-holders refusing rather than proceeding, which Phase 3b carries as its own unit; the outbox half of that work is already done, since #0116 put the outbox drain under the same lock and made a refused drain a no-op.

An account whose lock is held by another process enters a visible blocked state instead of running a second engine.

The migration removes direct client engine ownership once every CLI and TUI path uses RPC.

### `mp` client modes

The same executable dispatches into three process roles:

- `mp daemon run` enters foreground daemon mode without trying to connect to another daemon.
- `mp daemon ...` performs lifecycle administration.
- Normal CLI commands and the TUI ensure that a compatible daemon exists, connect, and operate as clients.

The daemon-start path uses `current_exe()` so an on-demand client launches the same installed build.

### GUI process

`Mailypoppins.app` is a separate Tauri executable distributed with `mp` inside its application bundle.

The Tauri Rust layer depends on the typed client and protocol crates only.

React communicates with the Tauri Rust layer through narrow commands and ordered channels.

The React code cannot open the database, read secrets, or call the mail engine directly.

### Recommended crate boundaries

Keep the root package buildable while adding explicit boundaries incrementally:

- `crates/mp-protocol` owns wire types, method names, errors, event envelopes, capability identifiers, and generated schemas.
- `crates/mp-client` owns socket connection, daemon discovery, startup, handshake, typed calls, subscriptions, reconnection, and bootstrap recovery.
- The existing `mailypoppins` library owns domain services and the daemon implementation during the migration.
- The TUI is moved behind a crate boundary that depends on `mp-client` and `mp-protocol`, with no dependency on engine modules.
- `desktop/src-tauri` depends on `mp-client` and `mp-protocol`.

CI runs bare `cargo test` (`.github/workflows/ci.yml:19`), which selects the root package only once the root manifest also becomes a workspace root.

The first commit that creates `crates/` therefore changes the test command in the same commit, or the 367 `#[test]` functions under `src/tui/`, the golden frames among them, stop running without anything turning red.

The replacement is `cargo test --workspace` with the Tauri member excluded by name, since that member needs webkit2gtk on `ubuntu-latest`, or `desktop/` kept outside the workspace with its own lockfile.

`AGENTS.md` and the CI workflow change together in that commit, and the commit asserts that the golden-frame count and the TUI test count are unchanged.

The offline suite runs in about 7 s (`AGENTS.md:15`), so the workspace selection stays cheap enough to keep as the default local command, and a figure that drifts far from it is a sign the selection changed rather than that the suite grew.

A later mechanical split may move daemon and engine modules into separate crates if the initial dependency graph cannot enforce the boundary cleanly.

The split must avoid moving all domain files before the protocol seam has tests.

## Runtime files and security

Use the existing platform data root:

- macOS: `~/Library/Application Support/mailypoppins/`.
- Linux: `$XDG_DATA_HOME/mailypoppins/`, defaulting to `~/.local/share/mailypoppins/`.

Add a runtime directory below that root containing:

- `daemon.sock` for the local socket.
- `daemon.start.lock` for concurrent startup exclusion.
- `daemon.pid` as diagnostic metadata rather than the locking primitive.
- An instance metadata file containing the daemon version and startup time if needed by `status`.

The daemon socket and runtime directory are user-only.

The implementation checks socket ownership and permissions before connecting or deleting a stale socket.

The startup lock uses an operating-system advisory lock whose lifetime is tied to its file descriptor.

A stale socket is removed only after a connection attempt proves that no daemon is listening and the current user owns the path.

Peer credentials are verified where the platform exposes them.

No secret appears in handshake payloads, logs, diagnostics, or protocol errors.

## Daemon lifecycle

### Foreground mode

`mp daemon run` initializes logging, takes the singleton lock, runs the legacy config-directory migration (`MIG-04`), loads configuration, binds the socket, and starts serving.

Account runtimes start after the socket is listening and after `initialize` can be answered, so a client never blocks on a store open, and each account reports `opening` until its store is validated.

A missing `config.toml` is not a startup failure: the daemon serves with zero accounts and an available `config.*` family until one is written.

It then serves until a controlled shutdown or process signal.

Foreground logs remain visible while structured logs also go to the existing log directory.

### Detached mode

`mp daemon start` takes the startup lock, checks the socket, launches `mp daemon run` with detached stdio, and waits for a bounded readiness handshake.

A second concurrent starter observes the first daemon becoming ready and returns success without spawning another process.

Startup failure reports the daemon log path and the last structured startup diagnostic.

### Automatic mode

Normal clients first try to connect.

A missing socket invokes the same start routine as `mp daemon start`.

The client performs one bounded connection retry sequence and then reports a diagnostic with the explicit foreground command.

### Login mode

A lifecycle command installs or removes a user-level launchd service on macOS and a systemd user service on Linux.

The service manager runs `mp daemon run` in foreground mode.

Login mode is optional and does not change socket, protocol, or client behavior.

### Shutdown and restart

`stop` requests a graceful shutdown through an administrative RPC, waits for bounded completion, and reports operations that prevented a clean stop.

`restart` performs a controlled stop and starts the invoking executable's daemon build.

The GUI's mismatch screen invokes the same explicit restart operation after user confirmation.

Shutdown stops accepting commands, settles or checkpoints active operations, closes watchers and connections, removes only its own socket, and releases locks.

An active undo-send hold is settled or cancelled during shutdown rather than silently dropped.

When the last client disconnects mid-hold, the daemon cancels the hold and leaves the draft approved, so nothing is sent that no client is watching.

That is the closest match to today, where quitting the TUI during a hold is refused and killing it sends nothing, and the parity gate carries it as a criterion.

## Protocol contract

### Framing and transport

Use a persistent full-duplex Unix-domain socket connection per client.

Encode JSON-RPC 2.0 requests, responses, errors, and notifications with a deterministic framing rule.

Prefer newline-delimited JSON while messages remain path- and metadata-oriented.

Do not send large attachment or message bytes as base64 in normal RPC responses.

Materialize files through daemon operations and return constrained handles or paths with explicit lifetime.

Set request-size, response-size, and write-time limits.

Attachments are not the only response that outgrows a single frame: `mp dump-mailbox --json` builds one string for a whole account, and `mp show --json` carries multi-MB bodies.

Decide in Phase 1a between two options, and record the choice here before Phase 2 implements framing.

One option extends the file-handle path to any response above a byte threshold, so the client reads a temporary file instead of a frame.

The other defines chunked frames with an explicit sequence and terminator, so a large response streams over one connection.

Whichever is chosen, the transport gate gains a throughput criterion beside the latency one: bytes per second for a whole-account `dump-mailbox` and for a 10 MB body, against the same freeze-tag binary.

Malformed input closes only the offending connection.

### Initialization handshake

The first request is `initialize` and contains:

- Client type such as CLI, TUI, or GUI.
- Client application version.
- Supported protocol version range.
- Required and optional capability identifiers.
- Data-directory and config-directory identity, sent as a pair and always, not only where an override is active.

The response contains:

- Daemon application version.
- Selected protocol version.
- Daemon instance identifier.
- Supported capabilities.
- Platform and lifecycle capabilities.
- Current configuration status.

A client cannot issue domain methods before successful initialization.

A client whose directory pair differs from the daemon's is refused with a typed error naming both directories on both sides, because `MAILYPOPPINS_CONFIG_DIR` and `MAILYPOPPINS_DATA_DIR` are independent and a client with only the config override set would otherwise reach a daemon holding different config, secrets, and signatures.

Protocol compatibility is determined by declared ranges and required capabilities rather than string equality alone.

Application-version differences are diagnostic unless they imply protocol or capability incompatibility.

### Method families

Define domain methods rather than database methods.

Initial namespaces should include:

- `state.*` for bootstrap and diagnostics.
- `account.*` for listing, selection metadata, sync health, and account operations.
- `mailbox.*` for listings, counts, and mailbox metadata.
- `message.*` for listing, retrieving, searching, selecting, mutating, attachments, and browser materialization.
- `draft.*` for creation, parsing status, validation, recipient editing, reply, reply-all, forward, approval, discard, and attachment changes.
- `send.*` for immediate send, approved batches, hold countdowns, cancellation, and outbox recovery.
- `sync.*` for quick sync, full sync, progress, and errors.
- `contact.*` for listing, ranking, rebuilding, and statistics.
- `calendar.*` for agenda queries, invitations, RSVP, updates, and cancellations.
- `signature.*` for list, read, create, update, rename, delete, and per-account default selection.
- `config.*` for safe reads, validation, updates, reload, account setup, authentication, and secret writes.
- `operation.*` for long-running operation status and cancellation.
- `diagnostic.*` for logs, health, and support information.
- `daemon.*` for status and graceful lifecycle control.

Every method declares whether it is a query, command, long-running operation, or client-side integration request.

No method accepts SQL, table names, arbitrary store paths, or internal transport objects.

The `message.*` family carries an open decision that Phase 1a settles with the list workload measurement.

Today `list_mailbox` returns a whole mailbox with no limit on every mailbox switch and after every sync, and jump-to-date, the metadata filter, the flagged filter, `G`, and select-all all operate on that whole in-memory list.

One option names daemon methods that make paging viable, which means `message.jump_to_date`, `message.filter`, `message.select_all`, and a total count, so a page is enough to serve every current interaction.

The other keeps whole-list transfer with a compact encoding and row-level delta events, so the client keeps the in-memory list it has today and the daemon sends only what changed.

The measurement that decides it is list refetch bytes and latency at 5 k rows per sync event, which Phase 1a runs as a named workload.

### Typed schemas

Protocol types derive Rust serialization and JSON Schema from one definition.

Generating TypeScript types or bindings from that schema is deferred past the parity gate to Phase 7, where the first TypeScript consumer appears.

Pulling a TypeScript toolchain into the protocol foundation would make every daemon commit depend on it for no reader, so Phase 2 commits the schema and Phase 7 adds the generator over it.

Commit protocol fixtures for every public request, response, error, and event shape.

Keep a protocol changelog from the first shipped version.

## Canonical state and event model

### Revision identity

Every daemon process receives a new opaque `instance_id`.

Every canonical state change receives a strictly increasing `revision` within that instance.

Revisions do not need persistence across daemon restarts because a changed instance identifier forces bootstrap.

Every command response that changes state includes its committed revision and authoritative affected resources.

Every event includes the daemon instance identifier, revision, kind, and typed payload.

Clients ignore duplicate or older revisions and bootstrap when they observe a gap.

### Atomic bootstrap

`state.bootstrap` performs one serialized operation:

1. Register the connection as a subscriber.
2. Capture the canonical state and revision R.
3. Begin queuing events whose revisions exceed R.
4. Return the snapshot, R, instance identifier, and negotiated capabilities.
5. Release queued events in revision order after the response frame.

Tests must force mutations at every boundary in this sequence and prove that the client observes each change exactly once or through an idempotent replacement.

### Snapshot contents

The bootstrap snapshot includes small, shared resources needed to render the shell:

- Account summaries, sync health, and per-account readiness as `opening`, `ready`, or `blocked`.
- Mailbox hierarchy, roles, counts, and badges, zeroed while the account is `opening`.
- Draft summaries and validation state.
- Outbox, active send holds, and pending-operation summaries.
- Contact and calendar view metadata.
- Active operations and diagnostics.

Large or unbounded resources remain queries:

- Message lists and search result pages.
- Message bodies and HTML.
- Attachment bytes.
- Full contact indexes.
- Full calendar ranges.
- Logs.

Client-local focus, selection, scroll position, panel width, and transient dialogs stay outside the canonical snapshot.

A snapshot taken before any account is ready is a valid snapshot, not an error: it is what the TUI renders today in the window between the first paint and the first store open.

Readiness transitions arrive as ordinary events, so a client that bootstrapped against a cold daemon converges without a second bootstrap.

### Event payload rules

Use complete replacement payloads for small resources such as account health, mailbox summaries, draft summaries, and operation status.

Use scoped invalidations for large result sets such as message lists, search results, contacts, and calendar ranges.

Use explicit removal events when stable identifiers disappear.

Avoid field-level patches whose correctness depends on every prior event being present.

A client refetches only visible invalidated queries.

A sync outcome is one of those small resources, and it is no longer a count plus a colour.

The tick result already carries the mailboxes whose body fetch stopped at its deadline and the mailboxes that downloaded the same UID set again (`bodies_truncated` and `non_converging` in `src/sync/mod.rs`), and both change how the outcome must read.

The sync-completed event therefore carries a typed outcome with those two lists, the deferred-prune count, the failed-mutation count, and a severity, rather than a formatted string.

Clients derive their own presentation from it: the TUI's status suffix and its downgrade to Warning, `mp sync`'s stderr line on a zero exit code, and the GUI's equivalent surface.

A client that cannot render one of those states must not present the tick as clean, which is the property the whole detector exists for.

### Backpressure

Each connection has limits on queued event count and encoded bytes.

Coalesce equivalent invalidations by resource and query scope.

Preserve non-coalescible command outcomes and lifecycle events.

When limits are exceeded, discard stale domain events, enqueue `state.resync_required`, and stop sending further domain events until bootstrap succeeds.

If the control notification cannot be delivered, close the connection so reconnect triggers bootstrap.

No slow client can grow daemon memory without bound or delay other clients.

## Daemon internals

### Dispatcher

Extract a transport-independent typed dispatcher from current CLI and TUI orchestration.

Each operation receives an authenticated local client context, validated parameters, and a cancellation handle where appropriate.

The dispatcher returns typed results or domain errors without printing, drawing, opening windows, or prompting.

The socket server is a thin adapter around the dispatcher.

The daemon mode may call the same dispatcher internally for startup and recovery jobs.

### Account runtimes

Create one runtime per configured account, behind the opt-in flag until Phase 5.

A daemon with no configured accounts is a normal state and creates no runtimes at all.

Each runtime owns its account engine lock, store access discipline, backend sessions, watchers, sync scheduler, pending operations, outbox recovery, and sync health.

The unit it schedules is the tick that `run_tick_with_drains` (`src/sync/tick.rs:25`) already defines, not a bare sync call: head drain, sync body, tail drain.

The daemon preserves that structure verbatim, including the two rules it exists for.

The tail drain runs whether the body succeeded or failed, because a failing tick is a long tick and a mutation queued during it must not wait for the next one.

Within a drain the outbox goes first and the mutation queue second, as in `drain_queues` (`src/tui/helpers.rs:339`) and `drain_queues_cli` (`src/main.rs:1322`), while the Graph path drains the mutation queue only since its outbox resume is a no-op.

The daemon is the first caller that can run one tick per account rather than one per client, so it inherits the drain exclusivity of #0116 instead of relying on it, and a client-initiated sync joins the running tick rather than starting a second one.

A daemon tick carries the account's body-fetch deadline, and the explicit recovery sync that `mp sync` performs stays unbounded.

Account commands serialize conflicting operations while independent reads and independent accounts can progress concurrently.

Long database work uses bounded blocking execution so it cannot stall socket IO.

Each runtime keeps a small read-connection pool rather than one parked connection, so a preview read is not queued behind a list load or a sync write; revisit if Phase 1a evidence contradicts.

That is the choice that preserves today's behaviour, where every call opens its own connection and reads already overlap under WAL, and it sidesteps the `Connection: !Sync` problem that `docs/plans/preview-latency.md` records against parking a single `Store`.

The pool is benchmarked at preview p95 with a list load and a sync write in flight, not on an idle daemon, because an idle number would hide the queueing this decision exists to avoid.

Network work uses the shared Tokio runtime and explicit cancellation and timeout policies.

### Long-running operations

Sync, authentication, contact rebuild, calendar rebuild, store maintenance, and batch send return operation identifiers.

Operation state includes queued, running, succeeded, failed, and cancelled states plus structured progress.

CLI commands that are currently blocking subscribe and wait for their operation while preserving existing output and exit behavior.

The TUI and GUI show progress from the same operation events.

Client disconnect does not cancel durable work unless the method explicitly declares client-scoped cancellation.

### Configuration and secrets

The daemon is the only component that parses complete configuration or opens secret backends.

The config directory it reads is part of its identity, alongside the data directory, so two daemons with different config directories are two daemons and neither serves the other's clients.

The legacy config-directory migration runs once at daemon startup, before the first load, and no client performs it.

The CLI may collect interactive answers and submit typed changes without loading the old configuration itself.

The GUI edits settings through typed methods.

Direct user edits to `config.toml` and signature files remain supported as watched inputs.

A watcher parses a candidate configuration into a new immutable snapshot, validates every account, and swaps it only on success.

A valid reload adds, updates, or removes account runtimes in a controlled sequence.

An invalid reload emits a diagnostic with file location and keeps the previous snapshot.

Secret mutation methods accept secret values only on local protected connections and redact all debug representations.

### Draft files

Keep canonical drafts at `<data_dir>/accounts/<account>/drafts/*.md`.

Draft files remain unrestricted so agents and external editors can use ordinary file tools.

A daemon watcher debounces create, write, rename, and delete sequences, then reparses the final file state.

The derived draft index and canonical revision update only after each parse attempt completes.

A parse failure produces an invalid-draft resource containing safe diagnostics and blocks approval or send.

The daemon does not silently restore or overwrite an invalid external edit.

Atomic-save rename patterns used by editors are covered by tests.

Concurrent editor warnings remain the editor's responsibility because there is no advisory or enforced lease.

`mp path` stays the supported selector-to-path edge for agents and external tools, and returns the daemon's canonical path.

### Client-side integrations

The client that requested an action owns presentation integrations such as:

- Opening an editor or embedded PTY.
- Opening a browser.
- Opening or revealing a materialized attachment.
- Clipboard writes.
- Native file pickers.
- Native menus and notifications that require application entitlements.

The daemon prepares domain data and constrained file handles for those actions.

A materialized handle keeps its backing blob alive until the client releases it or the handle expires, so the retention sweep cannot evict a file a client just opened.

The client also owns path resolution, because it is the process with a meaningful working directory.

Every user-supplied path is absolutised against the client's cwd before the RPC leaves the client, so `mp save` with no `-o` sends the absolute form of the directory the user was standing in, and a relative `attachments:` entry in a draft resolves the same way it does today.

That rule is separate from the materialized-handle model: a saved attachment is a permanent user artifact in a client-named directory, not a daemon-owned handle with a lifetime, and the two cases do not share a mechanism.

## CLI migration

Preserve existing command names, options, standard output, standard error, and exit codes unless a separately approved correction is required.

For each command family:

1. Capture current help and behavior fixtures.
2. Add the matching typed daemon operation.
3. Add typed client support.
4. Switch the CLI handler to the client.
5. Run contract and domain tests.
6. Remove the direct engine path for that command.

One-shot commands start the daemon automatically, with two exceptions: daemon lifecycle commands, and commands that read no domain state at all, such as `mp dump-keys`, `mp --help`, `mp --version`, and `mp config path`.

`mp config init` is not one of those exceptions: it starts the daemon like any other command and writes through `config.*`, which is why the daemon must serve with zero accounts.

Machine-readable commands retain stable JSON or NDJSON shapes.

`mp dump-mailbox --json` keeps its deterministic record ordering and its allow-listed field set when its data arrives over RPC instead of a direct store read.

Interactive prompts remain in the CLI process.

Server errors retain typed codes while the CLI maps them to concise human messages and stable exit statuses.

## TUI migration and parity gate

Keep the Elm-style state and rendering layers while replacing every direct store or engine effect with typed client operations.

The TUI owns:

- Focus, cursor, selection, scroll, zoom, overlays, and pending key prefixes.
- Formatting and terminal rendering.
- Terminal colour capability detection.
- Input coalescing, including the pre-draw event drain, its batch and time bounds, its app-running stop condition, and its terminal-suspend stop condition.
- Local optimistic presentation only where it can be reconciled by authoritative revisions.
- Editor suspension and resume.
- Clipboard and browser handling.

The daemon owns every query and mutation result.

Replace background worker variants that execute domain logic with request and subscription results.

Use `state.bootstrap` to initialize account, mailbox, draft, outbox, operation, and health summaries.

Use paged queries for visible lists and bodies once the list-transfer decision in the method-families section lands on paging.

If it lands on whole-list transfer with row-level deltas instead, the TUI keeps its in-memory list and the paging work disappears, so neither shape is implemented before Phase 1a settles the question.

Refresh visible queries when scoped invalidations arrive.

Map daemon errors into the existing persistent error and activity surfaces.

The undo-send hold stays in `src/tui/actions.rs` through the parity gate and moves into the daemon in Phase 6, keeping the current countdown duration, status-line presentation, and cancel key when it does.

The gate still tests today's behaviour of the hold, so the Phase 6 move has an oracle to match.

The TUI parity gate requires:

- Every current TUI action mapped to a daemon method or documented client-only action.
- Every keybinding and command-palette entry still reachable, including the overlay-internal keys that `mp dump-keys` does not list.
- Existing golden frames unchanged except for approved connection and operation-status surfaces.
- Existing editor, browser, attachment, calendar, contact, search, sync, send, and multi-account workflows passing.
- A held `j` or `k` still painting once per drained batch rather than once per repeat, with the drain still stopping before an editor handoff.
- A queued action after `q` never dispatches an editor while the TUI exits.
- Walking the list still marking nothing read, and `Enter`, `e`, and a focus move into the body pane still marking exactly once.
- Preview latency per cursor move within the p95 delta ceiling of the same-machine A/B against the freeze-tag binary, measured under the pinned run conditions in `docs/tickets/0108-coalesce-key-events.md`.
- No imports from store, network, secret, send, sync, or mutation implementation modules in the TUI crate.
- No daemon-unavailable direct fallback.
- Concurrent TUI and CLI operations passing integration tests against one daemon.

Those criteria need oracles that a broken daemon can actually fail, and the artifacts inherited from today do not supply them: the golden frames build `App::default_for_tests()` with hand-assigned rows and open no store, so they pin the render layer only, and `tests/cli_help_snapshot.rs` pins help text.

The gate therefore adds five concrete oracles.

- The existing integration suites `store_ingest_integration`, `store_search_integration`, `dump_mailbox_integration`, `outbox_integration`, `cli_read_surface_integration`, `cli_selector_contract`, `draft_integration`, and `imip_integration` run a second time through an `mp daemon run` instance against a temporary data root, with stdout and exit codes byte-diffed against the freeze-tag binary on the same fixtures.
- Those suites include the concurrency and reset contracts the last ten commits added, `two_racing_drains_append_each_row_exactly_once`, `a_drain_killed_mid_append_leaves_the_row_reclaimable_and_deduped` and `a_first_attempt_costs_no_dedup_search` in `tests/outbox_integration.rs`, and `n_listed_copies_of_one_message_id_get_n_rows`, `a_second_pass_over_the_same_copies_changes_no_row`, `the_unconditional_policy_still_rebinds_onto_a_listed_uid` and `a_reset_pass_maps_n_copies_onto_n_rows` in `tests/store_ingest_integration.rs`, so a daemon that serialises drains differently or reorders a reset pass fails the harness rather than the reader.
- Daemon-backed golden-frame variants sit beside the existing ones, built from a real bootstrap snapshot rather than hand-assigned rows, so a wrong snapshot changes a frame.
- `mp --help` recursively and `mp dump-keys --json` are byte-identical to the Phase 0 capture, from a binary reinstalled in the same run.
- A manual checklist covers the `KeyAction::Manual` keys of `ANO-2`, since no generated artifact reaches them.
- Quitting the last client during an undo-send hold leaves the draft approved and sends nothing, matching today's refusal to quit and today's behaviour when the process is killed.

Two cold-path measurements join the gate because the daemon is where startup cost now lives.

- Cold first paint with no daemon running, against the Phase 0 startup baseline, covering daemon spawn, socket bind, handshake, and the first frame.
- Cold and warm one-shot CLI latency for a representative command, also against the Phase 0 baseline, since a one-shot `mp` now pays for a daemon that may not exist yet.

GUI implementation does not begin before this gate passes.

## Feature inventory and GUI parity matrix

Create a checked-in feature-parity document before API implementation starts.

It extends every entry of the inventory below with the columns the implementation needs, keeping the identifiers stable:

- Required daemon methods, events, and resources.
- Intended GUI location and interaction.
- Unit, protocol, TUI, GUI, and end-to-end validation.
- Implementation and validation status.

Build and maintain it from these primary sources:

- Recursive `mp --help` output and the CLI help snapshots, produced by a binary reinstalled with `cargo install --path .` in the same run.
- `mp dump-keys --json`, generated from the same `KEYMAP` used by the TUI `?` overlay.
- `src/tui/app/keys.rs`, which hand-dispatches overlay-internal keys that never enter `KEYMAP` and therefore never appear in `dump-keys`.
- Command-palette registrations and action enums.
- `CHANGELOG.md` and shipped ticket documentation.
- TUI golden frames and integration tests.
- Existing architecture, feature, and UX audit documents.

Never treat a previously generated dump or a stale `target/` binary as truth, because a keymap change lands in the source before it reaches either.

The generated help and keymap sections are regenerated in CI so future commands cannot bypass the matrix silently.

The GUI consumes the same generated keymap data as the website rather than maintaining a second key table.

### Verified capability inventory

The identifiers below are stable and carry into the checked-in matrix, the phase checklists, and the test names.

Classification vocabulary, one value per capability:

- GUI parity: a user-facing capability that Phase 9 must deliver in the GUI.
- CLI automation: a machine-facing surface whose consumers are scripts, agents, and other tools.
- Diagnostics and maintenance: an operator surface for inspecting or repairing local state.
- Daemon administration: lifecycle, locking, watching, and queue operation of the daemon itself.
- Migration-only: a one-way conversion surface that disappears once every user has run it.

Source anchors name the file and symbol where useful.

Line numbers are retained only where they were checked against the current tree.

#### Accounts, configuration, secrets, and signatures

- `ACC-01` Interactive account setup wizard, GUI parity.
  Entry point `mp config init`, sources `src/main.rs` and `src/config_cmd/init.rs`.
  The wizard writes `config.toml` and one secret in a single pass, so the GUI drives it through `config.*` methods instead of spawning the CLI.
- `ACC-02` Add a further account to an existing configuration, GUI parity.
  Entry point `mp config add-account`, source `src/config_cmd/init.rs`.
- `ACC-03` Show the effective configuration, diagnostics and maintenance.
  Entry point `mp config show`, source `src/config_cmd/show.rs`.
  Output is redacted, and the daemon is the only reader of the underlying file after cutover.
- `ACC-04` Print the configuration file path, diagnostics and maintenance.
  Entry point `mp config path`, source `src/config_cmd/mod.rs`.
  Computes a path only, so it runs without starting a daemon.
- `ACC-05` Store an SMTP or IMAP password in the active secrets backend, GUI parity.
  Entry point `mp config set-password <smtp|imap> [--account]`, sources `src/main.rs` and `src/config_cmd/password.rs`.
  Secret values travel only on the local socket and never appear in logs, diagnostics, or protocol errors.
- `ACC-06` OAuth2 device-code login, GUI parity.
  Entry point `mp config oauth2-login [--account]`, sources `src/config_cmd/oauth2.rs` and `src/oauth2.rs`.
  A long-running operation whose user code and verification URL the client renders while opening the browser as a client-side integration.
- `ACC-07` Reset secrets, wiping the encrypted secrets file and the OAuth token caches before re-prompting, diagnostics and maintenance.
  Entry point `mp config reset-secrets`, source `src/config_cmd/reset.rs`.
  Recovery path after a restore onto a new machine, where the machine-uid derived key no longer decrypts the file.
- `ACC-08` Secrets backend itself, keyring plus a ChaCha20-Poly1305 file keyed through HKDF from the machine uid, daemon administration.
  Sources `src/secrets.rs` and `tests/secrets_integration.rs`.
  Only the daemon opens it once the cutover is complete.
- `ACC-09` Account and signature selection on every command through `-A/--account`, `-s/--signature`, and `--no-signature`, GUI parity.
  Sources the global arguments in `src/main.rs` and `src/signatures.rs`.
  The GUI equivalent is the active-account selector plus a per-composition signature choice.
- `ACC-10` Signature file management: list, read, create, edit in the editor, rename, delete, and per-account default, GUI parity.
  Entry point the TUI `cs` overlay registered at `src/tui/app/keymap.rs:588`, sources `src/signatures.rs` and the default recorded in `state.json`.
  There is no CLI equivalent, so the GUI takes this capability from the TUI, and inline-text signatures are edited through a temporary copy.
- `ACC-11` Signature injection into a draft through the `{{SIGNATURE}}` marker or a spliced block, GUI parity.
  Source `src/signatures.rs`.
  Editing recipients re-splices the block, which is the edge case that makes this its own capability.
- `ACC-12` Multi-account operation, where every domain command resolves an account and several accept `--all-accounts`, GUI parity.
  Account switching preserves per-account list and selection state.

#### Mailbox navigation and view switching

- `MBX-01` List server mailboxes, diagnostics and maintenance.
  Entry point `mp list-mailboxes`, sources `src/main.rs` and `src/imap_client/mod.rs`.
  A live server call, distinct from the mailbox hierarchy the store already holds.
- `MBX-02` Browse and select mailboxes in the sidebar, GUI parity.
  Entry points TUI `j/k` and `Enter`, plus `gm` to focus the sidebar, sources `src/tui/app/keymap.rs:636` and `src/tui/ui/sidebar.rs`.
- `MBX-03` Jump to a mailbox by digit `1` through `9`, GUI parity.
  Source `src/tui/app/keymap.rs:558`.
- `MBX-04` Switch account, GUI parity.
  Entry point TUI `ga`, guarded so it appears only with more than one configured account, source `src/tui/app/keymap.rs:592`.
- `MBX-05` Switch between the Mail, Contacts, and Calendar views, GUI parity.
  Entry points TUI `Space m`, `Space c`, `Space a`, source `src/tui/app/keymap.rs:601`.
- `MBX-06` Cycle pane focus and zoom the focused pane, GUI parity.
  Entry points TUI `Tab`, `Shift+Tab`, `z`, sources `src/tui/app/keymap.rs:559` and `src/tui/app/keymap.rs:569`.
  Purely presentation state, so it stays client-side and never enters the canonical snapshot.
- `MBX-07` Mailbox roles, slugs, sidebar labels, unread counts, and outbox badges, GUI parity.
  Delivered through the bootstrap snapshot rather than a query.

#### Listing, filtering, and search

- `LST-01` List received messages offline, grouped by mailbox, GUI parity.
  Entry point `mp list-messages [--mailbox] [-n]`, source `src/main.rs`.
  The mailbox argument accepts a role, a slug, or the sidebar label, and the default lists every mailbox of the account.
- `LST-02` Navigate a list with per-item movement, top and bottom jumps, and half-page scrolling, GUI parity.
  Entry points TUI `j/k`, `gg/G`, `Ctrl+d`, `Ctrl+u`, sources the EMAIL LIST and BODY keymap sections.
- `LST-03` Jump to a date in the list, GUI parity.
  Entry point TUI `gt`, sources `src/tui/app/keymap.rs:652` and `src/tui/app/jump_date.rs`.
  Accepts relative expressions such as last week alongside absolute dates.
- `LST-04` Filter the current list by metadata, GUI parity.
  Entry point TUI `fm`, source `src/tui/app/keymap.rs:624`.
- `LST-05` Toggle a flagged-only filter, GUI parity.
  Entry point TUI `fF`, source `src/tui/app/keymap.rs:673`.
- `LST-06` Search with one query grammar across every backend, GUI parity.
  Entry point `mp search [query] [--mailbox] [field flags] [-n] [--full]`, sources `src/main.rs` with `SEARCH_LONG_ABOUT`, `src/search.rs`, `src/imap_client/search.rs`.
  The grammar covers `from:`, `to:`, `cc:`, `subject:`, `body:`, `filename:`, `has:attachment`, `before:`, `after:` with `since:` as an alias, quoted phrases, `OR` groups, `in:`, and `message-id:`, and `filename:` resolves only on Gmail, Exchange, or the local index.
- `LST-07` Search the local ranked full-text index across every synced mailbox, GUI parity.
  Entry point `mp search --local`, sources `src/store/search.rs` and `tests/store_search_integration.rs`.
- `LST-08` Merged search in the TUI, where local hits appear immediately and server results merge behind them, GUI parity.
  Entry point TUI `ff`, sources `src/tui/app/keymap.rs:581` and `Action::ServerSearch` in `src/tui/app/types.rs`.
  Deduplication is by Message-ID, so a message found twice appears once.
- `LST-09` Act on a search result without leaving the overlay, GUI parity.
  Entry points open and jump with `Enter`, read-only editor with `e`, copy the Markdown rendition path with `y`, fetch a server-only hit into the store with `f`, plus reply, reply-all, forward, archive, browser, open attachment, and save attachment.
  Source the SERVER SEARCH keymap section.
- `LST-10` Show the conversation a message belongs to, GUI parity.
  Entry point TUI `tt`, sources `src/tui/app/keymap.rs:630` and the thread overlay in `src/tui/ui/overlays.rs`.
- `LST-11` Legacy server fetch with inline filters, diagnostics and maintenance.
  Entry point `mp fetch [--from --to --cc --subject --body --since --before -n --full --mailbox]`, source `src/main.rs`.
  Superseded by `sync` plus `search`, kept because the migration preserves command surfaces, with the deprecation decision deferred to `BACKLOG.md`.
- `LST-12` Dump message envelopes as NDJSON, CLI automation.
  Entry point `mp dump-mailbox --json [--mailbox ...]`, sources `src/dump.rs`, `docs/dump-allow-list.md`, `tests/dump_mailbox_integration.rs`.
  Two runs over an unchanged store are byte-identical, `--json` is required so a later default cannot silently change the format, no filesystem paths appear in the output, and all three properties must survive the move to an RPC data source.
- `LST-13` Coalesce queued input before repainting, so a held navigation key advances the cursor by the whole backlog and paints once, GUI parity.
  Sources `MAX_COALESCED_EVENTS`, `COALESCE_BUDGET`, and the drain in `src/tui/mod.rs`, `poll_pending_event` in `src/tui/event.rs`, and `Action::suspends_terminal` in `src/tui/app/types.rs`.
  Event order is preserved, so leader keys and resizes are unaffected.
  The drain stops when the app is no longer running or an action hands the terminal to `$EDITOR`, whose GUI counterpart is the handoff into the Neovim PTY.
  The GUI obligation is the property rather than the mechanism: holding a navigation key must not queue one full render and one round trip per repeat.

#### Reading and rendering

- `RD-01` Read one received message from the local store while offline, GUI parity.
  Entry point `mp show <selector> [--mailbox]`, sources `src/read_cmd.rs` and `tests/cli_read_surface_integration.rs`.
- `RD-02` Emit one message as a single JSON object with headers, attachments, and body, CLI automation.
  Entry point `mp show --json`, source `src/main.rs`.
- `RD-03` Read HTML-dominant mail in the reader as the plain text ingest flattened out of the markup, GUI parity.
  Source `wrap_and_style_body` in `src/tui/ui/preview.rs`, over the body `parse::html_to_plain` produced at ingest.
  #0111 retired the html2text rich render that #0091 had added, so links, emphasis, tables, and lists arrive as a wrapped block and `b` / `tb` is the styled view.
  The GUI baseline is that same flattened body, and any richer GUI rendering stays derived content rather than an embedded raw remote HTML document.
- `RD-04` Headers pane with Bcc, Reply-To, the attachment marker, and clamped scrolling, GUI parity.
  Sources `src/tui/ui/headers.rs` and the HEADERS keymap section.
- `RD-06` Open a message read-only in `$EDITOR` as a Markdown rendition, GUI parity.
  Entry points TUI `e` and `Enter`, search overlay `e`, source `src/tui/app/keymap.rs:611`.
  The daemon materializes the rendition and the client opens it, which in the GUI is a read-only editor session.
- `RD-07` Copy a message's `mp://` selector to the clipboard, GUI parity.
  Entry point TUI `y`, source `src/tui/app/keymap.rs:618`.
- `RD-08` Copy the Markdown rendition path of a search hit, GUI parity.
  Entry point the search overlay `y`, source the action set in `src/tui/app/types.rs`.

#### Selection and message actions

- `MSG-01` Archive a received message on the server and locally, GUI parity.
  Entry points `mp archive <selector> [--mailbox]` and TUI `a`, sources `src/main.rs` and `src/tui/app/keymap.rs:613`.
- `MSG-02` Delete a received message or a local draft, GUI parity.
  Entry points `mp delete <selector> [--mailbox] [--force]`, `mp delete --sent`, and TUI `d`, source `src/main.rs`.
  `--force` is required to delete an approved draft because that is a queued send, and `--sent` clears every sent draft of the account and takes no selector.
- `MSG-03` Toggle read and unread, GUI parity.
  Entry point TUI `u`, source `src/tui/app/keymap.rs:615`.
- `MSG-04` Toggle the `\Flagged` star, GUI parity.
  Entry point TUI `*`, source `src/tui/app/keymap.rs:616`.
  On a batch, flagging wins whenever any selected message is unflagged.
- `MSG-05` Move a message to another mailbox through a fuzzy picker, GUI parity.
  Entry point TUI `M`, source `Action::MoveToMailbox` in `src/tui/app/types.rs`.
- `MSG-06` Multi-select and batch actions covering read, flag, archive, delete, draft delete, approve, and demote, GUI parity.
  Entry points TUI `v` and `Ctrl+a`, sources `src/tui/app/keymap.rs:653` and the batch actions in `src/tui/app/types.rs`.
- `MSG-07` Confirmation dialogs guarding approve, demote, archive, delete, send, send-approved, and signature deletion, GUI parity.
  Source the confirm variants in `src/tui/app/types.rs`.
- `MSG-08` Mark a message read on an explicit open, GUI parity.
  Source `mark_open_read` in `src/tui/actions.rs`, reached from the received-row branch of `Action::EditCurrent` and from `Action::MarkAsRead`, which `queue_mark_open_read` in `src/tui/app/keys.rs` pushes on a focus move into the body pane.
  #0110 retired the #0087 trigger that fired on every cursor move, so walking the list marks nothing and the GUI must mark on the open rather than on selection.
  The action carries the `MessageRef` the open resolved, so a coalesced key batch marks the row that was opened rather than the row the cursor ended on.
- `MSG-09` Optimistic local mutation state reconciled against the server, GUI parity.
  Sources `src/pending_ops.rs` and `src/ops.rs`.
  The GUI shows the same pending and reconciled states rather than blocking on the network.

#### Drafts, composition, reply, and forward

- `DFT-01` Create a draft from the template and print its selector, GUI parity.
  Entry points `mp new <name>` and TUI `cn`, sources `src/main.rs` and `src/tui/app/keymap.rs:583`.
- `DFT-02` List the account's drafts, optionally filtered by status, GUI parity.
  Entry point `mp list [--status]`, source `src/main.rs`.
- `DFT-03` Validate draft frontmatter for one draft or for every draft of the account, GUI parity.
  Entry point `mp validate [selector]`, source `src/draft.rs`.
  An invalid draft stays editable and cannot be approved or sent.
- `DFT-04` Approve a draft and demote it back to draft status, GUI parity.
  Entry points `mp mark-approved`, `mp mark-draft`, TUI `cA` and `cD`, source `src/main.rs`.
- `DFT-05` Preview a draft as a dry run through a bare selector, GUI parity.
  Entry point `mp <selector>`, source the top-level positional argument in `src/main.rs`.
- `DFT-06` Resolve a draft selector to its filesystem path, CLI automation.
  Entry point `mp path <selector>`, source `src/main.rs`.
  The only selector-to-path edge, and the handle external editors and agents use, so it stays supported under the filesystem boundary.
- `DFT-07` Edit a draft in the editor, GUI parity.
  Entry point `mp edit <selector>`, source `src/main.rs`.
  The GUI equivalent is the embedded Neovim session on the same canonical file.
- `DFT-08` Create a reply or a reply-all draft from a received message, GUI parity.
  Entry points `mp reply <selector> [--all] [--mailbox]`, TUI `r`, `cr`, `ca`, search overlay `r` and `R`, sources `src/main.rs` and `src/tui/app/keymap.rs:626`.
- `DFT-09` Forward a message to new recipients, GUI parity.
  Entry points `mp forward <selector> [--mailbox]`, TUI `cf`, search overlay `w`, source `src/main.rs`.
  The forward carries the original attachments, which the GUI must reproduce rather than dropping.
- `DFT-10` Compose wizard for new and forwarded mail with an inline body field, a signature picker, and a submit chord, GUI parity.
  Sources the compose wizard variants in `src/tui/app/types.rs` and `src/tui/ui/compose.rs`.
- `DFT-11` Edit the recipients of an existing draft, GUI parity.
  Entry point TUI `ce` in the drafts mailbox, source `src/tui/app/keymap.rs:656`.
  Re-splices the signature block, so it is not a plain header edit.
- `DFT-12` Watch draft files and refresh the derived index after an external edit, GUI parity.
  Implicit workflow, daemon-owned after cutover, and the mechanism that keeps the GUI correct while Neovim writes the file.

#### Attachments

- `ATT-01` Open a received message's attachment in the default application, GUI parity.
  Entry points `mp open <selector> [--mailbox]`, TUI `to`, search overlay `o`, source `src/main.rs`.
- `ATT-02` Save a received message's attachments into a client-named directory, GUI parity.
  Entry points `mp save <selector> [-o dir] [--mailbox]`, TUI `ts`, search overlay `O`, sources `src/main.rs:301` for the option and `src/main.rs` for the handler.
  The destination defaults to the current directory, so the client absolutises it against its own cwd and sends an absolute path; the result is a permanent user artifact, not a daemon-owned handle with a lifetime, which is what separates this row from `ATT-01`, `ATT-04`, and `ATT-05`.
- `ATT-03` Attach a file to a draft, GUI parity.
  Entry point TUI `ta` in the drafts mailbox, source `src/tui/app/keymap.rs:659`.
  Appends to the `attachments:` frontmatter list and verifies the path at the prompt, so the GUI file picker applies the same verification.
  A relative entry resolves against the process working directory in `resolve_attachment_paths` (`src/send.rs:1902`), so the same absolutisation rule applies at the prompt, and a draft written by hand with a relative entry keeps resolving where the person who wrote it expects.
- `ATT-04` Open a draft's own attachment, GUI parity.
  Source `src/selector.rs`.
- `ATT-05` Open a message's HTML part in the browser, GUI parity.
  Entry points TUI `tb` and search overlay `b`, source `src/tui/app/keymap.rs:633`.
  A client-side integration over a daemon-materialized file.

#### Sending, outbox, and the undo hold

- `SND-01` Send one approved draft, GUI parity.
  Entry point `mp send <selector> [-y]`, source `src/main.rs`.
- `SND-02` Send every approved draft of one account or of all accounts, GUI parity.
  Entry points `mp send-approved [-y] [--all-accounts]` and TUI `cX`, sources `src/main.rs` and `src/tui/app/keymap.rs:669`.
- `SND-03` Approve and send the current draft with one key, GUI parity.
  Entry point TUI `x`, source `src/tui/app/keymap.rs:573`.
- `SND-04` Undo-send hold with a visible countdown and a cancel key, GUI parity.
  Configured by `email.send_hold_secs` with a 20 second default, source `src/tui/actions.rs:1231`.
  The hold lives inside the TUI process today and `mp send` and `mp send-approved` bypass it, so the migration moves the hold into the daemon, keeps the CLI commands sending immediately, and lets the TUI and GUI observe and cancel the same countdown.
  The move is scheduled for Phase 6, after the parity gate, because it is GUI-motivated and the CLI plus one TUI are the only observers before then.
  When the last client exits mid-hold the daemon cancels the hold and leaves the draft approved, which is what killing the TUI does today.
- `SND-05` Send a calendar invitation, GUI parity.
  Entry point `mp send --invite --to --cc --subject --start --end|--duration --location --description`, sources `src/main.rs` and `src/calendar.rs`.
  Start and end accept local time or RFC3339, duration accepts ISO8601 or the short form, and Microsoft Graph accounts are refused at `src/main.rs`, which the GUI shows as a disabled action with its reason rather than as a late failure.
- `SND-06` Outbox state visibility covering queued, retrying, failed, and partly delivered submissions, GUI parity.
  Source `src/outbox.rs`, surfaced today as a TUI badge and status entry.
- `SND-07` Outbox operator actions, retrying a failed submission and discarding one, daemon administration.
  Entry points `mp outbox list`, `mp outbox retry <id>`, `mp outbox discard <id>`, sources `src/main.rs`, `src/outbox.rs`, `tests/outbox_integration.rs`.
  Deliberately manual, because a submission that died without a verdict may or may not have been delivered, so the GUI shows the blocked state and names the command instead of guessing.
- `SND-08` Partly delivered submission where a recipient was refused, GUI parity for the surfacing.
  Only a human can close this state, and the GUI must not present it as a plain failure.
- `SND-09` Sent-copy append after submission, GUI parity.
  Source `src/imap_client/sent.rs`.
  Implicit workflow driven on the next startup or sync, which is how the outbox drives itself.

#### Sync, offline behavior, and pending operations

- `SYN-01` Quick sync and full sync from a client, GUI parity.
  Entry points TUI `ss` and `sS`, source `src/tui/app/keymap.rs:593`.
- `SYN-02` Sync command options, CLI automation.
  Entry point `mp sync [-n] [--mailbox ...] [--dry-run] [--all-accounts]`, sources `src/main.rs`, `src/sync/engine.rs`.
  `--all-accounts` conflicts with `-A` by construction so a cron line cannot silently sync accounts it never named, and that conflict is a contract to preserve.
- `SYN-03` Watch a mailbox through IMAP IDLE, daemon administration.
  Entry point `mp watch [--mailbox] [--timeout N]` with exit code 2 on timeout, sources `src/main.rs` and `src/imap_client/watch.rs`.
  After cutover the daemon watches continuously and the command becomes a subscription client over the daemon's watcher.
  The daemon's continuous watcher is INBOX-only, as `imap_watch` in `src/tui/helpers.rs` is today, so serving `--mailbox` for an arbitrary mailbox needs an on-demand, client-scoped IDLE connection that is torn down when the client disconnects.
  That connection is a named unit of Phase 4; if it is not built, the narrowing of `mp watch --mailbox` to INBOX is recorded in `BACKLOG.md` rather than left to be discovered at cutover.
- `SYN-04` Startup refresh per account, asynchronous store open, and background mailbox load, GUI parity.
  Sources the `Fetch`, `FetchAccount`, and `LoadMailbox` actions in `src/tui/app/types.rs`.
  Implicit workflow with no command, and the reason a client shows content before sync completes.
- `SYN-05` Sync health and error surfacing per account, GUI parity.
  Source `src/sync_health.rs`.
- `SYN-06` Pending operation queue replayed against the server, GUI parity.
  Sources `src/pending_ops.rs` and `src/ops.rs`.
- `SYN-07` Store ingest, reconciliation, and drop-and-rebuild on an unreadable SQLite file, diagnostics and maintenance.
  Sources `src/ingest.rs`, `src/reconcile.rs`, `src/store/rebuild.rs`, `tests/store_ingest_integration.rs`.
  The durable outbox survives a rebuild, which is the invariant this capability must not break.
- `SYN-08` Retention garbage collection over cached blobs, diagnostics and maintenance.
  Entry point `mp store gc [--dry-run] [--force] [--all-accounts]`, sources `src/main.rs` and `src/store/sweep.rs`.
  The first over-cap run warns and records a marker, while the second run evicts.
  A plan that would reclaim more than half the store's blob bytes is refused without `--force`.
  The daemon runs the same sweep automatically after every sync.
  The sweep skips blobs backing a materialized handle that a client still holds.
- `SYN-09` Per-account engine lock, daemon administration.
  Source `src/engine_lock.rs`, acquired in production by `drain_account` (`src/pending_ops.rs:567`) for the mutation queue and by `drain_guarded_at` (`src/outbox.rs:1422`) for the outbox.
  The outbox acquisition is #0116: a refused drain reports nothing done and opens no session, and the holder re-sweeps against a re-read clock, which is the exclusivity the daemon inherits rather than re-implements.
  `run_and_settle` (`src/pending_ops.rs:614`), the sync body and ingest still take no lock, so the lock excludes concurrent drains and not concurrent ingest.
  The daemon holding it for the account runtime's lifetime is new behaviour, and it is only full exclusion once Phase 3b extends it to the sync ingest path with non-holders refusing.
- `SYN-10` Microsoft Graph backend for sync and send, GUI parity as a backend variant.
  Source `src/graph.rs`.
  Delta cursors replace IMAP UID state, and invitation send is unavailable on this backend as recorded in `SND-05`.
- `SYN-11` Offline operation, where local store reads and draft editing continue while the server is unreachable, GUI parity.
  The GUI reaches this state through daemon events rather than by falling back to direct store access.
- `SYN-12` Per-mailbox body-fetch deadline, GUI parity for its surfacing.
  Configured by `[imap] body_fetch_deadline_secs` per account, default 30, `0` unbounded, clamped to 600 at load (`src/config.rs:240`, `src/config.rs:958`, documented at `website/src/pages/config.astro`).
  Bodies go out newest-first in chunks of 20 (`BODY_CHUNK_SIZE`, `src/imap_client/fetch.rs:525`) with the deadline checked between chunks and never inside a command, and the first chunk always goes out so an expired deadline still makes progress.
  A deadline stop returns `bodies_complete = false` (`src/sync/mod.rs`), which defers the prune and the modseq like any other short pass and resumes on the next tick.
  The TUI reports it as progress rather than failure with "{n} mailbox(es) stopped at the fetch deadline (resuming next sync)" (`src/tui/helpers.rs:411`), and a client must be able to say the same thing (#0113).
- `SYN-13` Tail drain of the outbox and the mutation queue after the sync body, GUI parity for its surfacing.
  Source `run_tick_with_drains` (`src/sync/tick.rs:25`), driven by both TUI tick paths and by `mp sync` (`src/main.rs:1405`).
  Tail report lines on `mp sync` carry an " (after sync)" label so they cannot be read as the head's, and a head-drain error prints "⚠ mutations: drain failed: {e}" and continues instead of aborting the sync (`src/main.rs:1348`).
  The daemon owns the tick after cutover, so this ordering and this non-fatal error are contracts rather than incidental behaviour (#0114).
- `SYN-14` Per-mailbox non-convergence detector, GUI parity for its surfacing.
  Source `src/sync/engine.rs`, which keeps a `nonconverging:{role}` row in the store's meta table holding `hash:count:streak` for the pass's UID set.
  It warns in the log at streak 2, at 3, and every tenth thereafter, `mp sync` prints "'{mailbox}' downloaded the same messages again: the fetch is not converging" on a still-zero exit code (`src/main.rs:1506`), and the TUI appends `NON_CONVERGING_MARKER` (`src/tui/helpers.rs:326`), which downgrades the tick's status level to Warning (`src/tui/bg.rs`).
  A truncated, incomplete, reset, dry-run, or given-up pass neither counts nor resets, and the Graph loop has no detector, so a client must not present its absence there as convergence (#0115).
- `SYN-15` Outbox drain exclusivity under the engine lock, daemon administration.
  Sources `drain_guarded` and `drain_guarded_at` (`src/outbox.rs:1401`), `send::drain_account` (`src/send.rs:2197`), `tests/outbox_integration.rs`.
  A drain refused the lock does nothing, opens no session, and is a success rather than an error, and the holder re-sweeps up to four times against a clock re-read per sweep (#0116).
- `SYN-16` UIDVALIDITY-reset unbind of unverified rows, diagnostics and maintenance.
  Source `unbind_rows_on_uids` (`src/ingest.rs:822`), run after the ingest loop of a pass that reported a reset.
  Every row still parked on a listed UID the pass did not itself ingest moves to the `-id` sentinel, which frees the UID for the message that now wears it and leaves the row rebindable; rows on UIDs the server does not list are left alone.
  Recovery completes on the next full sync, because the download window is positional and the repaired rows sit below it (#0117).

#### Contacts

- `CON-01` Fuzzy contact search over name and address, GUI parity.
  Entry points `mp contacts search [query] [-n] [--account]` and the TUI contacts view `/`, sources `src/main.rs` and `src/contacts/matcher.rs`.
- `CON-02` Tab-delimited `email` and `name` output for mutt, aerc, and vim integration, CLI automation.
  Entry point `mp contacts search --parsable`, source `src/main.rs`.
  A stable shape other tools already consume, so the daemon migration must not reformat it.
- `CON-03` Rebuild or refresh the contact index from the local message store, GUI parity.
  Entry points `mp contacts rebuild [--account]` and the TUI contacts view `r`, source `src/main.rs`.
  The all-accounts default of the CLI form is automation, while the single-account refresh is the user-facing capability.
- `CON-04` Contact index statistics, diagnostics and maintenance.
  Entry point `mp contacts stats [--account]`, source `src/main.rs`.
- `CON-05` Compose to a contact from the contacts view, GUI parity.
  Entry points TUI `Enter` and `n`, source the CONTACTS keymap section.
- `CON-06` Send a contact as a vCard, GUI parity.
  Entry point TUI `v`, source `src/contacts/vcard.rs`.
- `CON-07` Copy a contact's email address, GUI parity.
  Entry point TUI `c`, source the CONTACTS keymap section.
- `CON-08` Address extraction and frecency ranking from the message store, GUI parity.
  Sources `src/contacts/extractor.rs`, `src/contacts/rank.rs`, `src/contacts/hooks.rs`.
  Implicit workflow that keeps the index current as mail arrives.

#### Calendar and iMIP

- `CAL-01` RSVP to a received invitation with accept, tentative, or decline, GUI parity.
  Entry points `mp invite accept|tentative|decline <selector> [--mailbox]`, TUI `tv` in the message context and `V` in the calendar view, sources `src/main.rs`, `src/invite.rs`, `tests/imip_integration.rs`.
  Whole-series only in v1, the reply travels as iMIP over SMTP, and the target message must carry an `invite.ics` blob.
- `CAL-02` Agenda view with an upcoming and past toggle and a refresh, GUI parity.
  Entry points TUI `t` and `r` in the calendar view, sources `src/tui/app/calendar_view.rs` and `src/tui/ui/calendar.rs`.
- `CAL-03` Open the source email of an agenda entry, GUI parity.
  Entry points TUI `Enter` and `e` in the calendar view.
- `CAL-04` Report what stored attendee replies resolve on stored invitations, diagnostics and maintenance.
  Entry point `mp calendar rebuild [--account]`, sources `src/main.rs` and `src/calendar_cmd.rs`.
  Writes nothing, because attendee status is derived from the `invite.ics` payloads wherever it is displayed.
- `CAL-05` Invitation rendering with derived attendee statuses, GUI parity.
  Source `src/invite.rs`.
- `CAL-06` Invitation updates and cancellations reflected in the agenda and the reader, GUI parity.
  Implicit workflow driven by newly synced iMIP messages.

#### Client-side integrations

- `INT-01` Open `config.toml` in the editor, GUI parity.
  Entry point TUI `sc`, source `src/tui/app/keymap.rs:596`.
  The GUI equivalent is the settings surface plus an explicit reveal or open action, and the daemon reloads either way.
- `INT-02` Open the log file in the editor, GUI parity.
  Entry point TUI `sf`, source `src/tui/app/keymap.rs:597`.
  The GUI provides a log view plus an explicit reveal or open-in-editor action.
- `INT-03` Clipboard writes for selectors, paths, and addresses, GUI parity.
  Client-side in every client.
- `INT-04` Browser launch for HTML parts and OAuth verification URLs, GUI parity.
  Client-side in every client.
- `INT-05` Desktop notifications for new mail, GUI parity.
  Source `src/notify.rs`, using osascript on macOS and notify-send on Linux with sanitized payloads.
  The daemon decides that a notification is warranted and the client holding the entitlement presents it.
- `INT-06` Editor suspension and resume around an external `$EDITOR`, GUI parity.
  The GUI replaces suspension with the embedded PTY session.

#### Status, activity, logging, and help

- `OBS-01` Activity log overlay with scrolling and a filter, GUI parity.
  Entry points TUI `!` and `sl`, plus `/` inside the overlay, source the ACTIVITY LOG keymap section.
- `OBS-02` Command palette over every runnable action, GUI parity.
  Entry points TUI `:` and `Ctrl+p`, source `src/tui/app/keymap.rs:841`.
  Derived from `KEYMAP`, so it cannot drift from the bindings.
- `OBS-03` Help overlay and hint bar, GUI parity.
  Entry point TUI `?`, source `help_sections()` in `src/tui/app/keymap.rs`.
  Generated from the same `KEYMAP`.
- `OBS-04` Status line carrying counts, badges, operation progress, and persistent errors, GUI parity.
  Source `src/tui/ui/status.rs`.
- `OBS-05` Structured logging into the platform log directory, diagnostics and maintenance.
  Sources `src/timing.rs` and the `OpenLogFile` action.
- `OBS-06` Dump the key bindings as Markdown or JSON, CLI automation.
  Entry point `mp dump-keys [--json]`, sources `src/tui/app/keymap.rs` and `scripts/regen-website-keys.sh` feeding `website/src/data/tui-keys.json`.
  Needs no daemon, and it is the data feed the GUI key help reuses.
- `OBS-07` Theme configuration through the `[theme]` config section, GUI parity with a recorded deferral.
  Sources `src/tui/theme.rs` and ticket 0023, read once at startup so a change needs a restart.
  The first GUI release is dark-only by settled decision, so this row is deferred with the light theme in the deferred backlog.
- `OBS-08` Quit the client while leaving durable work in place, GUI parity.
  Entry point TUI `q`, source `src/tui/app/keymap.rs:557`.
  In the GUI, closing the window exits the client and leaves the daemon running.

#### Daemon administration and lifecycle

Every capability in this group is new in this plan and has no current source anchor.

- `LIF-01` Run the daemon in the foreground, daemon administration.
  Entry point `mp daemon run`.
- `LIF-02` Start a detached daemon and wait for readiness, daemon administration.
  Entry point `mp daemon start`.
- `LIF-03` Report daemon status, version, instance, and account health, daemon administration.
  Entry point `mp daemon status`.
- `LIF-04` Stop the daemon gracefully, naming operations that prevented a clean stop, daemon administration.
  Entry point `mp daemon stop`.
- `LIF-05` Restart the daemon explicitly, daemon administration.
  Entry point `mp daemon restart`, also invoked by the GUI mismatch screen after user confirmation.
- `LIF-06` Install or remove the login-start service for launchd and systemd user units, daemon administration.
- `LIF-07` Health and support diagnostics over `diagnostic.*`, diagnostics and maintenance.
- `LIF-08` On-demand automatic start from any normal client, daemon administration.
  Excludes the lifecycle commands themselves and the commands that read no domain state.

#### Migration-only surfaces

- `MIG-01` Report what remains of the file-era `.md` tree and import its drafts, migration-only.
  Entry point `mp cutover [--account] [--dry-run]`, sources `src/main.rs` and `src/cutover.rs`.
  Assigns an `id:` field to any draft lacking one so it becomes addressable by selector, names the dead mailbox directories, prints the command that removes them, and deletes nothing itself, while `--dry-run` writes not even the `id:` field.
- `MIG-02` Signature migration from the `[accounts.signatures]` TOML block into signature files with the default recorded in `state.json`, migration-only.
  Source `src/signatures.rs`.
- `MIG-03` Store schema migration at account-runtime start, migration-only.
  Source `src/store/schema.rs`.
  Runs inside the daemon after cutover, so no client performs it.
- `MIG-04` Legacy config-directory migration, migration-only.
  Source `migrate_legacy_config_dir` (`src/config.rs:668`), called today from `src/main.rs:1672` on every command.
  An explicit `MAILYPOPPINS_CONFIG_DIR` suppresses the fallback, which is why it belongs to daemon identity and not only to startup.
  After cutover it runs once in the daemon startup sequence, before the first configuration load, and no client performs it.

#### Retired capabilities

Identifiers stay reserved after a capability is retired, so a reference in an older document does not silently rebind.

- `RD-05` Inline image rendering in the reader, retired by #0109, no parity obligation.
  `src/tui/images.rs` is deleted with the `ratatui-image` and `image` dependencies and the startup graphics-capability probe.
  `parse::inline_images` and `parse::embed_inline_images` remain, feeding the browser view and the `.html` companion, which is where the images are seen at full size.

### Inventory anomalies carried into the plan

- `ANO-1` Generated key output goes stale.
  A `target/` binary older than `src/tui/app/keymap.rs` omits recent bindings, and the signature overlay `cs` was missing from the dumped keys for exactly that reason.
  Every generated artifact used for parity is produced after `cargo install --path .` in the same run, and CI regenerates it.
- `ANO-2` `mp dump-keys` under-describes the real key surface.
  Overlay-internal bindings dispatched as `KeyAction::Manual`, including the compose wizard, the mailbox and attachment pickers, the jump-date prompt, and the filter prompt, never enter `KEYMAP`.
  The parity gate reads `src/tui/app/keys.rs` alongside the dump.
- `ANO-3` `mp fetch` is a legacy surface parallel to `sync` plus `search`, recorded as `LST-11`.
  The migration preserves it, classifies it as diagnostics, and defers the deprecation decision to `BACKLOG.md`.
- `ANO-4` iMIP invitation send is refused for Microsoft Graph accounts, recorded as `SND-05`.
  The GUI presents the action as disabled with its reason instead of failing at send time.
- `ANO-5` Retention garbage collection carries two safety rules, the warn-then-evict marker and the refusal to reclaim more than half the store without `--force`, recorded as `SYN-08`.
  A GUI or daemon port reproduces both rather than relaxing them silently.
- `ANO-6` A materialized attachment or rendition handle can outlive the sweep's view of the store.
  Handles keep their blob alive until release or expiry.
- `ANO-7` The undo-send hold lives in the TUI process and the CLI send paths bypass it, recorded as `SND-04`.
  GUI parity requires daemon ownership of the hold, with CLI behavior unchanged.
- `ANO-8` `mp dump-keys --json` and `mp dump-mailbox --json` are data feeds for the website and for external automation, recorded as `OBS-06` and `LST-12`.
  Their shapes are contracts, and the GUI consumes the keymap feed rather than duplicating it.
- `ANO-9` `mp dump-mailbox` requires `--json` explicitly and `mp sync` rejects `--all-accounts` together with `-A`.
  Both are deliberate contracts that the migration preserves as they stand.
- `ANO-10` No daemon, serve, or IPC surface exists in the current tree, so the protocol, lifecycle, and event layers are greenfield and carry no legacy compatibility burden.
- `ANO-11` Three shipped features were deliberately retired for preview latency, #0010 by #0109, #0087 by #0110, and #0091 by #0111.
  `CHANGELOG.md` carries a reversal note on each original entry, so a matrix built from the changelog alone reads three capabilities that no longer exist.
  The GUI reproduces the current behavior and does not reintroduce them as parity work.
- `ANO-12` The per-keypress preview read is the path the daemon migration makes most expensive, since a local indexed store read becomes a socket round trip.
  The #0108 measurement was waived and `docs/plans/preview-latency.md` closed its cache-and-prefetch contingency as unnecessary, so there is no recorded baseline to compare a daemon-backed run against and Phase 0 has to produce one.
  The migration keeps that contingency buildable in client-side preview code, and the Phase 1a A/B is what decides whether it becomes required work.
- `ANO-13` The per-account engine lock is not the exclusion mechanism it reads as.
  It is taken around the mutation-queue drain (`drain_account`, `src/pending_ops.rs:567`) and, since #0116, around the outbox drain (`drain_guarded_at`, `src/outbox.rs:1422`), but not around the sync body or ingest, so during Phases 3 and 4 a daemon under test, a legacy TUI, a cron `mp sync`, and an older installed `mp` can still ingest into one store concurrently.
  The plan answers that by gating account runtimes behind an opt-in until Phase 5 and by extending the lock to ingest in Phase 3b, and it declares an out-of-band `mp` install out of scope.
- `ANO-14` `MAILYPOPPINS_CONFIG_DIR` (`src/config.rs:606`) and `MAILYPOPPINS_DATA_DIR` (`src/config.rs:1099`) are independent overrides.
  A client with only the config override set would otherwise reach a daemon holding different config, secrets, and signatures, and the integration tests exercise exactly that shape by pointing both at temporary directories.
  Daemon identity is therefore the pair, and a mismatch is refused at the handshake.
- `ANO-15` The client process is the only one with a meaningful working directory.
  `mp save` defaults to the current directory (`src/main.rs:301`) and relative `attachments:` entries resolve against the process cwd (`src/send.rs:1902`), neither of which survives a move into a long-lived daemon started from somewhere else.
  Clients absolutise before the RPC, and a saved attachment is a permanent artifact rather than a lifetime-bounded handle.
- `ANO-16` CI runs bare `cargo test`, which selects the root package only.
  The moment the root manifest also becomes a workspace root, the 367 `#[test]` functions under `src/tui/` stop running and nothing turns red, the golden frames among them.
  The commit that creates `crates/` changes the CI command and `AGENTS.md` in the same commit.
- `ANO-17` The message list is transferred whole today, not paged.
  `list_mailbox` (`src/store/read.rs:178`) has no limit and runs on every mailbox switch and after every sync, and jump-to-date, the metadata filter, the flagged filter, `G`, and select-all all read the resulting in-memory list.
  Paging is therefore a behaviour change rather than a transport detail, and the method-families decision settles it on Phase 1a evidence.

## GUI architecture

### Project structure

Add a Tauri 2 application under `desktop/` with:

- A React and TypeScript frontend built with Vite.
- shadcn/ui components on Radix primitives.
- Tailwind using semantic CSS tokens.
- A narrow Tauri command layer backed by `mp-client`.
- Generated TypeScript protocol types.
- Generated keymap data from the same source as `mp dump-keys --json`.
- A PTY manager for Neovim sessions.
- No SSR, server actions, or web-hosted backend assumptions.

The GUI process holds one daemon connection and one ordered subscription.

A single client-side query layer owns request deduplication, cancellation, invalidation, and reconnect bootstrap.

Presentation components never invoke raw method strings.

### Main layout

Use shadcn's `Sidebar` with `variant="inset"` and `SidebarInset`.

Wide windows show:

- An inset navigation sidebar for accounts, mailboxes, contacts, calendar, settings, activity, and outbox state.
- A center list for messages, conversations, contacts, calendar entries, drafts, or search results.
- A right content pane for message reading, headers, attachments, invitation actions, details, or composition.

Medium windows collapse the sidebar to an icon rail while preserving list and content panes.

Narrow windows navigate between sidebar, list, and content as separate views while retaining keyboard history.

Pane sizes and collapsed state are presentation preferences stored by the GUI rather than daemon domain state.

### Visual system

Use the dark inverse of Basenord Palette D from `/Users/sylvainhellin/Documents/sylvain/01-projects/basenord-pitch-deck/design-system/design-tokens.md`.

Start with:

- Prussian blue `#0C1B33` as the outer canvas and dominant background.
- Cream `#F4F1E8` as the primary foreground.
- Ocean blue `#2E86AB` and its accessible UI variants for selection, focus, progress, links, and framing.
- Pumpkin `#FF6700` only for primary actions, warnings that merit attention, charts, and deltas.
- Inter for UI and body text.
- Lucide outline icons at stroke weight 1.75.
- The approved shadcn radius and inset-shell geometry.

Derive card, popover, muted, input, border, sidebar, destructive, and focus tokens through a dedicated contrast pass.

Do not hardcode palette values in components.

The first release exposes one dark theme while retaining shadcn's semantic token structure for later light themes.

Verify text, focus rings, disabled states, selection states, error states, and orange accents against WCAG contrast requirements.

### Interaction model

Support current TUI keyboard commands whenever focus is outside Neovim.

Expose every runnable GUI action in the command palette.

Add standard mouse targets, context menus, toolbar actions, and macOS menu items without removing keyboard paths.

When the embedded terminal has focus, keys go to Neovim except for a minimal documented set of application-level accelerators.

Selection, focus, scroll, and expanded-header state survive query invalidation when their stable resource still exists.

A fresh bootstrap restores presentation state by stable identifiers and clears references to removed resources.

## Embedded Neovim

### Architecture

Start one real Neovim process per composition session through a native PTY.

Render the PTY in the React webview through a maintained terminal component.

The Tauri Rust layer owns PTY creation, resize, input, output, termination, and process cleanup.

Use ordered Tauri channels for PTY output rather than general broadcast events.

Launch Neovim against the canonical draft path returned by the daemon.

Use the user's normal Neovim configuration and plugins.

Resolve the Neovim executable reliably when the GUI is launched from Finder, whose environment may not contain the interactive shell's PATH.

Support an explicit editor path setting and probe common Homebrew and system locations before reporting a setup error.

Do not bundle a second Neovim distribution in the first release.

### Composition lifecycle

Creating, replying, replying-all, or forwarding first asks the daemon to create the canonical draft.

The GUI then replaces the reader pane with the PTY view for that draft.

Daemon file watching publishes saved changes while Neovim remains open.

Neovim exit returns the content pane to a draft summary or the previous message context.

`:q!` discards only unsaved buffer changes and never deletes the canonical draft.

An explicit GUI discard action confirms and invokes `draft.discard`.

Navigating away from an active process prompts the user to keep it open, terminate it while preserving the draft, or remain in the editor.

Process crashes preserve the draft and show the exit status plus a reopen action.

Window and pane resizing updates the PTY dimensions.

### Feasibility spike

Before building all compose workflows, validate:

- Real Neovim startup from a signed Tauri app.
- User config and plugin loading.
- Finder PATH resolution.
- PTY input, output, resize, clipboard, Unicode, IME, mouse, and color support.
- Correct keyboard routing between the app and terminal.
- Clean child-process termination on window close and app crash recovery.
- Save events reaching the daemon watcher and returning through the subscription.
- Acceptable cold-start and typing latency.

Record the selected PTY and terminal dependencies with maintenance, licensing, and platform evidence before adding them.

## Full GUI capability rollout

Implement vertical slices from daemon operation through GUI and parity tests.

Use the finalized feature matrix to prevent omissions, and name the inventory identifiers each slice closes.

Recommended slice order:

1. Connection, handshake, bootstrap, diagnostics, and daemon restart.
2. Accounts, mailbox hierarchy, counts, sync health, and account switching.
3. Message and conversation listing, pagination, sorting, filtering, and selection.
4. Reader, headers, Markdown and flattened-HTML rendering, the browser escape hatch, and attachments.
5. Archive, delete, move, read, unread, flag, batch selection, mark-read on explicit open, and pending-operation feedback.
6. Local and server search, search forms, scopes, advanced grammar, result navigation, and fetching a server-only hit into the store.
7. Draft listing, creation, recipients, signatures, approval states, and explicit discard.
8. Neovim composition for new, reply, reply-all, forward, and existing-draft editing.
9. Attachment add, open, save, remove, and forwarded-attachment behavior.
10. Send confirmation, approve-and-send, the daemon-owned undo-send hold, cancellation, partial-recipient outcomes, outbox state visibility, and the documented recovery path.
11. Quick sync, full sync, progress, account health, startup refresh, offline behavior, and the two outcomes that must not read as clean, a mailbox stopped at its body-fetch deadline and a mailbox whose fetch is not converging.
12. Contacts, frecency, copy actions, vCard send, rebuild, and composition handoff.
13. Calendar agenda, invitation rendering, RSVP, organizer reconciliation, updates, cancellation, and the Graph invitation restriction.
14. Signature management and per-account defaults.
15. Settings, account setup, authentication, secret updates, retention, and theme-ready preferences.
16. Activity, logs, notifications, help, command palette, and keyboard discoverability.
17. Remaining feature-matrix rows and accessibility corrections.

A slice is complete only when its daemon contract, typed client, TUI behavior, GUI behavior, and tests all pass.

## Migration phases

### Phase 0: Baselines and inventories

- The `pre-daemon` tag exists and points at `f8af44b`, the last of the ten bug-fix commits, following the `pre-dal-nuke` precedent at `BACKLOG.md:20`; the oracle binary is built from it.
- The `daemon` branch currently equals that commit, and `main` is merged into it at every phase boundary and at least fortnightly thereafter.
- Any further fix that lands on `main` before Phase 2 moves the tag, since an oracle behind `main` produces diffs that are fixes rather than regressions.
- Reinstall with `cargo install --path .` before generating any baseline artifact.
- Generate and commit the complete feature-parity matrix from the inventory identifiers above.
- Capture recursive CLI help, keymap JSON, golden frames, and representative CLI outputs.
- Record the overlay-internal keys from `src/tui/app/keys.rs` that the keymap dump omits.
- Add architecture dependency checks that describe forbidden client imports.
- Define representative list, body, search, command, and event workloads for the IPC benchmark, including list refetch at 5 k rows per sync event and a whole-account `dump-mailbox`.
- Record current startup, list, search, and mutation latency baselines, including cold first paint with a cold page cache and cold and warm one-shot CLI latency.
- Add a preview-query timing span so the preview cost is separable from the rest of the paint, then record the per-cursor-move preview cost and the held-key frame count under the pinned run conditions of `docs/tickets/0108-coalesce-key-events.md`.
- Commit every measurement as an artifact under version control rather than quoting it in a report, because the #0108 measurement was waived and nothing recorded survives from it.

Exit gate:

- Every current action and command is classified into one of the five classes.
- Every GUI-parity feature has a target interaction, or a settled deferral recorded in `BACKLOG.md`.
- Baseline tests and snapshots pass unchanged.
- The binary built from `pre-daemon` reproduces the committed baselines.

### Phase 1a: Daemon risk spikes

This half gates Phase 2 and nothing else.

- Implement a minimal JSON-RPC socket benchmark beside direct calls.
- Measure serialization, framing, socket, dispatch, and response costs separately.
- Confirm p50 and p95 incremental latency on representative payloads.
- Run the same-machine A/B against the `pre-daemon` binary and retain JSON-RPC while the p95 delta stays at or below 5 ms.
- Measure list refetch bytes and latency at 5 k rows per sync event, which decides paging against whole-list transfer with row-level deltas.
- Measure throughput for a whole-account `dump-mailbox` and a 10 MB body, which decides the file-handle threshold against chunked frames.
- Measure preview p95 with a list load and a sync write in flight, which sizes the read-connection pool.
- Prototype the atomic bootstrap ordering test without production domain methods.
- Perform dependency due diligence on the daemon-side additions before adding production packages, `notify` among them, since the drafts fingerprint poll deliberately avoids it today and keeping that poll inside the daemon is an acceptable first milestone.

Exit gate:

- The transport decision has measured evidence.
- The list-transfer, large-payload, and read-pool decisions each have a recorded number and a chosen option.
- The bootstrap algorithm passes forced-race tests.
- No spike code is carried into the product tree.

### Phase 1b: GUI risk spikes

This half gates Phase 7 and does not block any daemon work.

- Prototype Tauri, PTY, terminal rendering, and Neovim startup in a disposable branch or isolated directory.
- Perform dependency due diligence on the PTY and terminal packages before adding them.

Exit gate:

- Embedded Neovim meets input, resize, save, and latency requirements.
- No spike code is carried into the product tree.

### Phase 2: Protocol and lifecycle foundation

- Add `mp-protocol` and `mp-client` crate boundaries.
- Implement framing, typed errors, initialization, capabilities, and protocol fixtures.
- Add runtime path permissions, singleton lock, startup lock, stale-socket handling, and readiness checks.
- Add `mp daemon run`, `start`, `status`, `stop`, and `restart`.
- Implement foreground and detached logs.
- Add incompatible-version diagnostics and GUI-ready restart semantics.
- Key the handshake on the pair of data directory and config directory, and refuse a mismatch.
- Run the legacy config-directory migration (`MIG-04`) in the startup sequence, before the first configuration load.
- Serve with zero accounts when no `config.toml` exists, acquiring no engine lock and creating no runtime.
- Add one read-only method family, `account.list` plus `message.list` for one mailbox, reached through a hidden `mp --daemon` debug flag that changes no existing client path.

Exit gate, which is also the minimal milestone that can merge into `main` on its own:

- `mp daemon run`, `status`, and `stop` work, over a socket guarded by the startup lock, with an `initialize` handshake and committed fixtures.
- `account.list` and `message.list` answer through `mp --daemon` for one mailbox, with no engine lock acquired and no existing client path changed.
- Concurrent startup launches exactly one daemon.
- Crash and stale-socket recovery pass on macOS and Linux.
- Incompatible clients fail clearly, including a client whose directory pair does not match.
- A daemon started with no `config.toml` serves `config.*` and reports zero accounts.
- A compatible client can connect repeatedly without leaking tasks or descriptors.
- CI runs the workspace test selection introduced with `crates/`, and the golden-frame and TUI test counts are unchanged.

### Phase 3a: Dispatcher and state model

This half owns no domain state and stays mergeable into `main` on its own, because the daemon is inert unless it is started.

- Extract transport-independent operations from direct CLI and TUI orchestration.
- Implement canonical revisions, `state.bootstrap`, event delivery, coalescing, queue bounds, and resync.
- Publish per-account readiness as `opening`, `ready`, or `blocked`, and return zeroed counts for an `opening` account.
- Extend the read-only method family from Phase 2 to cover one more resource, still behind the debug flag.
- Add long-running operation tracking and progress.

Exit gate:

- Two clients observe ordered authoritative state.
- Event overflow produces bounded recovery.
- A bootstrap taken before any account is ready converges by event without a second bootstrap.
- Nothing in this half changes the behaviour of a client that never sets the debug flag.

### Phase 3b: Ownership moves

- Move config, secrets, store access, sync backends, watchers, pending operations, outbox recovery, and search under daemon ownership.
- Absorb the tick as it stands, head drain, sync body, tail drain, with the tail on the error path and the outbox before the mutation queue, rather than reassembling it from the pieces.
- Publish the tick outcome as a typed event carrying the deadline-stopped mailboxes, the non-converging mailboxes, the deferred prunes, and the failed mutations.
- Extend the per-account engine lock to the sync ingest path, with non-holders refusing rather than proceeding, matching the outbox drain's `Ok(None)` shape from #0116.
- Hold per-account engine locks for runtime lifetime, still behind the opt-in flag that Phase 5 turns on by default.
- Add atomic config reload and account-runtime reconciliation.
- Add unrestricted draft and signature watching, keeping the fingerprint poll inside the daemon unless Phase 1a cleared `notify`.
- Add materialized-handle lifetime accounting so the retention sweep respects open handles.
- Give each account runtime the read-connection pool sized in Phase 1a.

Exit gate:

- The daemon survives client disconnects and continues watchers and durable operations.
- Config and draft external edits propagate correctly.
- A second writer against the same account is refused rather than admitted, at the ingest entry point as well as at the two drains that already refuse.

### Phase 4: CLI cutover

- Migrate command families one vertical slice at a time.
- Preserve output and exit-code fixtures, including the NDJSON ordering contract of `dump-mailbox` and the tab-delimited contacts output.
- Keep prompts, editor launches, browser launches, clipboard, and file pickers in the client.
- Delete each command's direct engine path after its RPC path passes.
- Convert watch-like commands into daemon subscriptions.
- Build the on-demand, client-scoped IDLE connection that `mp watch --mailbox` needs for a mailbox other than INBOX, torn down on client disconnect, or record the narrowing to INBOX in `BACKLOG.md`.
- Absolutise every user-supplied path in the client before the call, `mp save -o` and draft `attachments:` entries included.
- Keep daemon lifecycle commands and the no-domain-state commands independent of automatic startup, with `mp config init` explicitly not among them.

Exit gate:

- Every command that touches domain state uses `mp-client`.
- Cold and warm one-shot CLI latency are within the Phase 0 baselines.
- No command depends on the daemon's working directory.
- No such command opens stores, secrets, network backends, or engine locks.
- Existing CLI tests and help snapshots pass.
- Two concurrent CLI commands share one daemon safely.

### Phase 5: TUI cutover

- Replace TUI initialization with handshake and bootstrap.
- Replace direct store reads with typed queries in the shape Phase 1a chose, paged or whole-list with row-level deltas.
- Replace action side effects with typed commands and operation subscriptions.
- Replace watcher threads with daemon events.
- Keep the in-process undo-send timer, whose move into the daemon is Phase 6 work.
- Turn on account runtimes by default, so the daemon holds the engine locks from here on.
- Preserve presentation state and all keymaps, including the hand-dispatched overlay keys.
- Preserve editor, browser, clipboard, and attachment integrations as client functions.
- Preserve the pre-draw event drain, its bounds, its app-running stop condition, and its terminal-suspend stop condition over the RPC preview query.
- Keep the mark-read trigger on the explicit open, now issued as a command rather than a local write.
- Move the TUI behind a dependency boundary that cannot import engine modules.

Exit gate:

- The complete TUI parity gate passes, oracles included.
- The TUI and CLI run concurrently against one daemon.
- Killing and restarting the daemon produces clear recovery without direct fallback.
- Cold first paint with no daemon running is within the Phase 0 startup baseline.
- `cargo install --path .` installs the working daemon-backed `mp`.

### Phase 6: Daemon hardening and service integration

- Add login-start installation for launchd and systemd user services, which is `LIF-06` and the first work that needs it.
- Move the undo-send hold from `src/tui/actions.rs` into the daemon's send scheduler, keeping the countdown duration, the status-line presentation, and the cancel key.
- Add structured health and support diagnostics.
- Add graceful shutdown semantics around active operations and active send holds, including cancel-and-leave-approved when the last client exits mid-hold.
- Run sustained multi-client, sync, search, draft-watch, and overflow tests.
- Re-run IPC benchmarks against the complete dispatcher.
- Update architecture, lifecycle, troubleshooting, and release documentation.

Exit gate:

- The daemon can run unattended across client churn.
- Memory remains bounded under slow clients and repeated syncs.
- Lifecycle commands and service-manager modes pass platform smoke tests.
- The daemon-owned hold reproduces the behaviour the parity gate recorded, and the last client exiting mid-hold cancels the hold and leaves the draft approved.

### Phase 7: GUI shell and design system

- Scaffold `desktop/` with Tauri 2, React, TypeScript, Vite, Tailwind, and selected shadcn components.
- Add the TypeScript binding generator over the schema Phase 2 committed, which is the first phase with a TypeScript consumer, and generate protocol types and keymap data.
- Implement connection, handshake, bootstrap, reconnect, and mismatch restart UI.
- Implement dark semantic tokens and the inset-sidebar shell.
- Add adaptive panes, command palette, keyboard routing, native menus, and accessibility primitives.
- Add component stories or fixture screens for visual iteration.

Exit gate:

- The shell renders all responsive layouts from fixtures.
- No component bypasses semantic tokens.
- Keyboard and screen-reader focus order pass.
- Connection and resync states are testable without a live mail server.

### Phase 8: Embedded Neovim

- Land the validated PTY and terminal dependencies.
- Implement one session per composition.
- Connect draft creation, path handoff, file-watch updates, process exit, crash recovery, and explicit discard.
- Resolve GUI-launch PATH and configurable editor paths.
- Add keyboard-focus and resize tests.

Exit gate:

- New, reply, reply-all, forward, and existing-draft edit workflows pass with a real Neovim process.
- User configuration and plugins load.
- Drafts survive every exit and crash path.

### Phase 9: Full GUI parity

- Implement every GUI-parity row in vertical slices.
- Keep each slice backed by the same daemon methods used by the TUI.
- Add GUI interaction tests and cross-client scenarios as slices land.
- Update help and website documentation alongside changed commands or interactions.

Exit gate:

- Every GUI-parity row is implemented and validated, or carries a settled deferral recorded in `BACKLOG.md`.
- Every current TUI user capability has a GUI path.
- CLI automation, diagnostics, daemon-administration, and migration-only classifications have an explicit rationale.
- TUI parity remains green.

### Phase 10: Distribution and release

- Extend the release workflow with signed and notarized macOS app bundles and DMGs.
- Bundle the matching `mp` executable inside `Mailypoppins.app`.
- Preserve standalone macOS and Linux CLI archives and Homebrew installation.
- Add a supported mechanism for exposing the bundled `mp` on PATH.
- Ensure the GUI and bundled daemon complete the version handshake.
- Add clean-install, upgrade, restart, uninstall, and quarantine smoke tests.
- Update `docs/release-process.md`, the website, and installation instructions.

Exit gate:

- A clean macOS machine can install the app, start the daemon, use the GUI, run `mp`, restart after a mismatch, and uninstall cleanly.
- Standalone CLI releases remain functional on every existing target.
- Signing and notarization pass in CI.

## Test strategy

### Protocol tests

- Golden serialization fixtures for requests, responses, errors, and events.
- Framing tests for partial reads, multiple frames, size limits, invalid UTF-8, invalid JSON, disconnects, and timeouts.
- Handshake tests for compatible ranges, missing capabilities, incompatible versions, and pre-initialize calls.
- Fuzz or property tests for framing and untrusted input boundaries where practical.

### State synchronization tests

- Atomic bootstrap under mutations before, during, and after snapshot capture.
- Strict revision ordering across concurrent commands.
- Duplicate-event idempotence.
- Gap detection and instance-identifier changes.
- Invalidation coalescing.
- Queue count and byte overflow.
- `state.resync_required` recovery.
- Client disconnect during snapshot and event writes.
- Daemon restart with presentation-state restoration by stable IDs.

### Lifecycle tests

- Concurrent start calls.
- Existing compatible daemon.
- Existing incompatible daemon.
- Stale socket owned by the user.
- Unsafe socket ownership or permissions.
- Foreground signal shutdown.
- Detached readiness timeout.
- Stop and restart during idle, sync, search, and pending send states.
- A daemon started with no `config.toml`, serving `config.*` with zero accounts, then picking up the file a client writes.
- A client whose data directory matches but whose config directory does not, refused at the handshake, and the reverse.
- Connecting and completing `initialize` while every account is still `opening`, with readiness arriving by event afterwards.
- The last client exiting during an undo-send hold, leaving the draft approved and sending nothing.
- launchd and systemd user-service smoke tests.

### Domain and parity tests

- Existing unit and integration tests continue covering engine behavior.
- The `store_ingest`, `store_search`, `dump_mailbox`, `outbox`, `cli_read_surface`, `cli_selector_contract`, `draft`, and `imip` integration suites run a second time through a live daemon against a temporary data root, byte-diffing stdout and exit codes against the `pre-daemon` binary.
- CLI output fixtures compare direct-era baselines with daemon-backed output, including the NDJSON dump and the parsable contacts output.
- TUI golden frames cover bootstrapping, reconnecting, stale state, operation progress, and every existing view.
- Daemon-backed golden-frame variants build their app state from a real bootstrap snapshot rather than the hand-assigned rows of `App::default_for_tests()`, so a wrong snapshot changes a frame.
- A CI check asserts that the golden-frame count and the `src/tui/` test count did not fall, which is the guard against a workspace split silently deselecting them.
- Cross-client tests mutate through one client and observe through another.
- Tick tests cover head drain, sync body, tail drain in that order, the tail running on the error path, and the outbox preceding the mutation queue within each drain.
- Sync-outcome tests cover a deadline stop reported as progress with the prune and the modseq deferred, a non-convergence streak downgrading the outcome without changing an exit code, and a refused outbox drain reporting nothing done while opening no session.
- Undo-send hold tests cover the countdown in the daemon, cancellation from a different client than the one that sent, and daemon shutdown during an active hold.
- Draft watcher tests cover direct writes, atomic renames, invalid frontmatter, deletion, and rapid saves.
- Config watcher tests cover valid swaps, invalid edits, secrets, added accounts, removed accounts, and runtime rollback.
- Retention tests cover the warn-then-evict marker, the half-store refusal, and a sweep running while a materialized handle is open.
- Durable queue and outbox tests cover daemon crashes at each state boundary.

### GUI tests

- Component tests use protocol fixtures and deterministic state snapshots.
- Interaction tests cover keyboard and mouse paths for every GUI-parity feature.
- Accessibility tests cover focus order, labels, contrast, reduced motion, and keyboard-only operation.
- Responsive tests cover wide, medium, and narrow layouts.
- PTY tests cover Neovim startup, Unicode, resize, save, quit, crash, and unavailable executable diagnostics.
- Packaged-app smoke tests use the signed bundle rather than only development mode.

### Performance tests

- IPC p50 and p95 incremental latency against the direct-call baseline.
- Bootstrap size and latency with representative account and mailbox counts.
- Message-list pagination and scroll behavior, or list refetch bytes and latency at 5 k rows per sync event, whichever shape Phase 1a chose.
- Preview response latency per cursor move as a same-machine A/B against the `pre-daemon` binary, under the pinned run conditions in `docs/tickets/0108-coalesce-key-events.md`, with the p95 delta as the criterion.
- Preview p95 with a list load and a sync write in flight, which is what the read-connection pool exists to hold.
- Throughput for a whole-account `dump-mailbox` and a 10 MB body, against the same binary.
- Frames painted for a held navigation key.
- Cold first paint with no daemon running, covering spawn, bind, handshake, and first frame.
- Cold and warm one-shot CLI latency for a representative command.
- Search response latency.
- Event throughput and memory under a sync burst.
- Slow-client queue bounds.
- GUI startup, Neovim startup, typing latency, and memory use.

## Documentation changes

Update these sources as their phases land:

- `docs/architecture.md` for daemon ownership, crate boundaries, state flow, and the retained draft-file truth.
- A new protocol document for methods, revisions, compatibility, errors, and event semantics.
- A new daemon operations document for lifecycle, logs, sockets, login mode, and recovery.
- `docs/release-process.md` for app signing, notarization, DMGs, bundled CLI, and standalone packages.
- `CHANGELOG.md` for each user-visible migration and the GUI release.
- `BACKLOG.md` for explicitly deferred improvements, including every recorded parity deferral and the `mp watch --mailbox` narrowing if it is not built.
- `AGENTS.md` and `.github/workflows/ci.yml` in the commit that creates `crates/`, so the documented and the executed test command stay the same command.
- `docs/plans/preview-latency.md` when the migration changes the per-keypress preview path or settles its cache-and-prefetch contingency.
- `mp --help` and the TUI `?` overlay as primary command and keybinding truth.
- Website pages derived from commands, keys, installation, and supported platforms.

Non-obvious behavior or hard-won fixes are appended to `docs/lessons-learned.md` in the same implementation turn.

## High-risk areas and mitigations

### Hidden direct access

Risk: the TUI or GUI retains a convenient direct engine call and the daemon stops being authoritative.

Mitigation: physical crate boundaries, dependency checks, and tests that run clients without engine configuration access.

### Command parity drift

Risk: the daemon migration changes CLI output, exit codes, or TUI behavior accidentally.

Mitigation: baseline fixtures, vertical command migration, immediate deletion of old paths, and a hard parity gate before GUI work.

The fixtures compare against a binary built from the `pre-daemon` tag rather than against a remembered behaviour, and the integration suites run a second time through a live daemon so the comparison exercises a round trip.

### Stale generated baselines

Risk: a keymap or help artifact generated from an old binary hides a capability from the parity matrix, as the signature overlay already showed.

Mitigation: reinstall before generating, regenerate in CI, and read the keymap source and the manual key dispatcher rather than a committed dump.

### Preview latency under RPC

Risk: the per-cursor-move preview read becomes a socket round trip, giving back the latency that #0108 through #0111 removed.

Mitigation: Phase 0 records the preview baseline under the pinned #0108 run conditions, since the #0108 measurement itself was waived and left nothing to compare against, and Phase 1a and Phase 5 repeat it as a same-machine A/B.

The pre-draw event drain ensures that a held key issues one query per batch.

A failed A/B promotes the preview-latency plan's client-side cache-and-prefetch contingency from optional to required, before the parity gate rather than after it.

### Concurrent writers during the migration

Risk: a daemon under test, a legacy TUI, a cron `mp sync`, and an older installed `mp` write one account's store at the same time, because the engine lock covers the two drains and not the sync body.

Mitigation: account runtimes stay behind an opt-in until Phase 5, Phase 3b extends the lock to the sync ingest path with non-holders refusing, and an out-of-band `mp` install is declared out of scope.

The outbox half of that exclusion already shipped as #0116, so the daemon inherits it: a refused outbox drain does nothing and opens no session, which is also the shape the ingest extension should copy.

### Silent test deselection

Risk: creating `crates/` turns the root manifest into a workspace root, bare `cargo test` selects the root package only, and the `src/tui/` tests stop running without a failure.

Mitigation: the CI command and `AGENTS.md` change in the same commit that creates `crates/`, the Tauri member is excluded by name or kept outside the workspace, and a check asserts the golden-frame and TUI test counts did not fall.

### Cold start regression

Risk: the daemon reintroduces the serial store opens that #0003 moved off the startup path, so first paint waits on `PRAGMA integrity_check` again.

Mitigation: the socket binds and `initialize` answers before any runtime starts, readiness is a snapshot field, bootstrap returns zeroed counts for an `opening` account, and Phases 4 and 5 measure cold first paint and cold one-shot CLI latency against the Phase 0 baselines.

### Event races

Risk: a mutation occurs between snapshot and subscription registration.

Mitigation: the single atomic `state.bootstrap` operation and adversarial race tests.

### Slow-client memory growth

Risk: a sleeping GUI accumulates unbounded events.

Mitigation: byte and count limits, invalidation coalescing, resync control messages, and connection closure as the final recovery path.

### Daemon upgrade mismatch

Risk: a persistent old daemon survives installation of a new client.

Mitigation: mandatory handshake, explicit compatibility ranges, blocking GUI restart action, and clear CLI recovery instructions.

### Draft-file races

Risk: Neovim and an agent edit one unrestricted draft concurrently.

Mitigation: preserve ordinary editor changed-file warnings, publish daemon invalidation promptly, never overwrite external content, and block send on invalid state.

### Materialized file lifetime

Risk: the retention sweep evicts a blob whose materialized file a client has just opened in an external application.

Mitigation: handle-scoped retention, expiry rather than immediate deletion, and a sweep test that runs against an open handle.

### Finder environment

Risk: a signed GUI cannot find the user's Neovim or shell-dependent paths.

Mitigation: configurable executable path, bounded login-shell resolution, known installation paths, and a setup diagnostic.

### Tauri dependency spread

Risk: the GUI stack leaks into standalone Linux or headless CLI builds.

Mitigation: keep the Tauri app in its own workspace member and keep `mp` free of GUI dependencies.

### Packaging two entry points

Risk: bundled and standalone `mp` installations start incompatible daemons.

Mitigation: socket singleton, handshake, explicit restart, version diagnostics, and packaging tests covering both launch paths.

## Deferred backlog

Three items are deferred past the parity gate rather than out of the plan, and each carries a named phase:

- The daemon-owned undo-send hold (`SND-04`), which moves in Phase 6, since the CLI plus one TUI are the only observers before then and the gate tests today's behaviour.
- Login-start launchd and systemd user units (`LIF-06`), which land in Phase 6 with the rest of the service integration.
- TypeScript binding generation, which lands in Phase 7 with its first consumer rather than inside the protocol foundation.

The rest is deferred out of this plan:

- Replacing the drafts fingerprint poll with a `notify` watcher, which the daemon inherits as a poll unless Phase 1a due diligence clears the dependency.
- Serving `mp watch --mailbox` for a mailbox other than INBOX, if Phase 4 records the narrowing instead of building the client-scoped IDLE connection.
- Light theme using the existing semantic tokens, which is the deferral behind `OBS-07`.
- Inline image display in the GUI reader, which the webview could render natively and which carries no parity obligation since #0109 retired `RD-05`.
- Restoring a rich HTML render in either client, which #0111 retired and which `store::read::load_html` keeps one caller away.
- Status-item menu-bar mode.
- Separate composition windows.
- Outbox retry and discard as GUI actions rather than CLI-only operator commands.
- Deprecating or removing the legacy `mp fetch` surface.
- Native Windows support.
- Reconnect replay across daemon instances or long disconnects.
- Protocol compatibility policies beyond the shipped version ranges and capabilities.
- Automatic daemon restart without user confirmation.
- Bundled Neovim distribution.
- Formal external extension system or MCP wrapper.
- Mobile clients or remote daemon transport.

## Final acceptance criteria

The architecture migration is complete when:

- One daemon exclusively owns every engine and network operation.
- Every CLI command that touches domain state and every TUI feature uses the typed client API.
- TUI behavior parity passes before GUI implementation begins, with its oracles run against the `pre-daemon` binary and producing no diff.
- The protocol handshake, atomic bootstrap, revisions, bounded queues, and resync behavior pass integration tests.
- Configuration and unrestricted draft-file edits propagate without a daemon restart.
- The daemon persists independently of clients and supports foreground, detached, automatic, login, stop, status, and restart modes.
- The GUI implements every GUI-parity row of the finalized matrix, with any deferral named by a settled decision and recorded in `BACKLOG.md`.
- The GUI uses the dark tokenized Basenord palette and the shadcn inset sidebar.
- The GUI provides an adaptive three-pane layout and current keyboard command coverage.
- Composition embeds a real user-configured Neovim process in place of the reader pane, and the draft survives every exit path.
- The macOS bundle includes the compatible `mp` executable and passes signing and notarization.
- Standalone macOS and Linux CLI packages continue to pass release smoke tests.
