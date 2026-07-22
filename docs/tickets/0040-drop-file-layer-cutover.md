---
id: 0040
title: Remove files-as-truth; received read-only; drafts local-only; wipe-and-resync cutover
type: refactor
priority: later
status: open
created: 2026-07-14
---

Stage 4 of the data-access-layer redesign. Plan: [data-access-layer](../plans/data-access-layer.md).

The paradigm flip, done as a clean resync rather than a migrator (honours the no-migrations-before-v1.0 rule).
Depends on [#0038](0038-read-path-to-db.md) and [#0039](0039-pending-ops-queue.md).

## Scope

1. Delete the files-as-truth machinery: the two scanners (`src/parse.rs:756`, `:989`), `deduplicate_mailbox`, `count_all_emails`, the `MessageIdIndex` HashMap, and the `needs_reconciliation` heuristic (`src/imap_client/sync.rs:40`).
2. Received mail becomes read-only: remove the edit-received-in-`$EDITOR` affordance; message bodies are checked out to an ephemeral temp file only for viewing.
3. Drafts are local-only in `<account_dir>/drafts/`: the `$EDITOR` + agent-writable draft surface, never synced to the server. The agent-writes-a-file contract is preserved here.
4. Cutover procedure: wipe the local `.md` tree, resync everything from the server into the store + blobs, one-time import of existing drafts (and any local-only sent records) into the new layout.

## Acceptance criteria

- No file walk or reconciliation heuristic remains in the codebase.
- Received mail is not editable; drafts are, and never leave the machine.
- A clean-machine cutover (wipe + resync + draft import) reproduces the account state from the server with no data loss beyond the intended draft-non-convergence.
- File-layer tests removed or rewritten against the store.

## Unblocks

- [#0041](0041-persistent-conn-condstore.md), [#0042](0042-graph-delta-sync.md), [#0043](0043-fts5-search.md) (Stage 5 protocol + search on a pure store base).
