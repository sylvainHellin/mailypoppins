---
id: 0122
title: Phase 3b of the daemon migration, the ownership moves
type: feature
priority: now
status: done
created: 2026-09-10
---

Fifth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md`), after #0118, #0119, #0120 and #0121.

Phase 3a built a spine that owned nothing.
Phase 3b hands the daemon the four things a client would otherwise have to own itself: the account's engine lock, the configuration, the drafts and signatures on disk, and the files a viewer has open.
Every one of them is a move rather than an addition, so the phase is measured by what a second writer can no longer do and by what a client no longer has to reconstruct from a sentence.

Still behind the `daemon` cargo feature, with one exception that is the point of the first unit: the engine lock on the ingest path is a change to the library every build ships, so `tests/engine_lock_ingest.rs` runs in the default suite and `mp sync` behaves differently today.

## Scope

Six pairs, twelve commits, `1f83463..38f823c`.

The engine lock reaches the ingest half of a sync, so `mp sync` in one terminal and an open TUI in another no longer download and ingest the same window into the same store.
A refused sync is a success that did nothing, the shape #0116's outbox drain already had.

An account runtime holds that lock for its lifetime and owns the account: a tick that is `run_tick_with_drains` verbatim, a readiness a client can ask about, and the two-connection read pool `docs/baselines/decisions/read-pool.md` sized, so a preview read is served while a list load and a write are in flight.

A sync tick publishes a typed `sync.completed` event carrying counts and a severity and never a rendered sentence, with the CLI and TUI wordings derived from it by pure functions in `mp-client`.

The daemon owns the configuration instead of reading it once at startup: an atomic swap that validates the whole candidate before it stops anything, a reload that reconciles the runtime table, and six `config.*` methods, one of which writes a first `config.toml` on a machine that has none.

The one-second fingerprint poll the TUI runs moves into the daemon and watches every account's drafts directory and the shared signatures directory, so an external edit becomes `draft.changed`, `draft.removed`, `draft.invalid` or `signature.changed` without any client watching a filesystem.

A materialised attachment or HTML rendition is a handle, and a live handle pins its backing blob, so the retention sweep of #0060 cannot pull a file out from under a viewer that has it open.

## Commits, by unit

- P3b-U1 and P3b-U2 (`1f83463`, one commit for both because the test file is not gated and cannot land red): `tests/engine_lock_ingest.rs` (730) and `sync::engine::run_sync_guarded` / `run_sync_guarded_at`, with `imap_client::sync_mailboxes` returning `Option<SyncResult>` and both live callers reading `None` as a success.
- P3b-U3 (`b37b87a`) and P3b-U4 (`d2873fc`): `tests/daemon_account_runtime.rs` (1182 as pinned, 1198 after the approved fixes) and `src/daemon/runtime/{account.rs,pool.rs}`. `7308a45` is the approved fix to two contract tests that could not pass as written.
- P3b-U5 (`01c13ac`) and P3b-U6 (`0a807d2`): `tests/daemon_sync_outcome.rs` (1740) and `src/daemon/sync_outcome.rs`, `crates/mp-protocol/src/events.rs`, `crates/mp-client/src/format.rs`.
- P3b-U7 (`4832d47`) and P3b-U8 (`0771ffb`): `tests/daemon_config.rs` (2103) and `src/daemon/config.rs` plus `src/daemon/methods/config.rs`.
- P3b-U9 (`da8de27`) and P3b-U10 (`33c0f64`): `tests/daemon_draft_watch.rs` (2277) and `src/daemon/watch.rs` plus `src/daemon/methods/draft.rs`.
- P3b-U11 (`675b8ed`) and P3b-U12 (`38f823c`): `tests/daemon_handles.rs` (1998) and `src/daemon/handles.rs`, the three `message.*` handle methods, and `store::sweep::sweep_pinned`.
- P3b-U13 (this ticket): this file, the CHANGELOG and BACKLOG entries, `docs/lessons-learned.md`, and `docs/baselines/phase3b-gate-evidence.md`.

## Tests added

176 tests in six files.

`tests/engine_lock_ingest.rs` adds 9 to the default suite, the only ones in the phase that a user's build runs.
The five gated suites add 167: `daemon_draft_watch` 44, `daemon_sync_outcome` 41, `daemon_handles` 41, `daemon_config` 25, `daemon_account_runtime` 16.

The plain suite is 1365 against Phase 3a's 1355, and the featured suite 1802 against 1605.
The 437 the feature adds are 366 across the fifteen gated targets and 71 inline in `src/daemon/`.

## Deviations from the plan

### Five modules over their line budgets

The plan sizes P3b-U4 at under 600 lines and it landed 766; P3b-U6 at under 350 and it landed 536; P3b-U8 at under 600 and it landed 1140; P3b-U10 at under 450 and it landed 990; P3b-U12 at under 400 and it landed 679.

The pattern is Phase 3a's and the answer is the same: the overrun is module-level prose, not logic.
`src/daemon/watch.rs` carries the three arms of a debounce and why a two-armed one never fires; `src/daemon/config.rs` carries the order a swap must take so that everything a reload did is in front of the event announcing it; `src/daemon/handles.rs` carries why the table's clock is a parameter and why `release` must not read a wall clock.
None of the five serves a method the unit did not ask for, and splitting any of them would put one contract across two files.

P3b-U8's figure is two files (`src/daemon/config.rs` and `src/daemon/methods/config.rs`) against one budget line, because the plan names neither.

### `bodies_truncated` carries a count, and the plan's prose says names

`SyncResult.bodies_truncated` is a `usize` (`src/sync/mod.rs`), so the `sync.completed` payload carries the data that exists rather than the list of deadline-stopped mailbox names the plan's prose describes.
The plan anticipated this and asked for the deferral to be recorded; it is in `BACKLOG.md`, and widening the field moves a wire type from `u64` to an array and needs a protocol-changelog entry of its own.

### The Graph ingest loop is not under the lock

`run_sync_guarded` wraps `sync::engine::run_sync`, which is the IMAP path.
`graph::sync_mailboxes_graph` still runs its own orchestration outside the engine (the parked parity half of #0059) and is therefore not guarded, so two processes syncing one Exchange account can still both ingest.

Nothing regresses: the Graph path was never guarded, and the account it would matter for is the parked EVOQS one.
It closes when `graph.rs` becomes a second `SyncBackend`, not before.

### `retention_sweep_after_sync` runs after a refused `mp sync`

`mp sync` treats `Ok(None)` as a success, and the retention sweep in `src/main.rs` rides on every non-dry-run success, so a `mp sync` that did nothing because a TUI held the lock still sweeps that account's blobs.

The sweep takes no engine lock and is safe to run beside the holder, and it is the same sweep the holder's own sync will run, so this is wasted work rather than a hazard.
Moving it under the refusal is a one-line change nobody has been able to justify a behaviour change for.

### The `StateTracker` gap arithmetic is resolved for lifecycle events only

Phase 3a left `mp-client`'s tracker treating any revision above `watermark + 1` as a gap, which coalescing makes wrong.
The lifecycle events this phase adds (`signature.changed` among them) carry their own revisions and are delivered densely, so the tracker is right about them.

A coalesced Replace or Invalidate stream still skips revisions by design, and the tracker still calls that a gap.
It is unobserved because no Phase 3b test drives two mergeable changes past a real tracker, and it stays open.

## The four test fixes

The convention is that an implementer never edits a T unit's file except to fix a contract error approved in writing.
Four such fixes landed, all approved by the orchestrator before the implementing commit.

Two are in `tests/daemon_account_runtime.rs` (`7308a45`), both flagged by P3b-U4's report as unpassable as written.
`a_second_tick_joins_the_running_one_and_the_body_runs_once` gated every body on one `tokio::sync::Notify` and notified it once: a `notify_one` delivered to an already registered waiter wakes that waiter and stores no permit, so the third tick's body parked for ever.
`a_preview_read_is_served_while_a_list_load_and_a_write_are_in_flight` asserted the writer had committed a row before the preview read returned, which is a race the preview usually wins; the writer now announces its first commit on a oneshot.

One is in `tests/daemon_sync_outcome.rs` (`0a807d2`): `PAYLOAD_KEYS` is compared against `sorted_keys(...)` and listed `prunes_deferred` before `pruned`.

One is in `tests/daemon_bootstrap.rs` (`0a807d2`): the test's own `reduce` matches `Change` exhaustively, and the new `Change::SyncCompleted` variant left it non-exhaustive, so it gained an empty arm matching the daemon's own reducer.

`git diff --stat 1f83463~1..38f823c -- tests/` shows the six new files, those four edits, and one widening a T unit made to two earlier T units' files: P3b-U9 grew `Change::DraftUpsert` to the fields `draft.changed` carries and added `Change::DraftInvalid`, which `tests/daemon_bootstrap.rs` and `tests/daemon_events.rs` both reduce over.

## The shapes worth remembering

A runtime that holds the engine lock must call `run_sync`, not `run_sync_guarded_at`.
`flock` is per open file description, so the guard would contend with its own holder and refuse every tick.

`SmtpSettings` and `ImapSettings` now have hand-written `Default` implementations.
They carried per-field `serde(default = ...)` attributes and a derived `Default`, so an account with no `[accounts.smtp]` table loaded port 0 where an account with an empty one loaded 465, and the same omission asked for an unbounded body fetch.
`config.get` reporting the effective configuration is what made the two readings visible in one place.

`draft.invalid` carries `account` alongside the plan's `{id, path, diagnostics}`, because it replaces the same `draft:<account>/<id>` resource as `draft.changed` and a client resolving that key needs the account in the payload rather than in the event's envelope.
Refusing to approve an unparseable draft needed a code the plan's table does not have, so `-32010 draft_invalid` widens the daemon's range to `-32010..=-32000`.

`message.release_handle` is registered as a `Query` rather than a `Command`.
Releasing a handle commits no change to the canonical state and publishes no event, so declaring it a command would stamp a revision every client has to look at for nothing.

A draft with no `id:` is announced under its file stem.
Minting an id would mean writing to a file an editor may be holding open, which is the one thing the watcher promises never to do.

`config.init` writes its `config.toml` through its own TOML writer rather than reusing `mp config init`'s, because the CLI's version prints, prompts and exits, and the daemon's has to answer over a socket.
There are two templates now, and keeping them in step is on whoever edits either.
The daemon's also refuses an existing file with `-32602` naming it, where the CLI asks "Overwrite? [y/N]": a socket has nobody to ask, and the error table has no "already exists" code to invent one from.

## What Phase 3b does not do

The daemon schedules no tick: `RuntimeTable::tick_account` has no caller outside its own tests, and the periodic scheduler is Phase 5/6.

`store::sweep::sweep_pinned` has no caller either except `sweep` itself with an empty pin set.
The daemon holds the pin set and the seam is proved in process, but nothing in the daemon sweeps yet, so the post-sync sweep stays `mp sync`'s.

## Gate evidence

`docs/baselines/phase3b-gate-evidence.md` maps each of the three Phase 3b exit-gate lines to the test or command that proves it, with the commands as run and their results.

All three pass as written.

## Acceptance criteria

- The daemon survives client disconnects and continues watchers and durable operations. Met, 16 tests in `daemon_account_runtime` and 44 in `daemon_draft_watch`, the last of which drive a real socket and a subscribed client rather than the watcher in process.
- Config and draft external edits propagate correctly. Met, 25 tests in `daemon_config` and the atomic-save, debounce-collapse and invalid-frontmatter cases in `daemon_draft_watch`.
- A second writer against the same account is refused rather than admitted, at the ingest entry point as well as at the two drains that already refuse. Met, 9 tests in `engine_lock_ingest` in the default suite, with the refusal asserted to open no session against a backend whose `fetch_targets` panics.

## Files

- `src/sync/engine.rs`, `src/imap_client/store_sync.rs`, `src/main.rs`, `src/tui/{helpers.rs,bg.rs}` for the ingest lock
- `src/daemon/runtime/{account.rs,pool.rs,mod.rs}`, `src/daemon/sync_outcome.rs`, `src/daemon/config.rs`, `src/daemon/watch.rs`, `src/daemon/handles.rs`
- `src/daemon/methods/{config.rs,draft.rs,message.rs,account.rs,mailbox.rs,mod.rs}`, `src/daemon/{server.rs,session.rs,lifecycle.rs,mod.rs}`, `src/daemon/state/{mod.rs,events.rs,snapshot.rs}`
- `src/config.rs` for the two hand-written `Default` implementations, `src/store/{read.rs,sweep.rs}` for the handle reads and the pin seam
- `crates/mp-protocol/src/{events.rs,error.rs,lib.rs}`, `crates/mp-client/src/{format.rs,lib.rs}`
- `tests/{engine_lock_ingest,daemon_account_runtime,daemon_sync_outcome,daemon_config,daemon_draft_watch,daemon_handles}.rs`, plus `tests/daemon_bootstrap.rs` and `tests/daemon_events.rs` for the `Change` variants this phase adds
- `docs/{architecture.md,daemon-protocol.md,daemon-operations.md,parity-matrix.md,lessons-learned.md}`, `docs/baselines/phase3b-gate-evidence.md`
- `Cargo.toml`, `CHANGELOG.md`, `BACKLOG.md`
