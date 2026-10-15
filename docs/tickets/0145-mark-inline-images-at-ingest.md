---
id: 0145
title: Mark inline (cid-referenced) images at ingest so attachment lists stop showing signature logos
type: bug
priority: next
status: open
created: 2026-10-15
---

## Problem

`message_blobs` stores no Content-ID and no inline flag.
So `mp show`, the TUI and the desktop app list Outlook signature logos (`image001.png`, `image002.png`) as attachments of a received mail.

Commit `b4209c44` fixed the forward path only.
`source_from_row` (`src/draft.rs`) now calls `materialise_forward_attachments` (`src/store/read.rs`), which re-parses the raw RFC822 at forward time (`parse::cid_referenced_image_contents`, `crates/mp-core/src/parse.rs`) and drops every attachment whose blob hash matches a cid-referenced image part.

Graph rows have no raw message, so their forwards still keep every attachment.
Graph's attachment API does return `isInline` and `contentId` (`src/graph.rs` ~738 and ~788), and ingest drops both.

## Proposed approach

Record at ingest, for IMAP and Graph alike, whether an attachment is inline and referenced by the HTML.
Two shapes to choose between: a `content_id` / `inline` column on `message_blobs` with a schema migration, or a filter at ingest that never stores the part as an attachment.
Then have `attachments_for` and the list renditions, `has_attachments`, and the forward path read that flag instead of re-parsing the raw message.
Decide how existing rows are backfilled: re-parse the raw lazily, or once in the migration.

Related follow-up: `mp_core::draft::source_from_fetched` (`crates/mp-core/src/draft.rs` ~416) still copies every fetched attachment; only tests call it today.

## Acceptance criteria

- A received Outlook mail with signature logos shows only its real attachments in `mp show`, the TUI and the desktop app.
- The forward behaviour of `b4209c44` is preserved, and now holds for Graph rows too.
- Tests cover IMAP ingest and Graph ingest.
