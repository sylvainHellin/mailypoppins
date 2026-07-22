---
id: 0043
title: FTS5 full-text search over the envelope + body store
type: feature
priority: later
status: open
created: 2026-07-14
---

Stage 5 (search) of the data-access-layer redesign. Plan: [data-access-layer](../plans/data-access-layer.md).

Full-text search becomes a `SELECT`, not a file stream.
This is the replacement for the dropped `grep`-the-tree affordance.
Depends on the store (Stage 2); best after the file layer is gone (Stage 4).

## Scope

1. FTS5 external-content virtual table `messages_fts` over subject / from / body-plaintext, fed at save/sync time from the store + blob bodies.
2. The `\` search path queries FTS5 instead of streaming files.
3. Keep the index fresh on sync writes; the store is the single freshness surface (no out-of-band file edits under the new model, so no reconcile-on-edit needed).

## Acceptance criteria

- Body/subject/from search returns correct hits via FTS5; ranked, fast on the full tree.
- Search latency `[TIMING]` well below today's file-streaming search.
- Index stays consistent with the store across sync and mutation.
