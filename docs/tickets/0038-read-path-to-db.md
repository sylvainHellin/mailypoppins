---
id: 0038
title: Read path reads the SQLite store (list, counts, search); cold start stops walking files
type: perf
priority: next
status: open
created: 2026-07-14
---

Stage 2 of the data-access-layer redesign. Plan: [data-access-layer](../plans/data-access-layer.md).
This is the STOP-GATE stage: it banks the headline perf win (cold start and list render stop touching thousands of files) and the owner can pause before Stages 3-4 if the convergence/durability wins are not worth funding.

Depends on [#0037](0037-sqlite-store-engine-skeleton.md).

## Scope

1. `load_emails` (`src/tui/app/types.rs:88`) builds the `EmailEntry` list from `messages` rows for the mailbox, sorted by `date_sort` in SQL, instead of walking + `gray_matter`-parsing files.
2. Per-mailbox counts become `SELECT COUNT(*) ... GROUP BY mailbox`, removing `count_all_emails` (`types.rs:1038`) and its second directory walk.
3. `build_message_id_index` (`types.rs:468`) startup walk is replaced by a store query; cold start no longer streams the tree.
4. Lazy body: `EmailEntry.body` loads from the blob store on selection/preview, not eagerly. Audit every `EmailEntry.body` consumer first (`src/tui/ui/preview.rs`, the body-search path). Respect the `Arc`-shared `email_cache` + generation guard; do not force a full clone per preview.
5. Files still written (Stage-1 dual-write stays) as the safety fallback; a store miss falls back to the file walk.

## Acceptance criteria

- List order and counts identical to today's file-based `load_emails` across the fixture set.
- Cold start `[TIMING]` shows no full-tree walk/read; `App::new` no longer reads thousands of files.
- Lazy body loads correct content on preview; body search still finds matches.
- Full suite green (file-layer tests rewritten against the store where needed); `cargo install` clean.

## Unblocks

- The stop-gate decision on Stages 3-5.
- [#0033](0033-view-switcher-contacts-view.md) `MailView` carve-out (store shape now settled).
