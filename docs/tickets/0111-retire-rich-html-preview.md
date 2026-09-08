---
id: 0111
title: Retire the rich HTML preview render
type: perf
priority: now
status: done
created: 2026-09-08
---

Ticket D of [docs/plans/preview-latency.md](../plans/preview-latency.md), and a deliberate reversal of [#0091](0091-html-to-text-rendering.md).

## Problem

`refresh_preview_html` ran at the top of every render pass, beside the body and invite memos. For any received row it called `load_message_html`, which opens a store, then `store::read::load_html`, which prefers the `html` blob and falls back to reading the whole raw RFC822 blob and walking its MIME tree when there is none. On the IMAP path there is no `html` blob, so the fallback is the common case: a full raw read and MIME parse on the UI thread, inside `terminal.draw`, on every cursor move.

The memo did not help. `PreviewHtml` is a one-slot memo keyed on the body key, so moving down and back up evicts and reloads; it only ever saved a repaint on an unchanged selection, never navigation.

The render that work fed was the second half of the cost. `render_html_body` ran an html2text rich pass and emitted one `Span` per annotation run, which is span-dense compared with the Markdown wrap, and `PreviewLinesCache` is keyed on the body epoch, which a cursor move bumps.

It was the last item of the per-keypress preview cost that Tickets B and C had not removed: #0109 took the image read and #0110 took the write, and this store open and MIME walk stayed.

## Approach

Delete the branch, not just its cost. The preview now always takes `wrap_and_style_body` over the stored plain body, which ingest already flattened out of the HTML through `parse::html_to_plain` (`config::plain()`, `use_doc_css()`).

- `refresh_preview_html` and its call in `src/tui/ui/mod.rs`, the `PreviewHtml` memo, the `App::preview_html` field and `load_message_html` go. Nothing else read the field: the body-pane render was its only consumer, and `PreviewLinesCache`, the `b` / `tb` action, the body search and `zoom` all read `preview_body`.
- `render_html_body`, `style_for_annotations`, the `RichAnnotation` import and the html2text rich config go with them. `src/tui/ui/preview.rs` no longer mentions html2text at all.

The accepted cost is #0091's own original complaint, taken back deliberately (Decision D3): links, emphasis, tables and lists collapse into a wrapped block. The rationale is that either a message is lightly styled and the flatten is fine, or it is heavily styled and the reader opens it in the browser with `b` / `tb`, which is unchanged.

What stays: `store::read::load_html`, still read by `b` / `tb` (`src/tui/actions.rs`) and by `draft.rs`; `parse::html_to_plain`, which is what makes the plain body readable in the first place; and the `css` feature of `html2text` in `Cargo.toml`, which `html_to_plain` needs for `use_doc_css()`. Deleting the preview caller is what removes the raw-RFC822 fallback parse from the per-keypress path, not deleting the reader.

Reversible by re-adding a caller, which is the reason `load_html` was left in the store layer.

## Acceptance

- An HTML-dominant message renders through `wrap_and_style_body`, the same path plain mail and drafts already took.
- A keypress costs one store open, one indexed SELECT, one blob read and one Markdown wrap. That is the absolute form of #0109's criterion, which had to wait for this ticket because `refresh_preview_html` still read a store and walked the raw MIME tree between B and D.
- `b` / `tb` is unchanged and still renders the sender's HTML with images in the browser.
- `cargo test` passes with the seven `render_html_body` / `style_for_annotations` tests deleted.

## Done (2026-09-08)

Everything matched the plan. The plan's line numbers had drifted by roughly 130 lines after #0109 and #0110, but every named item existed and nothing else consumed `preview_html`.

No golden frame rendered the rich path: the golden frames never populate `App::preview_html`, so they already exercised the plain branch and no snapshot changed. `cargo insta test --unreferenced=reject` reports no unreferenced snapshots.

The website FAQ ("Does it support HTML emails?") was the one reader-facing claim that HTML-dominant mail renders with headings, lists, links, tables and emphasis in the pane; it now says the pane shows the flattened text and points at `b` / `tb`. No keymap description, help-overlay line or `mp --help` string ever made that claim, so the CLI help snapshot is untouched.

With B and D both in, a keypress costs one store open, one indexed SELECT, one blob read and one Markdown wrap. That is the point the plan names for deciding the cache-and-prefetch contingency: #0108's measurement fields are re-taken under its pinned run conditions, and the contingency is then either opened as a ticket with that number as its justification or closed as unnecessary.

## Links

- Plan: [docs/plans/preview-latency.md](../plans/preview-latency.md), Ticket D and Decision D3.
- Reverses: [#0091](0091-html-to-text-rendering.md).
- Sibling tickets from the same plan: A ([#0108](0108-coalesce-key-events.md)), B ([#0109](0109-retire-inline-image-rendering.md)), C ([#0110](0110-retire-auto-mark-read.md)), all shipped.
