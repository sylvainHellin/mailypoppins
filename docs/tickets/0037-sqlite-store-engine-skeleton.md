---
id: 0037
title: SQLite store + blob cache + engine skeleton, dual-write alongside files
type: refactor
priority: next
status: open
created: 2026-07-14
---

Stage 1 of the data-access-layer redesign. Plan: [data-access-layer](../plans/data-access-layer.md).

Stand up the new storage substrate and engine skeleton with zero user-visible change.
Fetch and save write the SQLite store and the content-addressed blob store alongside the existing `.md` files; reads still use files.
This de-risks the schema, the WAL single-writer discipline, and the blob store behind a full file fallback.

## Scope

1. New `src/store/` module: per-account `store.sqlite3` (WAL, `busy_timeout=5000`, `synchronous=NORMAL`), schema v1 (`meta`, `mailboxes`, `messages`, `sync_cursors`, `pending_ops`, `messages_fts`), version-stamped with drop-and-rebuild on mismatch or `integrity_check` failure. `pub(crate)` with `#[cfg(test)]` tempdir tests, mirroring `sync.rs`'s cache section. Never a migrator.
2. Content-addressed blob store under `<account_dir>/blobs/ab/cd/<sha256>`: write-blob returns a hash, read-blob by hash, dedup by existence check.
3. New `src/engine/` skeleton: per-account owner of the store handle and a pull-loop stub (no protocol change yet). The store handle lives in the library, never in `tui/`.
4. Dual-write: `save_fetched_emails_*` and the send/append paths write the DB row + blobs in addition to the `.md` file. Reads unchanged.
5. `rusqlite = { features = ["bundled"] }` added to `Cargo.toml`.
6. Config surface for retention (metadata/body/attachment horizons) + a per-account and total `max_disk_bytes` disk budget, with sane defaults (keep-all metadata, generous body/attachment horizons, conservative cap). Blob store carries a refcount (or a mark-and-sweep GC) so dedup-shared blobs are only unlinked at refcount zero. Pruning/eviction logic itself can land incrementally, but the schema + config fields belong here. See the plan's "Retention tiers and disk budget" section.
7. Per-account engine advisory lock (lockfile beside `store.sqlite3`) so at most one sync engine runs per account across processes; other processes open the store read-only and enqueue mutations. See the plan's "Concurrency" section.

## Acceptance criteria

- After a fetch, the store holds a `messages` row and the body/attachments exist as blobs, byte-identical to the `.md`-derived content.
- Corrupt or version-mismatched DB is silently dropped and rebuilt; no user-visible failure.
- No change to `.md` file format or to any read path; full suite still green.
- `cargo install --path .` clean; `[TIMING]` spans added for store open/write.

## Unblocks

- [#0038](0038-read-path-to-db.md) (read path flips to the store).
