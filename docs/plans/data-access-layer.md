---
id: data-access-layer
title: "Data-access-layer redesign: server-as-truth SQLite mirror + local-only drafts"
status: DECIDED (2026-07-14) — implementer-facing plan, staged with a stop-gate
supersedes: ".agents/research/2026-07-12-architecture-rethink-decision-doc.md (Option choice), .agents/research/2026-07-12-perf-sync-improvement-plan.md (Phases 0-2)"
next_free_ticket: 0037
---

# Data-access-layer redesign

The paradigm question is settled.
mailypoppins stops treating local `.md` files as the source of truth.
The server is ground truth for received mail; a local SQLite mirror plus a content-addressed blob store is the client's fast, disposable copy; drafts are the only thing whose truth is local.
This is Option B from the decision doc, with three owner amendments recorded below.

> Do not reopen the paradigm question.
> The decision doc (`.agents/research/2026-07-12-architecture-rethink-decision-doc.md`) is the argument; this doc is the plan.
> If a future session wants the reasoning, read the decision doc; if it wants to build, read this.

## The decision, and the amendments to Option B

Confirmed 2026-07-14 by the owner:

1. Server is ground truth for received mail; the client mirrors it locally and reconciles.
2. Drafts are local-only files and never sync to the server.
   This is simpler than the decision doc's Option C (which synced drafts through `pending_ops` and needed a watched-drafts shim).
   Trade-off accepted: drafts do not converge across the two machines.
3. Received mail is read-only.
   Editing in `$EDITOR` is for drafts only.
   This settles the decision doc's Decision 3 as option (i), the biggest behavioural change and the one the owner explicitly accepted.
4. No Obsidian / live `.md` tree / `grep` affordance.
   Terminal `$EDITOR` access to a message is all that is required.
   This settles Decision 2 as "drop entirely"; full-text search is served by FTS5, not by `grep`.
5. Bodies and attachments live in a content-addressed blob store on disk, not inside SQLite.
6. A persistent authenticated IMAP connection is approved, replacing the "one session per operation" invariant (Stage 5).

What survives from the founding product, unchanged:

- Read mail in the terminal TUI.
- Compose and edit drafts in `$EDITOR` (Neovim / Helix), the original raison d'etre.
- Agents (Claude / Robin) create and modify drafts as plain files the owner reviews and sends.
- Send from the terminal after approval.

## Why files-as-truth is being retired (verified against the tree)

The cost, measured against the current code, not asserted:

- Cold start streams every byte of the tree.
  `build_message_id_index` (`src/tui/app/types.rs:468`) walks 1327 files / 308 MB on the homeserver (thousands more on the Mac) at startup; `load_emails` (`types.rs:88`) full-parses a mailbox with `gray_matter`; `count_all_emails` (`types.rs:1038`) walks every directory a second time.
- Quick sync is slow because it does the maximum work every time.
  Per mailbox it opens a fresh TCP + TLS + LOGIN (`open_imap_session`, `src/imap_client/mod.rs:175`), runs `UID SEARCH ALL`, then a full-window pass-1 header + FLAGS fetch that is deliberately never shrunk because flags have no delta channel (`src/imap_client/fetch.rs:167`, the #0004 constraint).
  Multiply by mailboxes across four accounts.
  Apple Mail is smooth because it holds a persistent connection, uses IDLE push and CONDSTORE/QRESYNC deltas ("what changed since cursor N", one round trip, empty when nothing changed), and reads a local index instead of parsing files.
- Reconciliation is heuristic and ambiguous.
  `needs_reconciliation` (`src/imap_client/sync.rs:40`) guesses move/delete from EXISTS/UIDNEXT deltas, and out-of-band file edits collide with server truth with no principled winner.

Under the new model none of these exist: the read path is a `SELECT`, sync is a cursor delta, and the only writers to state are the sync engine and the user's own actions, which removes the corruption surface the owner flagged.

## Target architecture

One engine, one store, one writer per account.
This is the Mailspring / JMAP shape (research doc `.agents/workflow/2026-07-12-db-first-redesign/.../research-local-first-email.md`, findings 1, 5, 8), adapted to this codebase.

### Storage layout (per account)

```
<account_dir>/
  store.sqlite3        WAL, single writer: envelopes, flags, threading, mailboxes,
                       sync_cursors, pending_ops, + FTS5 over subject/from/body-text
  store.sqlite3-wal
  store.sqlite3-shm
  blobs/
    ab/cd/abcd1234...  content-addressed raw RFC822 / decoded body / each attachment,
                       filename = SHA-256 of the bytes
  drafts/              plain .md drafts, local-only, the $EDITOR + agent surface
```

`store.sqlite3` sits beside the existing `mailbox-states.json` and `contacts-cache.json`, matching the current per-account cache layout.

Content-addressed blob store: each raw body / HTML / attachment is a file named by the hash of its own bytes, and the DB row keeps only a `blob_ref` hash.
This gives automatic dedup (a forwarded attachment is stored once), integrity checks (re-hash to verify), and immutability (content never changes under a key), and it keeps SQLite small and fast by never storing large blobs inline.
The existing stable-hardlink attachment mirror (#0006) maps directly onto this store.

### Retention tiers and disk budget (configurable)

Because the server is truth, local storage is a cache the client is free to shrink and re-fill on demand.
Anything evicted locally is always re-fetchable from the server, so eviction is lossless, not deletion.
This is what makes fine-grained retention control safe, and it is a first-class config surface, not an afterthought.

Three independent retention horizons, coarsest-to-finest, each a config field per account (and a global default):

1. Metadata horizon: how far back to keep envelope rows (from/to/cc/subject/date/flags/thread). Cheap; default is keep-all so the list and search always render for the full history.
2. Body horizon: how far back to keep full message bodies (the body blob). Older bodies are evicted; the envelope row stays with its `blob_ref` marked evicted, and the body is re-fetched from the server when the user opens the message.
3. Attachment horizon: how far back to keep attachment blobs. Independent of and typically shorter than the body horizon, since attachments dominate disk.

On top of the horizons, a global disk budget: a per-account (and total) `max_disk_bytes`.
When usage exceeds it, a pruning pass evicts blobs oldest-first until under a low-water mark.
Pruning runs in cycles with hysteresis (a high-water trigger and a lower stop target), not on every message save, so a busy sync does not thrash the evictor.

Contradiction and precedence: the disk budget is a hard cap that overrides the horizons.
The horizons express intent ("I want bodies for a year"); the budget enforces reality ("but never past 5 GB").
Within a pruning pass the order of application is the precedence, as the owner proposed: evict attachment blobs first, then body blobs, then (only if still over budget) trim metadata beyond the metadata horizon.
Envelope rows are evicted last and rarely, because they are what lets an evicted message still appear in the list and be re-fetched.

Implementation notes:

- Eviction sets the row's blob state to evicted and unlinks the blob file (respecting dedup: only unlink when refcount hits zero); it never touches the server.
- Re-fetch on open is a normal engine pull for one message by UID; the body arrives, the blob is rewritten, the row flips back to present.
- The blob store needs a refcount or a periodic mark-and-sweep GC so a shared attachment is not unlinked while another message still references it.
- Config lives in the existing config file; defaults must be sane (keep-all metadata, generous body/attachment horizons, a conservative disk cap) so a user who sets nothing still gets correct behaviour.

### SQLite schema (v1 sketch, drop-and-rebuild on version mismatch, never a migrator)

```
meta(key PRIMARY KEY, value)                         -- schema_version, app_version
mailboxes(account, name PRIMARY KEY, uidvalidity, uidnext, exists_count, unread_count)
messages(
  message_id PRIMARY KEY, account, mailbox, uid,
  from_, to_, cc, subject, date_sort, date_display,
  flags, in_reply_to, references_, thread_id,
  snippet, has_attachments, body_blob, raw_blob, size, mtime
)
sync_cursors(mailbox PRIMARY KEY, uidvalidity, highest_modseq, deltalink)
pending_ops(id PRIMARY KEY, kind, target_message_id, payload, state, attempts, last_error, created)
messages_fts USING fts5(subject, from_, body_text, content='messages')
```

`rusqlite = { features = ["bundled"] }` is a new static dependency (bundled SQLite + FTS5, no system libsqlite), validated by meli in the research.

### The engine

- One long-lived task per account owns the store and the sync loop.
  The TUI reads the store and enqueues intents; it never touches IMAP or the schema directly (preserves the TUI-never-implements-email-logic invariant).
- `pending_ops` is a durable two-phase queue.
  Every mutation (archive, delete, move, mark-read, send) applies to the local store immediately, enqueues a remote op, and the engine drains it with retry and backoff, marking done or failed.
  This is the optimistic-mutation model the TUI already has via `Action` / `BgResult`, made durable instead of fire-and-forget.
  Retried APPEND-to-Sent lives here, which is the sent-durability fix.
- Per-folder sync cursors drive incremental pull: QRESYNC where advertised, else CONDSTORE flag-delta with a persisted HIGHESTMODSEQ, else today's EXISTS/UIDNEXT heuristic; Graph accounts use `/messages/delta` with a persisted `deltaLink`.
- The engine emits `BgResult` variants over the existing `mpsc` channel; `App::update` stays a pure state machine and only the source of `BgResult` changes.
  The existing TEA architecture is the asset that makes this a clean fit.

### Concurrency: running the TUI and the CLI at the same time

The single-writer discipline is per-account, and it must hold even when two processes are live (a TUI in one terminal, `mp` in another).
SQLite WAL already allows many concurrent readers plus one writer, with writes serialized by the database lock and `busy_timeout` smoothing brief contention.
That covers the store safely, but two processes each running their own sync engine (each holding IMAP connections and each writing) would double the server load and race on `pending_ops`.
The rule: at most one engine per account across all processes.

The mechanism is an advisory lock per account (a lockfile beside `store.sqlite3`, or SQLite's own locking):

- The first process to open an account acquires the engine lock and runs the sync engine (IDLE, pulls, draining `pending_ops`).
- A second process (the CLI while the TUI is open) opens the store read-only for queries and, for mutations, writes a `pending_ops` row and lets the lock-holding engine drain it. It does not open its own IMAP connection.
- If the lock-holder exits, the next process to need the engine acquires the lock and takes over.

Consequence: `mp search`, `mp` read commands, and even enqueuing a send all work fine while the TUI is open; they read the shared store and hand mutations to the one running engine.
No clashes, and no duplicate server sessions.
This is cleaner than today, where each `mp` invocation opens its own IMAP session independently.
A later refinement (out of scope for the first cut) is a proper background daemon that owns the engine and to which both TUI and CLI are thin clients; the advisory-lock model is the lightweight version of the same idea and is enough to start.

### What survives in the code (~40% intact)

The RFC822 parser and MIME extraction (`src/parse.rs`), `markdown_to_html` and per-recipient SMTP send (`src/send.rs`), OAuth2 / XOAUTH2 and the encrypted secrets store, the Graph REST client (`src/graph.rs`), config loading, contacts mining (`src/contacts/`), IMAP session open and capability handling, `TimingSpan`, and the entire TEA / `Action` / `BgResult` scaffolding.
What disappears (~25%) is the files-as-truth machinery: the two full-file scanners (`parse.rs:756`, `:989`), `deduplicate_mailbox`, `count_all_emails`, the `MessageIdIndex` HashMap, and the reconciliation heuristics.

## Staged transition, with a stop-gate after Stage 2

No big-bang and no long dual-write hell.
The lever is the no-migrations-before-v1.0 rule: the cutover in Stage 4 is not a migration, it is wipe-the-local-tree-and-resync-from-the-server, with a one-time import of drafts.

| Stage | Ticket | Ships | De-risks | Effort |
|---|---|---|---|---|
| 1 Store + engine skeleton, dual-write | [#0037](../tickets/0037-sqlite-store-engine-skeleton.md) | `src/store/` (schema, WAL, blob cache) + `src/engine/` (pull-loop skeleton). Fetch/save wkites the DB and blobs alongside files; reads still use files. | Schema, WAL single-writer discipline, blob store, all proven with zero user-visible change and a file fallback. | L |
| 2 Read path flips to DB | [#0038](../tickets/0038-read-path-to-db.md) | `load_emails`, list render, counts, search read the store; cold start stops walking files. Files still written for safety. | List-render perf and envelope correctness. This banks the owner's headline win. STOP-GATE. | L |
| 3 Mutations to `pending_ops` | [#0039](../tickets/0039-pending-ops-queue.md) | Archive/delete/move/mark-read/send become durable queue rows; engine drains with retry/backoff. | The op-queue, the genuinely new core infra and its new bug class. Lands behind the still-present file layer so a queue bug cannot lose mail. | L-XL |
| 4 Drop file layer, cutover | [#0040](../tickets/0040-drop-file-layer-cutover.md) | Delete scanners/reconciler/`MessageIdIndex`; received mail read-only; drafts local-only in `drafts/`; cutover = wipe local + resync + one-time draft import. | The paradigm flip, done as a clean resync, not a migrator. | M-L |
| 5 Persistent conn + protocol + FTS | [#0041](../tickets/0041-persistent-conn-condstore.md), [#0042](../tickets/0042-graph-delta-sync.md), [#0043](../tickets/0043-fts5-search.md) | Persistent authenticated connection, CONDSTORE/QRESYNC flag-delta, Graph `/messages/delta`, FTS5 search. | Sync smoothness and full-text search. | M-L |

The stop-gate is real: after Stage 2 the owner has the perf win (cold start and quick-sync render both stop touching thousands of files) and can pause before funding Stages 3-4 if the convergence/durability wins are not worth it.
That optionality is the point of the ordering.

### A complete-nuke alternative to dual-write

The staging above dual-writes files through Stage 2 as a safety net.
The owner has accepted a harder cutover instead: a clean break, as long as a working copy of the current version stays runnable.
That is straightforward because the current version is a git commit and a self-contained binary.
Two ways to preserve it:

1. Keep the old binary: build the current `main` (HEAD `73b843d`) once and copy the binary aside under a distinct name, e.g. `cargo install --path . --root ~/.local/mp-stable` or `cp "$(which mp)" ~/.local/bin/mp-legacy`. It keeps reading the existing `.md` tree and works forever against its own data; the redesign is developed as the new `mp`.
2. Do the redesign on the home server against a separate maildir/account data dir, and keep the Mac on the current working `mp` until the new version is proven, then switch.

The one caution: the old (files-as-truth) and new (store + blobs) versions must not point at the same account data directory at the same time, or the new version's wipe-and-resync cutover will delete the `.md` files the old one treats as truth.
Use separate data dirs, or do not run both against one account concurrently.

If the complete nuke is chosen, the dual-write in Stages 1-2 can be dropped (the new version writes only the store + blobs from the start), which simplifies those stages; the staged read/mutation ordering and the Stage 2 stop-gate still apply.

## Decision points already settled (do not re-ask)

- Storage backend: SQLite, not JSON, not NoSQL. Straight to SQLite because the envelope store (Stage 2) and FTS5 (Stage 5) need it and a JSON mirror becomes a full-rewrite-per-save liability.
- Bodies: content-addressed blob files, not inline in SQLite.
- Received mail: read-only. Drafts only in `$EDITOR`.
- Drafts: local-only, never synced.
- Obsidian / `grep`: dropped; FTS5 replaces search. Note the in-app search today is server-side IMAP SEARCH (`f` in the TUI, `mp search` in the CLI), not a local `grep`; FTS5 is a new local capability that also makes search work offline and without a round trip. There is no existing local full-text index to reuse.
- Persistent connection: approved; the "one session per operation" invariant in `docs/architecture.md:23` gets rewritten in Stage 5.

## Resolved unknowns (2026-07-14)

Backing research: the sync mechanics explainer [imap-sync-internals-explainer](../../.agents/research/2026-07-14-imap-sync-internals-explainer.md) and the [server-capability-matrix](../../.agents/research/2026-07-14-server-capability-matrix.md).

- async-imap 0.11.2 surface (was unverified): typed `select_condstore()` exists and the parser (imap-proto 0.16.7) already decodes `HIGHESTMODSEQ`, per-message `MODSEQ`, and `VANISHED (EARLIER)` into typed responses. `run_command` / `run_command_and_check_ok` / `read_response` are public on `Session`. So CONDSTORE is largely typed (CHANGEDSINCE goes in the fetch modifier string), and QRESYNC/ENABLE are feasible via the raw escape hatch with parser support already present. No blocker; a small spike still confirms the CHANGEDSINCE fetch-string form.
- Server capabilities (was unverified): Proton Bridge (Gluon) does NOT support CONDSTORE or QRESYNC (only IDLE + UIDPLUS + MOVE), so the owner's daily Proton account stays on the EXISTS/UIDNEXT heuristic + IDLE tier and the #0004 full-window flag fetch remains correct there. Gmail supports CONDSTORE + UIDPLUS + IDLE but not QRESYNC. Dovecot (likely tum) supports the full QRESYNC ladder. Outlook syncs via Graph delta, not IMAP. Gate each mechanism on live CAPABILITY.
- Test targets (was: no harness): validate live against the real accounts rather than blocking on a mock. Robin's Gmail exercises CONDSTORE + UIDPLUS; tum/Dovecot exercises QRESYNC; Proton Bridge exercises the heuristic + IDLE + UIDPLUS path. A mock harness can come later; live-account validation unblocks Stages 3 and 5 now.

## Still open, to resolve during implementation

- A large share of the 540 tests are file-layer (`tempdir()` scan/reconcile/dedup).
  They get rewritten against the store or deleted; budget this as a first-order cost in Stage 2 and Stage 4, not a rounding error.
- Confirm tum is Dovecot with a live CAPABILITY probe before relying on its QRESYNC (the probe recipe is in the capability matrix doc).
- Graph deltaLink expiry handling: treat an invalidated deltaLink like a UIDVALIDITY reset (full resync of that folder).

## Relationship to #0033 (MailView carve-out)

#0033's `MailView` state carve-out is blocked on this plan because it reshapes the same `App` mail-data fields (`emails`, `mailboxes`, `mailbox_counts`, `MessageIdIndex`, `load_emails` / `count_all_emails`) that Stage 2 rewrites.
Settle the store shape through Stage 2, then carve `MailView` once against the final shape.
Do not carve before Stage 2 lands.

## Verified code anchors the implementation will touch

- Read path: `src/tui/app/types.rs:88` (`load_emails`), `:468` (`build_message_id_index`), `:1038` (`count_all_emails`).
- Scanners to delete: `src/parse.rs:756` (`scan_mailbox_message_ids`), `:989` (`deduplicate_mailbox`).
- Reconciliation to replace: `src/imap_client/sync.rs:40` (`needs_reconciliation`), `:30` (`MailboxState`), `:86` (`mailbox-states.json` cache, the sibling-cache precedent).
- Fetch to reshape into the engine: `src/imap_client/fetch.rs:167` (`fetch_new_emails_on_session`), pass-1 headers `:213`.
- Session open to make persistent: `src/imap_client/mod.rs:175` (`open_imap_session`).
- Invariant to rewrite in Stage 5: `docs/architecture.md:23` ("one session per operation").
- TEA seam that stays: `Action` / `BgResult` in `src/tui/`, `App::update`.
