---
id: 0145
title: Mark inline (cid-referenced) images at ingest so attachment lists stop showing signature logos
type: bug
priority: next
status: done
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

## Resolution (2026-10-22, PERSO-165)

Root cause: ingest dropped the part's `Content-ID` (the IMAP parser and the Graph attachment list both carried it in `AttachmentData`) and stored no inline flag, so every reader had to re-parse the raw message to tell a logo from an attachment, and a Graph row has no raw message.
The same gap broke Graph inline images in the desktop reader (PERSO-165): `message.html` inlined `cid:` images only from the raw.

What shipped:

- `message_blobs` gains the additive columns `content_id` (normalised) and `is_inline`, added on open with no schema bump (`src/store/schema.rs`, decision log 2026-10-22).
- Inline means: the HTML body references the part's id by `cid:` URL and the part is an image by its bytes or extension (`parse::is_inline_image`); Exchange gives every attachment a content id, so the id alone says nothing, and a `cid:` link to a PDF stays an attachment.
- Ingest marks every attachment reference for both backends, and `has_attachments` counts real attachments only, so an inline-only message shows no paperclip (`src/ingest.rs`).
- `read::attachments_for` leaves inline references out, which covers `mp show`, the TUI, the desktop app, `mp save` and `mp open`; the forward reads the mark and re-parses the raw only for an unmarked row (`src/store/read.rs`).
- `message.html` and `message.materialise_html` inline `cid:` images from the stored blobs by `content_id` (`read::stored_inline_images`), and fall back on the raw re-parse only when the blobs give nothing; the desktop reader consumes that rendition unchanged.
- Backfill of rows an older build stored, bounded per sync pass: `ingest::classify_inline_from_raw` (200 rows a pass, local) for every row with a raw message; on Graph, `GraphClient::classify_stored_inline` (50 rows a mailbox a pass) asks the attachment list as names and content ids only and pairs it with the stored references by name in order.
- Inline-only Graph messages stored before 88342da1 are covered too: `ingest::mark_inline_only_rows` queues every Graph row without attachments whose HTML references `cid:` for the attachment repair, once per store, and the repair stores the images marked inline.

Live run on a copy of the EVOQS store (`examples/graph_inline_probe.rs --backfill`): 534 rows marked from 534 metadata requests, about 730 KB in all (1.3 KB a message), 49 s; 472 inline-only rows queued, of which 441 got their images (35 MB, 70 s) and 31 had nothing storable.
The WZT signature mail of 2026-07-31 (`image001.png` beside a PDF) lists the PDF only and its rendition carries the logo as a `data:` URI.

Left as is: `mp_core::draft::source_from_fetched` still copies every fetched attachment (only tests call it), and a Gmail-forwarded mail whose part ids no longer match its HTML keeps those images listed, since nothing ties them to the body.
