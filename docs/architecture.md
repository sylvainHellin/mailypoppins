# Architecture

How mailypoppins is put together.
Read this before non-trivial changes.

## Project invariants

- The server is truth, the store is a cache.
Received mail lives in a per-account SQLite file plus a content-addressed blob store, and both are disposable: a schema mismatch, a failed integrity check or an unreadable file is answered by deleting the store and letting the next sync refill it.
Nothing in the store may be the only copy of anything the user typed.
- Drafts are the only local truth.
They are Markdown files with YAML frontmatter under `<account_dir>/drafts/`, written by `mp new` and by `$EDITOR`, and the `drafts` table is a derived index over them.
- Received mail is read-only locally.
The client never edits a message body; it changes flags and mailbox membership, and the server is told immediately.
- No migration paths until v1.0.
When changing data formats, secret storage or wire protocols, drop the old code and prompt the user to reconfigure.
Do not write v1 to v2 migrators.
- The TUI implements no email logic.
No SMTP, IMAP, MIME or Graph REST code belongs in `tui/app/` or `tui/ui/`; the TUI layering section below states what those two layers do and do not touch today.
- Windows is targeted via WSL only.
No native-Windows code paths (registry, Credential Manager).

## Crate shape

A Cargo workspace: the root package is the library plus the binary, and `crates/mp-core`, `crates/mp-protocol` and `crates/mp-client` are the three crates beside it (see "Daemon and crate boundaries" below).
The engine lives in the root package's `src/lib.rs` modules, the shared engine-free modules in `mp-core` (re-exported from the root crate under their old paths), and the daemon in `src/daemon/` drives both; the CLI and the TUI are clients of it over a Unix socket and spawn no subprocess of their own.
Config types derive `Clone` so they can be moved into background threads.

The installed binary is `mp` (`cargo install --path .`).
The Cargo package and library are `mailypoppins` (#0022), so imports read `use mailypoppins::...` and `insta` snapshot files are prefixed `mailypoppins__`.
The user-facing name and version string is `mailypoppins X.Y.Z`, set via clap `#[command(name = "mailypoppins")]` and `#[command(version)]` in `src/main.rs`; the Homebrew formula test asserts against that string.
The one place the old spelling survives is the keyring service fallback below.

## Daemon and crate boundaries

mp is being restructured around a local daemon that owns every store read, every network call and every durable operation, with the CLI, the TUI and a later GUI as clients of it (`.agents/workflow/native-gui-daemon/plan.md`).
The wire contract is [daemon-protocol.md](daemon-protocol.md) and the operator's half is [daemon-operations.md](daemon-operations.md).
What follows is the shape that migration imposes on the tree today, which is all that is built.

### The shared crate

`crates/mp-core` owns what a client and the engine both need and neither owns: configuration and the data-directory layout, secrets and the OAuth2 token cache, signature files and `app_state`, RFC822 parsing, the shared types, iCalendar parsing and building, the search grammar, the `mp://` selector grammar, the draft file format, the contact index, the iMIP reply fold, desktop notifications, `TimingSpan` and `SyncHealth`.
It reaches no store, no IMAP session, no outbox and no sending transport, which is the whole of its definition; P5-U10a (#0126) moved it out of the root package so that a future `crates/mp-tui` has somewhere to depend on, and P5-U10b finished the closure.

Every module it holds is re-exported from `src/lib.rs` under the path it had inside the root package, so `crate::config::…` and `mailypoppins::parse::…` resolve unchanged everywhere: the CLI, the daemon, the TUI and the integration tests.
Six modules are split rather than moved whole, and in each case the root crate keeps the half that reads an engine and re-exports the rest with `pub use mp_core::<module>::*`:

| module | in `mp-core` | left in the root package |
|---|---|---|
| `selector` | the grammar, the parser, the formatter, percent-encoding, `draft_not_found` | `resolve_received`, `resolve_draft` (one indexed lookup each) and the `MessageRowRef` impl for `store::read::MessageRow` |
| `search` | the whole parser and its four renderers, plus `imap_query`, the three pure IMAP string helpers it reads out of `imap_client` | nothing; `imap_client::search` re-exports the three helpers |
| `invite` | the ICS building, `Rsvp`, the reply builder, the date and duration grammar | `plan_invite`, `InviteRequest`, `InvitePlan`, `GRAPH_REFUSAL` (they read an account) |
| `reconcile` | the fold: `InviteMessage`, `fold_replies`, `apply_replies`, `fold_status`, `own_rsvp`, `ReconcileReport` | `load_invites`, `event_for_message`, `reconcile_account` (each opens a store and reads blobs) |
| `contacts` | `cache`, `filter`, `matcher`, `rank`, `types`, `vcard` whole, plus `extractor`'s observation half (`observe`, `process_header`, `self_address`) | `build_index_for_account`, the store rebuild, and `hooks`, which reads `sync::FreshObservation` |
| `draft` | the file format: the skeleton, the signature sentinels, the frontmatter rewrites, the reply and forward builders, `validate_draft`, `preview_draft`, `find_drafts`, `mark_as_*` | `new_draft_skeleton` (mints an id), `source_from_row`, `create_draft_from_source`, `settle_sent_draft`, `delete_indexed_draft` |

`mp-core/addresses.rs` arrived the way `imap_query` did: four pure RFC 5322 string functions (`split_addresses`, `normalize_address_for_smtp`, `quote_display_name`, `format_recipient`) that lived in `send` because that is where the first caller was, and that `draft`'s validation reads without wanting a transport.
`send` re-exports the three public ones.

`#[cfg(test)]` does not cross a crate boundary, so the two test seams the root crate's own tests depend on - `config::test_env`'s thread-local data-dir overrides (#0077) and `parse::test_temp_root`'s per-thread materialisation root - are behind `mp-core`'s `test-support` feature, which the root crate enables through its `[dev-dependencies]` entry.
Resolver v2 keeps a dev-dependency's features out of a plain `cargo build`, so the shipped binary compiles exactly what `#[cfg(test)]` used to leave out.

### The two client-side crates

`crates/mp-protocol` owns the wire: the JSON-RPC message structs, the numeric error table, the newline framing codec and the event envelope.
It knows nothing about sockets, accounts or the store, and `crates/mp-protocol/fixtures/*.json` pins one committed example of every public shape.
It also owns the pure serde types both ends of the socket speak, which is why `mp_protocol::calendar` holds `EventFrontmatter` and `EventAttendee` (re-exported from `mailypoppins::types` under their old paths) beside the `AgendaEvent` row `calendar.events` answers with.

`crates/mp-client` owns the transport: one `Connection` is one Unix-socket connection, and the crate carries the `initialize` handshake and the typed errors a caller branches on.
It owns no policy, no paths and no configuration.

Neither crate depends on `mailypoppins`, and nor does `mp-core`; that is the boundary that matters: a GUI links `mp-client` alone and cannot reach the engine by accident.
The daemon itself is not a crate; it is `src/daemon/` inside the root package, because it drives the engine that already lives there.

### The `daemon` feature, and its removal

For Phases 2 to 3b, `src/daemon/` sat behind `#[cfg(feature = "daemon")]` and every daemon contract test had an explicit `[[test]]` target in `Cargo.toml` with `required-features = ["daemon"]`.
Cargo skips building a target whose required features are unmet, which is what let a contract test land in one commit and its implementation in the next: the test commit's proof was that `cargo test` was green and unchanged while `cargo test --features daemon` failed to compile with unresolved imports naming exactly the contract items.

P4-U1 removed the feature.
From Phase 4 the daemon is required by default, so there is one build, one test command and no `[[test]]` stanzas at all (Cargo autodiscovers `tests/*.rs`):

```sh
cargo test --workspace   # the whole tree, daemon included
```

What did not change is the help surface: `mp daemon`, `mp account` and the global `--daemon` flag keep `#[command(hide = true)]` / `hide = true`, so `mp --help` stays byte-identical to `docs/baselines/pre-daemon/cli-help.txt` until a later unit moves it deliberately.

`tests/test_selection_guard.rs` defends the arrangement from the other side.
It counts `#[test]` attributes by scanning `src/tui/**/*.rs` and `crates/mp-core/src/**/*.rs` rather than by asking the harness what it selected, so a workspace change that silently deselects a whole file of tests fails the guard instead of shrinking a summary line nobody reads.
The four floors are 467 TUI tests, 416 `mp-core` tests, 20 golden-frame tests and 20 snapshot files, and they track the tree rather than the pre-workspace commit: a floor a hundred tests below the tree lets three whole test modules vanish together without failing.
The TUI floor came down once, from 492 to 467, when P5-U10c-I2 moved the agenda loader and its 25 tests out of `src/tui/`; they stayed in the root package, so the `--lib` run did not move.
The `mp-core` floor is #0126's and its arithmetic is the move's proof: the root package's `--lib` run went from 1 398 to 982 across P5-U10a and P5-U10b while `mp-core` runs 416, and 982 + 416 is 1 398.

### What the daemon owns since Phase 6

Phase 6 (#0125) is the phase that makes the daemon something which can be left running, and it added five things to `src/daemon/` rather than to any client.

**The undo-send hold is a scheduler** (`src/daemon/hold.rs`).
A held send is an operation with a deadline: `send.draft` and `send.approved` answer `{operation_id, held: true}` at once, a task publishes one `send.hold_tick` a second, and at the deadline it awaits the very future the unheld path would have spawned, so there is one send path and not two.
The window is `email.send_hold_secs`, resolved by the daemon from the configuration it already owns, which is why `hold` is a boolean on the wire and why `mp send` bypasses the hold by passing nothing.
One hold ends exactly once because the table is the authority: `fire` and `cancel` both remove the row under one mutex, so the timer task needs no cancellation token.
The TUI only renders it; `u` is a round trip (`Action::CancelHeldSend`) and the countdown stays on screen until the daemon says it is cancelled.

**Shutdown is a sequence, and the order is the contract** (`src/daemon/shutdown.rs`).
Mark shutting down, cancel every armed hold, announce `daemon.shutting_down`, answer the `daemon.stop`, wait out the grace, stop the watchers and the runtimes, report `daemon.stopped` on the asking connection and close every connection, unlink the three runtime files.
Steps 1 to 4 run inside the connection task that answered, so the answer cannot describe a daemon that has already moved past them; steps 5 and 6 run in a driver task, because the grace may be ten seconds and the stop's answer may not wait for it.
The grace is a ceiling and never a sleep, which is what keeps stopping an idle daemon as cheap as every fixture in the test tree needs it to be.
`SIGTERM` and `SIGINT` run the same steps, so a unit stopped by systemd is as graceful as a typed `mp daemon stop`.

**Diagnostics are an assembly, not a family of getters** (`src/daemon/diagnostics.rs`).
One module holds the checks, the ledger a check's status is compared against, the log reader and the redacting bundle writer; everything else is routing, four method arms and three CLI commands.
A check is re-evaluated when an account runtime reports, when a `config.reload` happens or is refused, and every 60 seconds as a safety net, and only a status that *moved* publishes `diagnostic.check_changed`.
Reads evaluate and publish nothing, which is what keeps a bootstrap from announcing the very checks it is handing over in the same frame.

**The login service is two templates and a writer** (`src/daemon/service.rs`, `src/daemon/templates/`).
`mp daemon install-service` writes a systemd user unit or a launchd agent, both running `mp daemon run` by the absolute path `current_exe()` resolved to, and both carrying the two directory variables the installing `mp` resolved.
The templates are compiled in rather than `include_str!`ed from `tests/fixtures/service/`, so the library does not need its own test fixtures to build, and one module test asserts the two copies are byte-identical.

**The operation registry forgets** (`src/daemon/operations.rs`).
It kept every operation the process had ever started, which the soak measured at about 0.8 KiB per settled operation, never returned; a `start` that finds more than `HISTORY` (256) settled entries now forgets the oldest of them first.
Live operations are never forgotten, because the table is also what a disconnect and a shutdown cancel through, and an id past the window answers as an id this daemon never issued, which a client that outlived a restart already handles.

The operator's half of all five - the commands, their stdout, the environment hooks and the recovery paths - is [daemon-operations.md](daemon-operations.md).

### The engine-import allow-list, the engine-path allow-list, and the CLI's engine-touch residue

`tests/architecture_boundaries.rs` holds all three halves of the client/engine boundary.

The first walks `src/tui/`, collects every `use` of an engine module, and asserts the set equals `tests/fixtures/tui-engine-imports.txt`.
The file holds 5 pairs over 4 files today, and that 5 is the number the deferred P5-U10 has to drive to zero as the last of the TUI's engine calls become daemon calls (see "TUI layering" below).

The second (P5-U10d-T) is what the import scan could not see: most of the calls that block the crate move are spelled as fully-qualified paths no `use` line mentions, so the import list read six while the work was six *groups* of call sites.
It walks the same tree for those paths, over production code only, and asserts the set equals `tests/fixtures/tui-engine-paths.txt`, 5 rows over 4 files today, down from the 15 P5-U10d-T recorded.
Three kinds of path: the eleven `ENGINE_MODULES` names reached through `crate::…`, the root crate's own halves of the shared modules (`crate::agenda::` and the five `draft` operations, named as the symbols they are called by), and `crate::daemon::` itself.
Test modules are stripped, whole test files are stripped by deriving them from the `#[cfg(test)] mod x;` lines that declare them, and `use` lines are stripped so nothing is counted by both guards.

Both are records, not ceilings: a removed import or a removed call fails the test as loudly as a new one, because the counts are the migration's progress bar.
Re-record a deliberate change with `UPDATE_TUI_ENGINE_IMPORTS=1 cargo test --test architecture_boundaries`, which rewrites both fixtures.
Neither is feature-gated and both pass on the pre-daemon tree, and `engine_imports` takes the client source root as an argument so Phase 5 can re-point it at a `crates/mp-tui/` without a rewrite.

The third (P4-U15) walks the client-side sources - `src/main.rs`, `src/cutover.rs`, `src/config_cmd/` - for the 23 symbols that open a store, a secret backend, a network backend or an engine lock, and compares the result against `CLI_ENGINE_RESIDUE`, an inline table whose third column is why each survivor is still there.
Seventeen rows in four groups: the server leg of `mp search` (`docs/parity-matrix.md` LST-06, which no Phase 4 slice contracted), the startup preamble (which runs before any socket and on the no-daemon list too), `mp config show`'s secret and token probes (`config.get` is contracted *not* to look a secret up), and the two `config.toml` wizards (one interactive transaction).
The TUI is deliberately outside this second list until Phase 5; the first half is what records its residue meanwhile.
There is no `UPDATE_` switch for it: an entry is added by hand, with its reason, or it is not added.

`docs/baselines/phase4-gate-evidence.md` is the long form.

### The hidden CLI surfaces

Three surfaces carry `hide = true`, so `mp --help` is byte-identical to `docs/baselines/pre-daemon/cli-help.txt`.

- `mp daemon run | start | status | stop | restart`, the lifecycle commands, plus `install-service | uninstall-service` (the login units) and `health | logs | support-bundle` (the diagnostics), all added in Phase 6 under the same hidden subtree.
- `mp --daemon`, a global flag that routes a command through the daemon instead of answering it in process. It never falls back; a routed command that cannot reach a daemon exits 4.
- `mp account list`, which lands with the daemon work because it is the oracle `mp --daemon account list` must match.

The double gate is deliberate.
`tests/cli_help_snapshot.rs` holds one snapshot shared by the featured and the unfeatured build, so a subcommand visible under the feature would fail one of the two `cargo test` runs against a snapshot it cannot satisfy.
P4-U1 drops both the `cfg` and the `hide` and moves the snapshot once.

## The store

One `store.sqlite3` per account, in the account directory, opened in WAL mode with a 5 s busy timeout and `synchronous = NORMAL` (`src/store/mod.rs`).

The drop-and-rebuild contract is the reason there is no migrator.
`Store::open` rebuilds the file from scratch when the stamped schema version is not the current one, when a required table is missing, when `PRAGMA integrity_check` fails, or when the file does not open as a database at all.
None of those is a user-visible error: the store holds no truth, so the answer is a log line and a file built from scratch.
The integrity check walks the whole file, so it runs once per file per process rather than on every open.

One table is not a cache and does not go with the file: `outbox` (#0066, `src/store/rebuild.rs`).
Before the old file is deleted its unfinished rows (`pending_send`, `sent_pending_append`, `failed`) are read back defensively, by column name, so an outbox of an older shape still comes across, and are written into the new file with a reference on the raw RFC822 blob each one points at.
`done` rows owe nothing and stay behind.
A row that cannot be carried, because its bytes are gone from the blob store or its columns are unreadable, is named in a `store-rebuild-<timestamp>.txt` note written next to the store; nothing about a submitted message is discarded silently.
The same pass then sweeps the blob tree, deleting every file the rebuilt store holds no refcount row for, so a rebuild cannot leave the blob directory full of orphans that nothing reclaims.

Schema v6 lives in `src/store/schema.rs`, which carries the identity notes in full; the short version:

- `messages` is one row per message per mailbox, with a synthetic `id` and `UNIQUE (account, mailbox, uid)` as the real identity.
The same message in two mailboxes is two rows.
Its `flags` column holds the IMAP flag string, which is where the second status axis lives (#TKT-0051): `\Seen`, `\Answered` and the `$Forwarded` keyword, parsed into `types::MessageFlags`.
Three independent bits rather than one state, and no schema change, because the column was already there and every sync pass restates it for the whole window.
The `messages_message_id` index is deliberately non-unique: it serves threading, idempotent re-ingest, cross-mailbox copy detection and selector resolution.
- `blobs` is the refcount index for the content-addressed blob store, and `message_blobs` is the per-message list of `body`, `html`, `raw` and `attachment` references.
Refcounts live in the database so a reference can be taken in the same transaction as the row that carries the hash.
- `messages_fts` is a contentless FTS5 index (`content=''`, `contentless_delete=1`) over subject, from and body text.
Only `rowid`-returning `MATCH` queries work; there is nothing to rebuild from, and nothing needs to be, because a store that loses its index is dropped.
It is written inside the same transaction as the `messages` row it describes and removed by every delete path, so it needs no reconcile pass; `store::search::index_drift` is the check that says so rather than the comment claiming it.
`store::search` is the query side (#0043), behind `mp search --local`.
Since #0086a it no longer parses: it lowers the shared query AST (`crate::search`) with `to_fts`, so `--local` and server search read one grammar and the #0043 two-grammar debt is closed.
- `sync_cursors` is keyed by `(account, mailbox)` and keeps `last_uid` (where the IMAP pull resumes) apart from `highest_modseq` (a CONDSTORE sequence, NULL until #0041) and `deltalink` (the Graph `/messages/delta` resume point, #0042; on a Graph account `uidvalidity` holds the hashed folder id that token is bound to, which is the analogous column on purpose).
The two were one column until #0054, which stored a UID where a modseq was read back.
`arrival_mark` (v5, #0072) is the one column here a later pass reads back: the UID above which the mailbox still owes the store a message the server lists, which keeps the prune gate shut until a pass reaches through it.
A message the pass downloaded and then failed to write pulls that mark under itself (v6, #0074), because a message not written is as absent as one never fetched.
- `ingest_failures` (v6, #0074) counts those failures per `(account, mailbox, uid)` and bounds them: after `ingest::MAX_INGEST_ATTEMPTS` passes the UID is given up on loudly and stops holding the mark down, so a message the store rejects deterministically cannot suspend the prune for the whole account for good.
A successful ingest deletes the row, so transient failures never accumulate towards the bound.
- `outbox` carries the durable send state machine described below.
- `drafts` is the derived index over the drafts directory. The daemon watches that directory itself instead of reading the index (`src/daemon/watch.rs`): a debounced one-second `stat` poll that reparses what settled and takes no engine lock.
- `pending_ops` carries the durable mutation queue (#0039): one row per owed server op, with `kind`, the `messages` row id in `target_message_id`, the full `ServerOp` plus its rollback in the JSON `payload`, and a `queued` / `failed` state. `src/pending_ops.rs` owns it, the mutation twin of `outbox`. Like `outbox` its live paths are not the schema's concern, but unlike `outbox` it is a plain cache table: a lost queue row loses a flag change or delays a move, never a message, so it is dropped and rebuilt with the file.
- Every table carries `account`, although one file holds one account.
The redundancy keeps a future shared database a schema change rather than a rewrite of every query.

The blob store (`src/store/blobs.rs`) is `<account_dir>/blobs/ab/cd/<sha256>`: every raw message, decoded body and attachment is a file named by the hex SHA-256 of its own bytes.
The name is the content, which buys dedup, verification (a read re-hashes and refuses bytes that no longer match their name) and immutability.
Blob files are written before the transaction that references them, never inside it: an unreferenced blob is a harmless orphan a sweep reclaims, while a row pointing at a missing blob is a hole in the read path.

### Retention sweep

`src/store/sweep.rs` is the one code path that deletes user data (#0060), and it deletes only from the cache: blob *files* and their `blobs` refcount rows, never a `messages` row, so the listing stays complete and an evicted body is re-materialised by a re-ingest of the same message.
It runs after every `mp sync` and on demand via `mp store gc`, honouring the resolved `RetentionPolicy` (`max_disk_bytes`, default 10 GB, and the body/attachment age horizons).
A two-strike rule protects a store that briefly spikes: the first over-cap sweep only warns and persists a store-level marker in `meta`, the next over-cap sweep evicts, and dropping back under the cap clears the marker.
When it does evict, victims are taken age-horizon-first (attachments then bodies past their horizon), then attachment blobs oldest-first, then body blobs oldest-first, stopping the moment the store is back under the cap; a blob's age is its freshest referencing message, so a shared blob survives while any message still inside its horizon references it.
The sweep and a concurrent ingest cannot interleave mid-statement: both go through the store's single-writer WAL connection discipline, so a `mp store gc` run during a sync only ever sees committed blobs.
One half is deferred to #0085: on-open re-fetch of an evicted body does not exist yet (a plain `mp sync` skips a UID it already has a row for), so until it ships recovery is a targeted re-ingest, and `mp store gc` refuses to reclaim more than half a store at once without `--force`.

`sweep_pinned` is the same sweep with a set of blob hashes held back from the eviction plan, and the daemon fills it from the handles a client has materialised and not yet released (`ANO-6`, [daemon-protocol.md](daemon-protocol.md#materialised-handles)).
A pin changes who may be a victim and nothing else: the store's size, the two-strike marker and the half-store guard are all computed as before, over the plan the pin left, so `--force` still overrules the guard and never the pin.
`sweep` is `sweep_pinned` with an empty set, which is what `mp store gc` and the post-sync sweep call.

## Data flow

### Receive

A sync backend hands raw messages to `src/ingest.rs`, the only writer on the receive path.
One transaction per message writes the `messages` row, its blob references and its FTS entry, so a crash leaves whole messages behind and never half of one.
Re-ingesting a UID is an UPSERT that keeps the row `id`, its thread assignment and the blob references whose content did not change.
After a UIDVALIDITY reset the row is found by Message-ID and rebound to the new UID in place.
A message with no `Message-ID` header gets a deterministic `sha256-<hex16>@local.invalid` synthetic id.

### Read

Everything the TUI, `mp dump-mailbox` and the contact index show comes from `src/store/read.rs` and `src/store/drafts.rs`.
There is no directory-walk fallback: nothing writes `.md` for received mail, so a missing row is a bug in ingest and a walk that quietly produced the message anyway would hide it.
Attachments are blobs, so anything that needs a file materialises them: `mp open` and the TUI's `o` into a handle directory under `<data_dir>/runtime/handles/`, one per handle, minted by the daemon and pinning the blob against the retention sweep for its lifetime (P5-U6, #0122), a forward draft into `<account_dir>/attachments/<message-id>/` so the draft keeps resolving them after the source row is archived or evicted.

### Mutate

A flag, move, archive or delete is one local write plus one server op, and both frontends now route it through the durable queue `src/pending_ops.rs` (#0039).
`apply_move` / `apply_delete` / `apply_set_read` / `apply_set_flagged` commit the local write and the owed `ServerOp` (defined in `src/ops.rs`, the library home of the remote op) in one transaction, so a crash between the halves can never lose the op nor leave the store optimistically changed with nothing owed.
`src/mutations.rs`'s `queue_*` functions call those `apply_*` and return the rows they touched for the list update; they moved out of `src/tui/` in P5-U6, so the daemon's five message mutations and the TUI that asks for them run one pairing rather than two copies of one, and neither keeps a server thread or a rollback of its own, because the queue owns both.
The background `drain` retires confirmed ops and rolls failed ones back under the engine lock, and it runs at the sync/fetch resume points beside `resume_outbox` (`pending_ops::resume_account`), draining nothing and building no backend when no row is owed.
Replay is exactly-once for the local half because the drain runs only the server op and never re-applies the local change, and it converges a crash-replayed not-found rather than failing it.
The CLI (`mp archive`, `mp delete`) enqueues through the same `apply_*` and then runs the op synchronously with `pending_ops::run_and_settle`, keeping its blocking UX: a success retires the row, a refusal rolls the local half back and returns the error verbatim, so a not-found stays byte-identical to the pre-queue message.
The synchronous settle deliberately does *not* converge a not-found, because a CLI invocation runs the op once in the process that enqueued it and so is never a crash replay.

### Send

`send::send_draft(&EmailDraft, &SendContext) -> SentDraft` is the one orchestration behind `mp send`, `mp send-approved` and both TUI send keys: it builds the bytes, commits the outbox row, submits over SMTP or Graph depending on which the context names, and retires the draft file.
Callers keep only what differs between them, the confirmation prompt, the wording of the result and the exit code.
Every caller reaches it through the daemon since P6-U2: the TUI's two send keys are `send.draft` and `send.approved` like the CLI's two commands, and the undo-send window in front of them is the daemon's scheduler rather than a timer in whichever client was open.

A reply or forward draft names its source in `in_reply_to:` / `forwarded_from:`, and `send::mark_source_after_send` is the one reader: after a successful submission it flags every local copy of that source `\Answered` or `$Forwarded` and enqueues the server half on the durable queue as a single `ServerOp::SetAnswered` naming every server folder the source is filed in (#TKT-0051, #0076).
The send path opens no IMAP session for this: it costs one `COMMIT`, and the drain writes the flag in every named folder over one session (`imap_client::add_flag_in_mailboxes`) at the next resume point.
Best effort throughout, which here is a durability statement: the enqueue happens strictly after delivery, touches no `outbox` row, and every error is logged and swallowed, so bookkeeping can neither fail nor re-send a message that already went out.
The op's rollback is `Rollback::None` on purpose: the answered bit records something that happened, so a server refusal is not a reason to un-say it; the next sync restates whatever the server holds.
A Graph account writes the local bit and queues nothing (answered lives in extended MAPI properties, #0042/#0055).

`src/send.rs` builds the message, then `DurableSend::begin` commits the raw bytes as a blob and a `pending_send` outbox row *before* SMTP opens.
Submission is per recipient: each recipient gets an individual envelope while the visible To and Cc headers are preserved for all, which gives per-recipient success and failure tracking.
`src/outbox.rs` owns the four-state machine (`pending_send`, `sent_pending_append`, `done`, `failed`) and the exactly-once marker: `submission_started_at` is committed immediately before the SMTP session opens, so a `pending_send` row found on restart says whether the transport was ever entered.
Rows that provably never reached it are resubmitted; rows that died inside it are parked in `failed` for a human and never auto re-sent.
The APPEND to the server's Sent mailbox is retried until acknowledged, and any attempt that is not the row's first searches Sent by Message-ID before appending so it cannot duplicate.
`attempts` is incremented immediately before the request goes out rather than after it comes back (#0116), which is what makes an attempt whose process died look like the retry it is.
The drain itself runs under the per-account engine lock (`outbox::drain_guarded`), because every send drains the account to file its own copy and unguarded drains APPEND each other's rows; a drain refused the lock does nothing, and the holder sweeps again to file what it left.
Accounts whose server files its own Sent copy (Gmail, Graph, Proton) skip the APPEND entirely.
A fully sent draft with a durable record behind it is removed from `drafts/`; anything less keeps its file.

Because SMTP runs once per recipient, a submission has one verdict per recipient rather than one verdict (#0063).
The verdicts are committed to the row's `envelope` column: `delivered` is what a retry skips, `rejected` is what the user is told about, and what is in neither is what the next pass attempts.
A recipient that answered 250 is therefore never spoken to twice, whatever else the pass did, and a 5xx stops that recipient instead of being retried forever.
A row with a recipient that gave no verdict at all is parked in `failed` as before; a row that reached some recipients and was refused by others reaches `done` and keeps a note in `last_error`, which is what keeps it listed by `mp outbox list` and counted in the TUI's outbox badge until it is discarded.

One draft is one submission at a time.
Every build mints a fresh `Message-ID`, so a second send of the same draft would look like an unrelated message to both the outbox and the Sent dedup search; the envelope therefore carries the draft key, `outbox::enqueue` refuses a draft that already has an open row, and `send_draft` holds a process-wide slot per draft so the TUI's cursor send and approved batch cannot both submit it.

## Sync backends

Two transports, one ingest path and one `SyncResult` shape: IMAP/SMTP for password and OAuth2 XOAUTH2 accounts, Microsoft Graph REST for tenants that block IMAP/SMTP (see [auth.md](auth.md)).
TUI actions branch on `app.is_graph()`.

The shared half is `src/sync/` (#0059): the sync types (`SyncTarget`, `SyncResult`, `FreshObservation`, `MailboxFetch`), the `SyncBackend` trait, and `sync::engine::run_sync`, which is the orchestration itself: skip lists, ingest, arrival marks, the #0074 ingest-failure bound, flags, cursors and the deferred prune pass.
Since #0122 the live callers reach it through `sync::engine::run_sync_guarded`, which runs the pass under the per-account engine lock: a process that cannot take the lock returns `Ok(None)` before the transport is touched, so a second `mp sync` or an open TUI no longer downloads and ingests the same window into the same store.
`run_sync` itself stays lock-free, so the fake-backend engine tests drive the mechanism without a lock file, and the refusal is a success everywhere (`mp sync` prints `Sync skipped: another engine is syncing '<account>'; leaving the ingest to it` and exits 0, the TUI shows the same sentence as an info status line).
`SyncBackend` has one method, `fetch_targets`, and takes `&mut self`, which is where a backend keeps what outlives a mailbox (a persistent session and its `HIGHESTMODSEQ`, #0041; a `deltaLink`, #0042).
The seam's first payoff is that the engine is driven by a fake backend in `src/sync/engine.rs`'s tests, offline, over the properties that used to be verifiable only against a live server.
`SyncBackend::fetch_targets` is a native async fn in the trait, so its future is not `Send`; callers await it in place, and spawning a sync onto another task would need a `Send` bound first (noted in #0041).
The parity half of #0059 is parked with the Graph backend: `graph.rs` still runs its own loop rather than the engine, and #0042 deliberately landed the Graph delta in that loop rather than folding first (its "Shape" section carries the reasoning).

### IMAP

The backend is `ImapBackend` in `src/imap_client/store_sync.rs`, and `sync_mailboxes()` is now the wiring that hands it and the store to `sync::engine::run_sync_guarded`; it returns `Option<SyncResult>`, `None` being the refused lock (#0122).
The Graph loop (`graph::sync_mailboxes_graph`) is still unguarded, as it is still outside the engine.

IMAP sessions are persistent and shared, not one per operation (#0041, owner-approved rewrite of the old invariant).
`src/imap_client/pool.rs` keeps authenticated sessions for the life of the process, keyed by `host:port/username`; every IMAP path borrows one with `pool::checkout()` and returns it by dropping the guard, so a sync, a queued archive and a post-send flag write no longer each pay TCP + TLS + LOGIN.
What makes reuse safe: every borrower `SELECT`s (a borrowed connection's selected mailbox is whatever the last one left), a session idle over 20 s is `NOOP`-probed on checkout and one idle over 10 min is dropped rather than probed, connecting retries with backoff, and a borrower whose op failed poisons the session instead of returning it, because a half-read response would be misread as the next borrower's answer.
The IDLE watcher (`watch.rs`) is deliberately unpooled: IDLE blocks its connection for its whole duration, so it takes a dedicated one, which is the ticket's second connection.

IMAP allows one SELECTed mailbox per connection, so the mailboxes are fetched in parallel, each on its own borrowed session, up to `imap.fetch_concurrency` at once (default 4, clamped to [1, 8]); #0005.
The store reads that seed each fetch happen serially first, the network fetches overlap, and ingest runs serially in target order afterwards, so `buffered` (which preserves input order) keeps the #0072 prune ordering and the single-writer SQLite discipline intact.
Per mailbox, `UID SEARCH ALL` gives the UID list, the last `limit` UIDs are the window, pass 1 fetches `(UID FLAGS)` over the whole window and pass 2 downloads `BODY.PEEK[]` only for UIDs the store does not hold.
The store answers "which UIDs do I hold" with one query, so there is no local scan and no dedup pass.
Pass 2 is chunked (20 UIDs per `UID FETCH`, newest chunk first) and, on a TUI tick, bounded by `imap.body_fetch_deadline_secs` per mailbox (default 30, clamped to [0, 600], 0 unbounded); `mp sync` passes no budget (#0113).
The budget is checked between chunks and never inside one, because abandoning a `UID FETCH` mid-stream would poison the pooled session; a pass that stops sets `MailboxFetch.bodies_complete = false`, which suspends every prune in the pass and blocks the modseq, and the rest resumes from the cursor on the next pass.
IMAP supports implicit TLS (port 993) and STARTTLS (any other port, for example 1143 for Proton Bridge); the `ImapStream` wrapper injects a fake greeting for STARTTLS because `async_imap` expects one.

### Graph

The client and its orchestrator are `src/graph.rs`.
The folder enumeration returns every message's `internetMessageId`, read flag and received date; the messages the store does not hold are downloaded by id, twenty per `/$batch` call, newest first so a capped pass still takes the arrivals a user is waiting for.
Graph never returns RFC822, so rows get `raw_blob` NULL and the HTML part is stored as an `html` blob instead.
Graph has no UID, so the row's `uid` is a 63-bit hash of the Message-ID (`ingest::graph_uid`), which keeps the `(account, mailbox, uid)` identity meaningful.
Since #0055 the orchestration mirrors the IMAP one line for line, prune pass included.
The enumeration is keyed on the trimmed `internetMessageId`, walks the folder newest-first, and reports whether it saw all of it; #0065 turned that report into the prune's precondition.

Since #0042 a quick sync may replace that enumeration with a `/messages/delta` walk from the token in `sync_cursors.deltalink`.
The token means "at the moment it was minted, the store held every message the folder listed", and every rule around it exists to keep that true: it is minted with `$deltatoken=latest` *before* the enumeration it is stored alongside, only by a pass that saw the whole folder and wrote every message in it, and only together with the folder id it is bound to.
A full sync always relists, which is the periodic whole-folder observation the prune leans on; a quick sync takes the delta only on an exact match (`delta_verdict`), and 410, 404, an unparseable page, a page-cap and a chain that ends without a `@odata.deltaLink` all throw the token away and enumerate in the same pass.
Graph's UIDVALIDITY equivalent is the folder id: a token is bound to one, so an `Archive` deleted and recreated under the same config is a different folder and its token is dropped.
Deletions are the one thing the delta does not resolve: a `@removed` entry names the message by Graph id and the store keys Graph rows on `internetMessageId`, so a pass whose delta reports a removal escalates to the full enumeration and the prune keeps its existing `known − enumerated` source of truth with the #0065/#0072/#0074 gates on unchanged inputs.

### Watchers

One IMAP IDLE round per password or OAuth2 account, and one polling round per Graph account that compares the *set* of inbox ids rather than its cardinality.
Both live in the daemon's account runtime since P5-U8 (`src/daemon/runtime/watcher.rs`), one per account beside its tick and its read pool, and both widen their retry interval after consecutive failures instead of hammering a server that is down.
A round that sees the mailbox move runs one quick tick and publishes what changed as events, so every connected client converges without a tick-specific rule; a runtime whose engine lock is held by another process does not watch, because the engine holding it is watching the same mailbox.
Changes on a non-active account set `has_unseen` in the TUI, which is the badge in the status bar.

## Module map

| File | Responsibility |
|------|---------------|
| `mp-core/types.rs` | Shared types: `EmailStatus` (the three draft states), `MessageFlags` (the received-mail status axis: seen, answered, forwarded), `MailboxRole` (the store's mailbox key), `EmailFrontmatter`, `EmailDraft`, `EventFrontmatter`, `collapse_hyphens` |
| `mp-core/config.rs` | Config loading (`~/.config/mailypoppins/config.toml`), `config_dir` + the one-time #0022 legacy move, secrets-backend dispatch, data dir helpers (`mailypoppins_data_dir`, `account_dir`, `store_path`, `blobs_dir`, `drafts_dir`, `tokens_dir`, `logs_dir`, `contacts_cache_path`), legacy-config rejection, logging init |
| `mp-core/signatures.rs` | App-managed signature files (#0107): one Markdown file per signature at `config_dir()/signatures/<name>.md`, the file stem being both key and display name. Name validation, list/read/write/create/rename/delete, the per-account default via `app_state`, and `migrate_config_signatures`, the one-time copy out of the legacy `[accounts.*.signatures]` tables. |
| `mp-core/app_state.rs` | App-owned state that is not user-edited config (#0107): `<data_dir>/state.json`, pretty-printed JSON, load/save modelled on `contacts::cache`. Holds the per-account default signature; a missing or corrupt file means "nothing recorded" and is never fatal. |
| `mp-core/secrets.rs` | Machine-bound encrypted secrets store (ChaCha20-Poly1305 + HKDF-SHA256). `SecretsBackend` trait with `EncryptedFileBackend` (default) and `KeyringBackend` (opt-in). See [secrets.md](secrets.md). |
| `mp-core/oauth2.rs` | OAuth2 device-code flow, encrypted token cache at `tokens_dir()/<account>.enc`, refresh, XOAUTH2 SASL builder. Scope-parameterised (`IMAP_SMTP_SCOPES` vs `GRAPH_SCOPES`). |
| `src/ingest.rs` | The receive-path writer: fetched message to one `messages` row plus blobs, FTS maintenance, cursors, `prune_vanished`, `apply_seen_flags`, `graph_uid` |
| `mp-core/search.rs` | The unified search grammar (#0086a): one parser (`parse`) to one AST (`Query`/`Clause`/`Term`, plus the `in:`/`message-id:` directives), the CLI-flag builder (`from_cli`), and four renderers (`to_imap` with a `has:attachment` post-filter split, `to_gmail`/`to_gmail_search_command` for `X-GM-RAW`, `to_graph` for `$search`/`$filter`, `to_fts` for the local index). Malformed queries return a caret-pointed `ParseError`. `fts_expression` survives as a thin renderer wrapper. `mp-core/imap_query.rs` beside it holds the three pure IMAP string helpers it shares with `imap_client::search` (`normalize_message_id`, `bracketed_message_id`, `parse_date_to_imap`). |
| `mp-core/selector.rs` + `src/selector.rs` | The `mp://account/mailbox/key` grammar: parse, resolve, format. Namespace fixed by the command, never sniffed. Split at the resolvers: the grammar is in `mp-core`, the two indexed lookups stay beside the store. |
| `src/dump.rs` | `mp dump-mailbox`: path-free NDJSON envelope dump of the store, the parity harness for the data-layer rewrite |
| `src/read_cmd.rs` | `mp show`, `mp list-messages` (#0062) and the `mp search --local` listing (#0043): the human read surface over `store::read` and `store::search`, offline, rendering to a `String` so the layout is testable. Not the dump: that is an oracle with a pinned record shape. |
| `src/cutover.rs` | `mp cutover` (#0040): the end of the file-era transition. Mints an `id:` into any draft that has none (the one-time draft "import"; the drafts directory never moved) and reports the dead file-era mailbox directories. Deletes nothing, by design. |
| `mp-core/reconcile.rs` + `src/reconcile.rs` | iMIP invite reconciliation, folded over the rows at display time and never persisted: attendee `PARTSTAT`s (#0030) and, since #0031, the `(UID, RECURRENCE-ID)` cancellation/version fold (`fold_status`) that marks an event cancelled, superseded, or missing individual occurrences. Split at the readers: the fold is in `mp-core`, the three functions that open a store and read blobs stay. |
| `src/agenda.rs` | The local agenda (#0034): one row per `(UID, RECURRENCE-ID)` over the account's invite rows, deduped by `(sequence, dtstamp, is_organizer, mailbox, uid)`, sorted by start with undated last, answered as `mp_protocol::calendar::AgendaEvent`. It is what `calendar.events` serves and what the TUI decodes; it was `tui::app::calendar_view` until #0126 (P5-U10c-I2). |
| `mp-core/parse.rs` | RFC822 parsing, attachment extraction and sanitisation, `inline_images` and `embed_inline_images` (the `cid:`-referenced image parts, inlined as `data:` URIs for the browser view and the `.html` companion; the in-pane rendering they were written for was retired by #0109), `open_file_with_system()`, `materialisation_dir()`, `stable_attachments_dir()`, `ensure_utf8_charset()` |
| `mp-core/draft.rs` + `src/draft.rs` | Draft parsing and validation, reply and forward creation, status transitions. Split at what needs an index, a row or an outbox record: the file format is in `mp-core`, `new_draft_skeleton`, `source_from_row`, `create_draft_from_source`, `settle_sent_draft` and `delete_indexed_draft` stay. |
| `mp-core/addresses.rs` | RFC 5322 address strings: `split_addresses`, `normalize_address_for_smtp`, `quote_display_name`, `format_recipient`. Out of `send` in P5-U10b, which re-exports them, because `draft`'s validation reads them and wants no transport. |
| `src/send.rs` | `markdown_to_html`, message building, `send_draft` + `SendContext`, per-recipient submission, `DurableSend`, `resume_outbox` |
| `src/outbox.rs` | The durable send state machine and its blob refcounting |
| `src/ops.rs` | `ServerOp` (the remote half of a mutation) and its IMAP/Graph execution seam `run_op`, at library layer so the durable queue and the CLI can drive it without depending on `tui/` |
| `src/pending_ops.rs` | The durable mutation queue (#0039): atomic local-write-plus-enqueue, the drain with backoff and per-kind rollback, crash-replay, `resume_account` (sync-tick drain) and `run_and_settle` (the CLI's synchronous single-op path) |
| `src/engine_lock.rs` | One engine per account across processes (#0061): a non-blocking `flock` on `<account_dir>/store.lock`, released on exit or crash; taken by the `pending_ops` drain, by the outbox drain (#0116) and by the IMAP sync ingest (#0122) |
| `src/graph.rs` | Microsoft Graph REST client: folders, fetch, sync, send, move, delete, read flags, search |
| `mp-core/calendar.rs` + `mp-core/invite.rs` + `src/invite.rs` | iCalendar receive-side parsing and send-side building. `invite` is split at `plan_invite`, which reads the sending account. |
| `mp-core/contacts/` + `src/contacts/` + `src/contacts_cmd.rs` | Contact index built from `messages` rows, frecency ranking, per-account cache at `account_dir(name)/contacts-cache.json`. CLI: `mp contacts {rebuild,stats,list}`. Split at the store read: the cache, the filter, the ranker, the matcher, the vCard writer and the observation merge are in `mp-core`, the rebuild and the send/sync hooks stay. |
| `src/config_cmd/` | Config subcommands: init wizard, add-account, show, set-password, oauth2-login, reset-secrets, path |
| `src/calendar_cmd.rs` | `mp calendar rebuild`: reports what the invite fold resolves, writes nothing |
| `mp-core/notify.rs` | Desktop notifications for new mail, shelling out to `osascript` / `notify-send` |
| `mp-core/sync_health.rs` | `SyncHealth`, the per-account outcome of the last sync, plus the `mp sync` failure summary and exit code (#0071) |
| `mp-core/timing.rs` | `TimingSpan`, which emits `[TIMING]` log lines with millisecond precision. Filter logs with `rg '\[TIMING\]'`. |
| **`src/sync/`** | |
| `mod.rs` | The transport-independent sync types and the `SyncBackend` trait (#0059) |
| `engine.rs` | `run_sync`: the orchestration every backend is driven through, plus `run_sync_guarded`/`run_sync_guarded_at` (the engine lock on the ingest path, #0122), `mark_below_unmet` (#0074) and the fake-backend engine tests |
| **`src/store/`** | |
| `mod.rs` | `Store`: the file, the pragmas, the drop-and-rebuild contract |
| `schema.rs` | Schema v6 SQL, version stamping, required-table validation, and the identity notes |
| `read.rs` | Listings, counts, Message-ID lookup, body and HTML loading, `materialise_attachments` |
| `search.rs` | Full-text search over `messages_fts` (#0043): `search`/`search_ast` join the index, `crate::search::to_fts` renders the `MATCH` string plus the attachment/date SQL predicates, then bm25 ranks; `fts_expression` and `index_drift` |
| `write.rs` | The optimistic local half of a flag, move or delete |
| `drafts.rs` | The derived index over `<account_dir>/drafts/` |
| `blobs.rs` | The content-addressed blob store and its refcount discipline |
| `sweep.rs` | The retention sweep (#0060): the over-cap two-strike marker, the eviction order, `mp store gc` |
| **`src/imap_client/`** | |
| `mod.rs` | `ImapStream` wrapper, `open_imap_session()`, re-exports |
| `pool.rs` | The persistent session pool (`checkout`, `PooledSession`) and `ServerCaps`, the strict post-LOGIN capability gate (#0041) |
| `fetch.rs` | `fetch_new_raw_on_session` (the two-pass store fetch), `vanished_uids`, `fetch_emails*`, `search_on_session` (runs a pre-rendered `SEARCH`, the seam the unified grammar lowers to), the arrival-coverage arithmetic |
| `store_sync.rs` | `ImapBackend` (the `SyncBackend` impl: the parallel per-mailbox fetch), `sync_mailboxes()`, `list_mailboxes()` |
| `search.rs` | `build_imap_search_query()`, `FetchCriteria` (the structured direct-lookup path; the user grammar moved to `crate::search`, and `normalize_message_id` / `bracketed_message_id` / `parse_date_to_imap` moved to `mp_core::imap_query` and are re-exported from here) |
| `watch.rs` | `watch_mailbox()` (IMAP IDLE) |
| `ops.rs` | Single-message server ops: move, delete, read flags |
| `batch.rs` | `batch_move_on_server`, `batch_delete_on_server` |
| `sent.rs` | `ImapSentMailbox`: the APPEND seam the outbox drives, faked in tests |
| **`src/tui/`** | |
| `mod.rs` | Event loop (`run_loop`), the session and event-stream drain, background result drain. One iteration drains the queued terminal events and the queued daemon events into the model and then paints once (#0108), both bounded by `MAX_COALESCED_EVENTS` and `COALESCE_BUDGET`, stopping early on an action that `Action::suspends_terminal()` flags. |
| `session.rs` | The one daemon connection: a thread with a current-thread runtime on it, `call` / `dispatch` / `events`, and `handle()`, the weak-sender door a worker thread owns |
| `queries.rs` | Every read, as typed functions over the object-safe `Queries` trait, plus the uid index and the row-delta decoder |
| `commands.rs` | `route()`, the exhaustive `Action` classification table, and `dispatch()`, which turns a daemon-routed action into its calls |
| `events.rs` | `Incoming`, the subscription drain, `App::apply_event` and the watermark, and the rebootstrap a resync or a reconnect costs |
| `actions.rs` | `handle_action()`, the side-effect dispatch for the `Action` variants `commands::dispatch` hands back: the client-only ones and the overlays |
| `bg.rs` | `handle_bg_result()`, processing background task completions, and `land_sync`, shared by a tick this client asked for and one it heard about |
| `helpers.rs` | Terminal suspend and resume, editor, clipboard, `resolve_send_account`, and the server search leg (`LST-08`) that is still client-side |
| `test_daemon.rs` | `TestDaemon`, the in-process daemon every TUI test module builds its fixture on |
| `event.rs` | Crossterm event polling: `poll_event` waits up to the 250 ms tick, `poll_pending_event` takes an already-queued event without waiting (the drain step, #0108). Both return `None` for an event we do not model. |
| `theme.rs` | Named themes, semantic colour slots |
| **`src/tui/app/`** | |
| `mod.rs` | `App` struct, `new()`, `update()`, account sync, core state helpers |
| `bootstrap.rs` | `App::shell`, `App::from_bootstrap` and the two `apply_bootstrap` entries (startup, which skips an account that already opened, and resync, which does not) |
| `store_rows.rs` | The store-backed readers the query layer replaced, kept as the equality oracle and as what an `App` with no session reads |
| `types.rs` | `EmailEntry`, `AccountState`, `BgResult`, `Action`, `Focus`, `MailboxKind`, `open_store`, mailbox builders |
| `keys.rs` | `handle_key()` dispatch and all `handle_*_key()` methods |
| `keymap.rs` | The single `KEYMAP` table behind the help overlay, the hint bar and `mp dump-keys` |
| `jump_date.rs` | The closed date grammar behind jump-to-date (`g t`, #0017): pure, clock-free, `parse_jump_date(input, today)` |
| **`src/tui/ui/`** | |
| `mod.rs` | `view()`, the top-level layout dispatch, including the #TKT-0044 pane zoom (one pane over the whole content area, hint and status bars kept) and the shared overlay dispatch |
| `views.rs` | View switcher chrome |
| `sidebar.rs`, `list.rs`, `headers.rs`, `preview.rs`, `compose.rs`, `status.rs`, `activity.rs` | Mail view panes |
| `calendar.rs`, `contacts.rs` | The other two views |
| `overlays.rs`, `search.rs` | Confirm dialog, attachment picker, persistent error, help overlay, server search, and the signatures manager (`cs`, #0107): the signature files with the default starred, `Enter` to set or clear it, `e`/`n`/`r`/`d` for edit, create, rename and delete, every mutation going through `crate::signatures`. The server-search overlay (`f`) is the Outlook-shape form (#0086b): a scope toggle, `From`/`To`/`Subject`/`Keywords` text fields, custom `After`/`Before` dates, an attachment toggle, and an `Advanced` raw-grammar line. The form builds a `search::Query` AST directly (via `SearchForm::build_query` -> `search::from_cli`, no string concatenation) and `Action::ServerSearch` carries the parsed `Query`; a non-blank `Advanced` line takes over and greys the structured fields. |
| `widgets.rs`, `util.rs` | Shared widgets, `pane_border_style`, `hint_span`, `truncate` |

## TUI layering

Since Phase 5 of the daemon migration (#0124) the TUI is a client of the daemon rather than a caller of the engine.
It still lives in the root package, under `src/tui/`, because the crate move is deferred; what changed is where its reads, its writes and its watchers happen.

### The shape

- The TUI follows The Elm Architecture.
`App::update()` is a state machine (`Message -> State`).
Side effects are dispatched as `Action` variants and executed either by `tui/commands.rs::dispatch()`, which turns the action into daemon calls, or by `tui/actions.rs::handle_action()`, which owns the ones no daemon can perform.
- `ui/` renders from `App` state only.
It opens no store, runs no SQL and performs no I/O.
- `app/` is not pure in that sense, but what it does now is call the daemon rather than open a database.
The protocol boundary stays absolute: no SMTP, IMAP, MIME or Graph code in `app/` or `ui/`.
- Account state proxy pattern.
`App` holds a `Vec<AccountState>` plus top-level proxy fields (mailboxes, list index) that mirror the active account, with `save_to_account()` and `load_from_account()` syncing on switch.
This avoids routing every key handler through indirect access.
- Mutations are optimistic and stay so: the daemon commits the row change and the owed server op in one transaction and lets the next sync tick drain it, which is what `settle: false` on the five message mutations means and why a thousand-row selection costs no network (#0039, P5-U6).

### The session thread

`src/tui/session.rs` owns the one connection.
It is a thread of its own with a current-thread tokio runtime on it, because `mp`'s `main` is already inside a runtime that `run_loop` cannot block on, and a `Connection` holds a `UnixStream` registered with the runtime that created it.
The UI thread talks to it over channels: `dispatch` posts and forgets, `call` blocks for the answer, and `Session::handle()` hands a worker thread a `QueryHandle` holding a weak sender, so quitting closes the channel under every worker instead of joining on one.

The session comes up **before** the terminal does.
`client_session` may start a daemon and may end the run with the exit-4 diagnostic, and neither reads well through a terminal already in raw mode on the alternate screen.
A first paint therefore costs a connect and a handshake, and on a cold start the daemon's own start as well; `docs/baselines/phase5-gate-evidence.md` measures both.

The thread `select!`s over the call channel and the notification stream, so an event does not wait for the next keystroke.
A closed socket refuses every in-flight call at once rather than waiting out the 30 s ceiling, posts `Disconnected`, and retries `client::reopen_session` on a widening gap from 250 ms to 2 s.

### Queries

`src/tui/queries.rs` is every read.
`Queries` is an object-safe trait with one method, `call`, implemented for `Session` and for `QueryHandle`, so a query layer is testable against an in-process `Dispatcher` without a socket.
Over it sit the typed readers the call sites need: `list_emails` (`message.list`), `mailbox_counts` (`mailbox.list`), `message_body` (`message.get`), `thread` (`message.thread`, P5-U10d), and the three invitation reads P5-U10 added (`calendar.events`, `message.ics`, `message.invite`).

A wire row becomes a `MessageRow` and goes through `entry_from_row`, the same function the store-backed path uses, so the two are equal by construction rather than by inspection.
A held list is keyed by `messages.id` and the daemon removes a row by `(mailbox, uid)`, so the query layer keeps a process-wide `(account, mailbox) -> (uid -> id)` table; a uid it does not know owes a refetch rather than a guess.

### Commands

`src/tui/commands.rs` is every write.
`route()` classifies all fifty `Action` variants exhaustively, with no wildcard arm, into `Daemon` (the methods it issues, in issue order), `ClientOnly` (editor, browser, clipboard, file picker, terminal suspend) and `Local` (pure UI state).
`dispatch()` returns `true` when it handled the action and `false` when `handle_action` still owns it; a refusal from the daemon is not a `false`, it lands on the status line exactly as a refused store mutation did.

An operation-kind method (`sync.quick`, `sync.full`, `send.approved`, `calendar.rsvp`) answers `{operation_id}` at once and finishes later.
The arm records the id against what it is awaiting and returns; there is no worker thread and no poll.

### Events

`src/tui/events.rs` replaced the two watcher threads.
`Incoming` carries the decoded events *and* the connection's own state (`Resync`, `Disconnected`, `Reconnected`) on one channel, which is what keeps a reconnect from overtaking the last event of the dead instance.
`drain()` runs in the same pre-draw pass as the terminal drain and is held to the same two bounds, `MAX_COALESCED_EVENTS` and `COALESCE_BUDGET`, so a first sync of a large mailbox publishing a row per message cannot starve the paint.

`App::apply_event` consults the watermark before it looks at a kind: an event above it is applied and moves it, one at or below it is a duplicate the snapshot already carries, and one from an instance this client never bootstrapped against is refused, stickily.
`App::apply_bootstrap` is the only thing that sets the watermark, which is why a resync and a reconnect are both spelled "bootstrap again"; `App::apply_resync_bootstrap` is the recovery entry and, unlike the startup one, it replaces the mailboxes, the counts and the listing caches of an account that had already opened.

### The watchers are the daemon's

`imap_watch`, the Graph poller, `watcher_loop` and `WatchEvent` are gone from `src/tui/`; `src/daemon/runtime/watcher.rs` is where they went.
One IDLE round of 300 s per IMAP account and one 60 s enumeration per Graph account, the same numbers and the same backoff curve the TUI's threads used, and a round that sees the mailbox move runs one quick tick and publishes the counts that moved with it.
`AccountState::watcher_active` keeps its name and means the daemon session's health now.

### The sessionless store fallback

`src/tui/app/store_rows.rs` holds the store-backed readers the query layer replaced: `load_emails`, `count_all_emails` and `App::load_message_body`.
They have two callers and no third.
They are the equality oracle `queries_tests.rs` and `invites_tests.rs` compare every daemon-backed answer against, and they are what an `App` with no session reads, which is every one of the ~370 sessionless-`App` unit tests and, in a real run, only a `Session::connect` that wedged for 30 s.

That fallback is **not** the direct fallback the plan forbids: nothing recovers a *failed* daemon call by reading the store.
A failed call degrades exactly as it did before, as an empty list, a zeroed count, an empty preview and a line in the log, and `tests/tui_daemon_recovery.rs` asserts it as a lock, by taking the account's engine lock during the outage from a second open file description.

### The residue, at five imports and five paths

`tests/fixtures/tui-engine-imports.txt` is the engine-import allow-list, 5 pairs over 4 files, and `tests/fixtures/tui-engine-paths.txt` is the engine-path allow-list, 5 rows over 4 files.
Both are the migration's progress bar: a removed row fails the test as loudly as a new one.
The plan drives both to zero in P5-U10, which is in progress; everything left waits on the crate move rather than on a method.

- `app/mod.rs store`, `app/store_rows.rs store` and their two path twins are the sessionless readers above. They die with the crate move, when the tests that need them move to the root crate, not with a new method.
  P5-U10b routed the last reader that had no daemon-backed twin: `App::draft_body` calls `draft.path` and parses the file the daemon names, and `load_draft_body` stays as its oracle. That one opened the store with `Store::open` rather than `open_store`, so `TUI_APP_STORE_RESIDUE` never listed it and still does not.
- `app/types.rs store` is three `#[cfg(test)]` imports plus `row_to_wire`'s signature and `indexed_drafts`, the Drafts oracle; `app/types.rs ingest` is a test module. Both die with the move.
- `actions.rs store` is three `#[cfg(test)]` imports and nothing else: the arms themselves reach no store since P5-U10c-I2.
- `app/mod.rs crate::agenda::` is the sessionless agenda oracle, and `session.rs crate::daemon::` is the connect helper, which has nowhere better to live until `crates/mp-tui` exists.

`mod.rs store`, the drafts-directory poll's `drafts::fingerprint` / `drafts::refresh_account`, went in P5-U10d-I with the poll itself, and so did the ten paths that guard recorded at 15: the two editor-return refreshes, `create_draft_from_source`, `new_draft_skeleton`, the three `crate::outbox::` sites of the dead badge, `app/keys.rs`'s thread read, and both `crate::send::format_recipient` spellings.

`queries.rs store`, `helpers.rs store` and `helpers.rs imap_client` went in P5-U10c-I1, with the four surfaces: a listing row is `mp_protocol::listing::MessageListRow` and a draft row is `mp_protocol::draft::DraftEntry`, and the server search leg is two daemon operations.
`app/calendar_view.rs store` went in P5-U10c-I2, with the agenda loader itself: it is `src/agenda.rs`, in the crate that owns the store it reads, and it answers `mp_protocol::calendar::AgendaEvent` to `calendar.events` and to the TUI alike.
`actions.rs send` went in P6-U2: the undo-send hold and the send behind it are `send.draft` now, so the action layer's last `crate::send` import left with them.

`src/tui/actions.rs` carries a second, narrower allow-list of its own, `TUI_ACTION_ENGINE_RESIDUE` in `src/tui/actions_tests.rs`, where the import list says which file and this one says which function still opens a store.
**It is empty since P5-U10c-I2**: the `OpenEventSource` arm reads the row's `invite.ics` through `message.ics` like every other reader, and `store_for_mutation` died with it.
The table stays as the gate, because an empty one fails on the first engine call anyone adds back.

### The crate move, in progress

The plan's shape is `crates/mp-tui` depending on `mp-client` and `mp-protocol` and on nothing else.
The obstacle is not the engine residue above; it is the shared modules the allow-list deliberately does not scan.
`src/tui/` reaches twenty root-crate modules, fourteen of which (`config`, `parse`, `types`, `selector`, `search`, `contacts`, `draft`, `signatures`, `notify`, `timing`, `invite`, `calendar`, `sync_health`, `reconcile`) are not engine modules at all, and their own closure was about 15 000 lines across sixteen modules before any of it moved.
The three-unit sequencing that does it is in `docs/tickets/0124-tui-cutover.md`.
**P5-U10a and P5-U10b's splits landed** (#0126): eleven of those modules whole, and the engine-free halves of `selector`, `search`, `invite`, `reconcile`, `contacts` and `draft`, are `crates/mp-core` above.
**P5-U10c-I1's four surfaces landed too**: `RD-06`'s `message.materialise_markdown`, `RD-07`'s `selector` on the listing row, and `LST-08`/`LST-09`'s `message.search_server` and `message.fetch`, which took the allow-list from ten rows to seven and the action residue from seven to two.
**P5-U10c-I2 took the contacts refresh onto `contact.rebuild`, the agenda out of the TUI and the invite blob onto `message.ics`**, which is the import allow-list at six and the action residue at zero.

The move itself did not land there, and the reason is worth recording rather than rediscovering: the import allow-list is a `use` scan, and the calls that block the move are mostly fully-qualified paths it never sees.
P5-U10d-T wrote them down as a second guard (fifteen rows over six groups) and P5-U10d-I closed four of the six: `message.thread` for the conversation overlay, `draft.create_from_message` for the reply to a server-only hit, the drafts poll against the watcher and `draft.list`'s fresh scan, and the outbox badge as a deletion, since `AccountState::outbox` was written twice and read nowhere.

What is left of the production call sites in `src/tui/` that a `crates/mp-tui` could not compile:

| group | where | what it needs |
|---|---|---|
| the sessionless oracles | `app/store_rows.rs`, five readers in `app/mod.rs`, `row_to_wire` and `indexed_drafts` in `app/types.rs` | the test move, and the decision to drop the fallbacks the plan's "no direct fallback" already implies (P5-U10e) |
| the connect helper | `session.rs` | `daemon::client::{client_session, reopen_session}`, the exit-4 diagnostic the CLI shares; the recommendation is that the binary injects a connector, which is the crate move's own decision (P5-U10f) |

One consequence to carry into those units: `secrets` and `oauth2` are named in `ENGINE_MODULES` and now live in `mp-core`.
No file under `src/tui/` imports either, so the allow-list did not move, but a `crates/mp-tui` depending on `mp-core` would be able to reach both without the textual scan (which looks for `use crate::` / `use mailypoppins::`) ever seeing it.
The unit that moves the crate has to decide whether they leave that list or whether the scan learns about `mp_core::`.

## Multi-account

Config uses an `[[accounts]]` array.
Each account has independent IMAP/SMTP settings and mailbox mappings, and its own store, blob directory and secrets keys (`smtp-password-{name}`, `imap-password-{name}`).
Signature files are shared across accounts; what is per-account is which one is the default (`app_state`).
The TUI shows one account at a time, switching via backtick or Ctrl+1-9, and watches all of them for new mail simultaneously.
CLI commands target an account via `--account` and default to the first.

## Selector contract

No CLI input position takes a filesystem path (#0050).
A message is named by `[mp://<account>/][<mailbox>/]<key>`, and the canonical form every command prints is the fully qualified `mp://<account>/<mailbox>/<key>`.
Elision is positional: without the scheme, the account comes from `-A/--account` or the default account and the mailbox from `--mailbox` or the command's declared default scope.
The namespace (received mail or drafts) is fixed by the command, never sniffed from the string, so a Message-ID that happens to look like a draft id cannot be reinterpreted.
Resolution is one indexed lookup; an ambiguous key lists every candidate and asks for `--mailbox` rather than picking one.

## Performance-critical invariants

These exist for measured reasons; do not regress them without re-measuring.

- **Pass 1 covers the full window.**
The flags it collects are the only server-to-local channel for the whole status axis (`\Seen`, `\Answered`, `$Forwarded`), so any "probe fewer UIDs first and bail early" optimisation silently breaks read/unread sync and the answered/forwarded state with it.
This happened once already (#0004).
IMAP states the whole flag set and is truth for all three bits (`ingest::apply_flags`); Graph knows only `isRead` and merges that one bit in (`ingest::apply_seen_flags`), so a Graph pass cannot erase an `\Answered` no Graph call can restate.
- **The prune is clamped to the window's UID range.**
`UID SEARCH ALL` returns the whole mailbox but the window is only its newest `limit` UIDs, so only a known UID *between* the window's lowest and highest is provably gone from the server.
Negative UIDs (the local-move sentinel) and hash-sized UIDs (an APPEND with no `APPENDUID`) fall outside by construction.
- **The Graph prune runs only on a pass that saw everything, and never on a fresh row.**
Graph enumerates the whole folder, so there is no UID range to clamp to; what stands in for the clamp is that every target must have enumerated in full and downloaded its whole backlog before any prune applies (a capped quick sync defers them), and that a row dated within `ingest::PRUNE_MIN_AGE_SECS` of now is skipped.
The age window is what keeps the prune from deleting the copy of a just-sent message, which the store files under our own Message-ID and the server lists under one of its own (#0065).
- **Prunes run after every target is ingested.**
Targets sync in order, so pruning inside the loop would delete the inbox row of a message archived elsewhere before the archive pass ingests it, leaving a window with no row anywhere and blobs dropping to refcount zero.
Both backends hold their prunes back for this reason.
- **The integrity check is amortised.**
It is a full walk of the file, so it runs once per file per process, not once per open.
- **Read-flag updates land in one transaction per mailbox**, not one commit per message.
- **Queued mutations.**
A mutation enqueues into the durable `pending_ops` queue and applies locally at once, spawning no background job.
Nothing defers a fetch or sync behind it any more: #0039 retired the mutation-count gate and the "Quick sync queued (N ops pending)" stacking it needed, and #0076 removed the vestigial always-zero field it left behind.
The owed server op is drained at the next sync/fetch resume point.

## Data and config layout

User-owned config:

- The config file is `~/.config/mailypoppins/config.toml`, a multi-account `[[accounts]]` array.
  It is user-edited and holds connection and account-level settings.
- The secrets file is `~/.config/mailypoppins/secrets.enc`, machine-bound encrypted (see [secrets.md](secrets.md)).
- The signatures directory is `~/.config/mailypoppins/signatures/`, one `<name>.md` per signature (#0107).
  App-managed rather than user-owned, but it sits under the config dir because a signature is something the user also edits by hand; the selection of a default is app state and lives in the data dir instead.

All three live under `config_dir()`, overridable with the `MAILYPOPPINS_CONFIG_DIR` env var, which mirrors `MAILYPOPPINS_DATA_DIR` and is what the CLI integration tests point at a tempdir.

The directory was `~/.config/email` before #0022, and `config::migrate_legacy_config_dir()` moves it once, at startup in `main()`, before anything reads config or secrets.
This does not contradict the no-migrations invariant: that invariant is scoped to data formats, secret storage and wire protocols, and a directory rename reads not one byte inside the directory.
A hard cut would instead have cost every stored SMTP/IMAP password.
The move is one `fs::rename` and nothing else, which is what makes it idempotent and safe under two concurrent `mp` invocations: the loser of the race gets `ENOENT` and treats old-absent plus new-present as success.
There is no copy fallback and, more importantly, no read fallback: a rename that fails names both paths and the exact `mv` to run and exits 1, because a client that quietly kept reading `~/.config/email` would never finish the move.
Setting `MAILYPOPPINS_CONFIG_DIR` skips the move entirely, since an explicit override must not carry a migration side effect.

App-managed data, all under `mailypoppins_data_dir()`:

| Platform          | Default `mailypoppins_data_dir()`                          |
|-------------------|------------------------------------------------------------|
| macOS             | `~/Library/Application Support/mailypoppins`               |
| Linux (incl. WSL) | `$XDG_DATA_HOME/mailypoppins` (def. `~/.local/share/mailypoppins`) |

Layout under the data dir:

```
<data_dir>/
  accounts/<name>/store.sqlite3          # the per-account store (plus -wal, -shm)
  accounts/<name>/blobs/ab/cd/<sha256>   # bodies, raw messages, attachments
  accounts/<name>/drafts/*.md            # the only local truth
  accounts/<name>/attachments/<message-id>/   # materialised for forward drafts (#0006)
  accounts/<name>/contacts-cache.json
  tokens/<name>.enc                      # OAuth2 / Graph encrypted refresh tokens
  state.json                             # app state (#0107): per-account default signature
  logs/mailypoppins-YYYY-MM-DD.log
```

Nothing under `accounts/<name>/` is created eagerly: `mp config init` makes the account directory, the first sync makes the store and its blob directory, and the first draft makes `drafts/`.
Override the root via the `MAILYPOPPINS_DATA_DIR` env var, which tests use and which doubles as the escape hatch for a portable location.

`retention` is parsed and validated in config but not enforced yet: the blob store grows without bound until #0060 lands.

The OS keyring service name (when the keyring backend is opted into) is `mailypoppins` (constant `KEYRING_SERVICE` in `src/secrets.rs`).
It was `email-cli` before #0022, and `get` falls back to that name so a user who opted in before the rename is not locked out of stored credentials; `set` and `delete` touch the new service only, so the next `mp config set-password` migrates the credential and leaves a harmless stale entry behind.

## Testing

- **2343 tests**, run by `cargo test --workspace`, the parity harness, the six daemon slice suites and the soak file included.
All of them run offline, the plain selection in a few seconds.
- Unit tests are inline `#[cfg(test)] mod tests` in each module; integration tests live in `tests/` and use `tempfile::tempdir()` plus `MAILYPOPPINS_CONFIG_DIR` and `MAILYPOPPINS_DATA_DIR` for isolation.
- `insta` snapshots cover `markdown_to_html`, the whole `mp --help` surface (`tests/cli_help_snapshot.rs`) and the TUI golden frames (`src/tui/ui/golden_frames.rs`).
`cargo insta review` approves changes; a diff there is a decision, not an approval reflex.
- The store side is fixture-driven: `tests/store_ingest_integration.rs` ingests real RFC822 bytes and asserts rows, blobs, refcounts and FTS state; `tests/store_search_integration.rs` asserts the search itself (ranking, phrases, prefixes, unicode, mailbox and account scope) and that the index does not drift across re-ingest, a UIDVALIDITY rebind, a move, a delete and a prune; `tests/outbox_integration.rs` drives the state machine against a fake Sent mailbox.
- The sync engine (`src/sync/engine.rs`) is tested offline against a fake `SyncBackend` (#0059): ingest and cursor advance, the #0074 arrival mark and its give-up bound, the UIDVALIDITY reset, the deferred prune pass and its account-wide coverage gate, `dry_run`, and the flag application.
  `ops.rs` and `batch.rs` still have none: their seam is `ops::run_op`, not this one.
  The Graph orchestrator still has none either, because it does not run on the engine yet.
- There is no IMAP/SMTP mock server.
