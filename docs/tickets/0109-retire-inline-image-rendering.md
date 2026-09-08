---
id: 0109
title: Retire inline image rendering in the preview pane
type: perf
priority: now
status: done
created: 2026-09-08
---

Ticket B of [docs/plans/preview-latency.md](../plans/preview-latency.md), and a deliberate reversal of [#0010](0010-inline-image-rendering.md).

## Problem

Every cursor move in the message list refreshes four one-slot memos inside `terminal.draw`, and the inline-image one is the most expensive of them.

`refresh_preview_images` fired on any row with attachments: one store open, one `load_html` (which parses the whole raw RFC822 when there is no `html` blob, the common case on the IMAP path), one more raw-blob read, a full MIME walk, and a base64 decode plus an image decode per `cid:`-referenced part. All of it on the UI thread, all of it thrown away on the next `j`.

What it bought is narrow. The pixels only appear on a terminal that speaks kitty, iTerm2 or sixel; everywhere else, and in every test process, the whole pipeline runs to produce a `[image: name]` line. The images the user actually wants to see at full size are one keypress away in the browser (`b` / `tb`), which renders the message with its images embedded and is unaffected by this ticket.

## Approach

Delete the feature, the module and its two dependencies. Nothing is replaced.

- `refresh_preview_images`, `load_inline_images`, `App::preview_images` and `App::prime_preview_images` go from `src/tui/app/mod.rs`, and the call site from `src/tui/ui/mod.rs`.
- `ImagePlacement`, `placement_rect`, `append_image_block`, `render_inline_images` and the `ratatui_image::StatefulImage` import go from `src/tui/ui/preview.rs`. The `[image: ...]` placeholder lines go with `append_image_block`.
- `PreviewLinesCache` loses the `images_key` component of its key and the `placements` it carried. The key is now `(body epoch, width)`: the third element only ever discriminated two bodies by their image sets, and there are no image sets.
- `src/tui/images.rs` goes whole, with its `mod` declaration and the `images::init()` capability probe in `tui::run`. The TUI no longer queries the terminal for a graphics protocol at startup.
- `ratatui-image` and `image` leave `Cargo.toml`; they had no other call sites.

What stays: `parse::inline_images` and `parse::embed_inline_images`, which the browser path and the `.html` companion depend on and which are one word away from the deleted `load_inline_images`; the attachment affordance (`is_attachment_part` and the `Attach:` paperclip line of #0096 are untouched; the pane shows only that marker, and the names live in the `to` / `ts` overlay); and `store::read::load_html`, still read by `b` / `tb` and, until Ticket D, by `refresh_preview_html`.

## Acceptance

- No image-attributable store read, MIME walk or base64 decode occurs for a row with attachments. The absolute form of that ("one store open per keypress") belongs to Ticket D: `refresh_preview_html` still opens a store and still walks the raw MIME tree between B and D.
- `b` / `tb` still renders inline images in the browser.
- The headers pane still shows its `Attach:` paperclip line, and `to` / `ts` still list the attachments by name.
- `cargo test` passes with the four image tests, their `tiny_image` / `drawable` fixtures, the `golden_mail_view_inline_image_placeholders` golden and its snapshot file deleted.
- No `ratatui_image::` or `image::` reference survives outside `Cargo.lock`.

## Done (2026-09-08)

16 packages left `Cargo.lock` with the two direct dependencies.

The plan placed `PreviewImages` in `src/tui/app/types.rs`; it lived in `src/tui/images.rs` and went with the module. Everything else matched.

No keymap, `mp --help`, help-overlay or website change was needed: no image binding existed in `src/tui/app/keymap.rs` and no page under `website/src/pages/` mentions inline images.

The measurement fields of [#0108](0108-coalesce-key-events.md) are re-taken after this lands.

## Links

- Plan: [docs/plans/preview-latency.md](../plans/preview-latency.md), Ticket B and Decision D2.
- Reverses: [#0010](0010-inline-image-rendering.md).
- Sibling tickets from the same plan: A ([#0108](0108-coalesce-key-events.md), shipped), C ([#0110](0110-retire-auto-mark-read.md), retiring auto-mark-read, shipped), D (retire the rich HTML render, reverses [#0091](0091-html-to-text-rendering.md)).
