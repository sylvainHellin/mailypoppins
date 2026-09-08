# Design: preview latency on list navigation

> Status: implemented (2026-09-08) as [#0108](../tickets/0108-coalesce-key-events.md), [#0109](../tickets/0109-retire-inline-image-rendering.md), [#0110](../tickets/0110-retire-auto-mark-read.md) and [#0111](../tickets/0111-retire-rich-html-preview.md); the measurement in #0108 and the contingency decision are still open. Line numbers below predate the removals. Implementer-facing.
> Owner decisions taken in session 2026-09-09. Four of them are deliberate reversals of shipped tickets: [#0087](../tickets/0087-auto-mark-read-on-open.md), [#0010](../tickets/0010-inline-image-rendering.md), [#0091](../tickets/0091-html-to-text-rendering.md), and the render-time half of [#0093](../tickets/0093-memoise-preview-body-wrap.md).
> Reviewed once (2026-09-09) against an earlier draft that led with a cache; the cache is demoted here to a contingency behind a measurement.
> Next free ticket ID at time of writing: #0108.

## The problem

Navigating the message list with `j` / `k` has a visible delay on every keypress.
Holding the key makes it worse rather than smoother.

Every cursor move invalidates four memos and does their work synchronously, on the UI thread, inside `terminal.draw`.

`src/tui/ui/mod.rs:79-88` calls `refresh_preview_body`, `refresh_preview_html`, `refresh_preview_invite` and `refresh_preview_images` at the top of the render pass.
Each is a **one-slot** memo (`PreviewBody`, `PreviewHtml`, `PreviewInvite`, `PreviewImages`, `src/tui/app/types.rs:535` and siblings) holding a single key and a single value.
Moving down then back up evicts and reloads, so the memos only ever help a repaint on an unchanged selection, never navigation.

Two of the four open a store unconditionally: `refresh_preview_body` via `load_message_body` (`src/tui/app/mod.rs:1499`) and `refresh_preview_html` via `load_message_html` (`src/tui/app/mod.rs:1362`).
The other two are gated, on `is_invite` (`src/tui/app/mod.rs:1436`) and on `has_attachments` (`src/tui/app/mod.rs:1386`).
So an ordinary row costs two `open_store` calls per keypress, three with attachments, four for an invite with attachments.
Each is a fresh `rusqlite::Connection` plus its pragmas (`src/store/mod.rs:210`), because `Connection` is not `Sync` and cannot be parked on `AccountState`.

`load_html` (`src/store/read.rs:602`) prefers the `html` blob and falls back to reading the entire raw RFC822 blob and walking its MIME tree when there is none, which is the common case on the IMAP path.

`refresh_preview_images` (`src/tui/app/mod.rs:1381`) fires on any row with attachments: another raw-blob read, another full MIME walk, and a base64 decode per `cid:`-referenced image.
It is the single most expensive item in the render pass.

`PreviewLinesCache` (`src/tui/ui/preview.rs:140`) is a further one-slot memo keyed on `(body epoch, width, images key)`.
A cursor move bumps the epoch, so the whole body is re-rendered: either an html2text rich pass (`render_html_body`, `src/tui/ui/preview.rs:679`) or a Markdown parse and word-wrap (`wrap_and_style_body`).
The rich pass is span-dense, emitting one `Span` per annotation run.

`actions::auto_mark_open_read` (`src/tui/actions.rs:3334`, called from `src/tui/mod.rs:317`) then runs at the bottom of the same loop iteration.
For a newly selected unread row it opens the store again and commits a write transaction: the local `\Seen` update plus the owed server op.

Finally, crossterm key events are not coalesced.
`poll_event` (`src/tui/event.rs:11-24`) returns one event per loop iteration and each iteration pays the full cost above, so a held `j` accumulates a backlog behind the terminal's key repeat rate.
This is the most likely explanation for holding the key being worse than tapping it.

### The second problem

Auto-mark-read is not only a write on the UI thread.
Scrolling through the inbox marks every row the cursor passes as read and queues a `\Seen` op for each, which is wrong behaviour independently of its cost.

## Approach

Four independent removals first, each cheap and each a win on its own, with a measurement after them.
A cache and a background prefetch are held in reserve and built only if the measurement still demands them.

The earlier draft of this plan led with the cache.
Review showed the cache carries a correctness burden (see the Contingency section) that the removals do not, and that three of the four removals need neither the cache nor a schema change.
Sequencing them first is therefore both cheaper and more informative.

## Decision summary (settled, do not reopen)

- **D1, auto-mark-read is retired.** #0087 is reversed. A message is marked read on the explicit `Enter` / `e` open, and when focus moves to the body pane. The manual `u` toggle (`KeyCtx::Message`) is unchanged. The consequence is deliberate: the plain `j` / `k` workflow, with the preview following and focus never leaving the list, marks nothing read. `J` / `K` (`A::NextMessage`, `src/tui/app/keys.rs:511`) does not change focus and so does not mark read either.
- **D2, inline image rendering is retired.** #0010 goes in full, including `images::init()` and its terminal graphics protocol probe. Attachment *names* stay listed in the headers pane, and `b` / `tb` still renders the message with images in the browser. Only the in-pane pixel rendering and its `[image: ...]` placeholder lines go.
- **D3, the rich HTML render is retired.** #0091 is reversed. HTML-dominant mail falls back to `wrap_and_style_body` over the plain body that ingest already flattened (`html_to_plain`, `src/parse.rs:199`). The accepted cost: links, emphasis, tables and lists collapse into a wrapped block. The rationale is that either a message is lightly styled and the flatten is fine, or it is heavily styled and the reader opens it in the browser with `b` / `tb`, which is unchanged.
- **D4, key events are coalesced** before the loop draws.
- **D5, measure before building anything additive.** The cache and the prefetch are contingent on the baseline and the post-removal measurement.

## Scope

Four tickets, ordered. Each lands and is verified before the next starts.

Each ticket adds its own line to `BACKLOG.md` when it is cut and removes it when it ships.
None of the four removes a BACKLOG line for the ticket it reverses: #0010, #0087 and #0091 all shipped and were removed from the index already (`BACKLOG.md:11`, `docs/tickets/README.md:37`).

### Ticket A: baseline measurement and key-event coalescing

#### The measurement

The repo already carries the instrument: `TimingSpan` (`src/timing.rs:40`) with `mark()` (`src/timing.rs:78`), already used on hot paths such as `src/tui/helpers.rs:272`.
Wrap `terminal.draw` in one, and reuse the existing `store_open` span (`src/store/mod.rs:92`) to count store opens per keypress, since `open_store` (`src/store/mod.rs:210`) logs nothing on success.

The run conditions are pinned in the ticket so a second person reproduces the number: release build via `cargo install --path .` (never a debug build), the terminal emulator by name, the OS key-repeat delay and rate, the account and mailbox, the specific rows walked, and a warm page cache after one discarded pass.

Two figures: keypress to painted frame for `j` onto an inbox row with an HTML body and attachments, and wall-clock for a held `j` across twenty named rows.

#### The coalescing

Drain pending crossterm events before drawing, so a held `j` advances the cursor by the whole backlog and paints once.

`poll_event` returns `Option<Message>` (`src/tui/event.rs:11`) and `run_loop` applies one message then draws (`src/tui/mod.rs:158-170`).
Draining changes that contract, so the edit is in `run_loop` as much as in `event.rs`.

The drain must be **incremental**, not a bulk read: `poll(0)`, read one event, run `app.update`, then break if `app.pending_actions` now holds a terminal-suspending action.
A bulk drain cannot work, because whether a key yields such an action is a property of `pending_actions` *after* `app.update` and depends on `pending_prefix` (`src/tui/app/keys.rs:94`), focus, overlay and guards.
It is also unrecoverable: once an event leaves the kernel tty buffer it cannot be put back, and `suspend_terminal` (`src/tui/helpers.rs:192-202`) only leaves the alternate screen and disables raw mode while `edit_file` (`src/tui/helpers.rs:242-252`) spawns with inherited stdin.
Breaking after a bulk drain would therefore discard the keys rather than hand them to the editor.

No classifier exists for this today: `edit_file` has ten call sites in `src/tui/actions.rs` (472, 615, 1432, 1650, 1673, 2129, 2356, 2475, 2525, 2697) and no `Action` or `KeyAction` predicate marks them.
Adding `Action::suspends_terminal()` is explicit scope of this ticket.

`Resize` shares the crossterm queue and must not be reordered relative to keys.
`pending_prefix` is safe as long as order is preserved, so the `g` leader is not at risk.

#### Interim behaviour change

`auto_mark_open_read` runs once per loop iteration (`src/tui/mod.rs:317`), so coalescing twenty keypresses into one iteration marks only the final row read instead of all twenty.
That is an expected interim change between A and C, not a regression, and it is recorded here so a bisect does not read it as one.

Re-measure after landing.

### Ticket B: retire inline image rendering (#0010)

Remove `refresh_preview_images` (`src/tui/app/mod.rs:1381`), `load_inline_images`, `PreviewImages`, the `App::preview_images` field (`src/tui/app/mod.rs:97`), `App::prime_preview_images` (`src/tui/app/mod.rs:1575`), `ImagePlacement`, `placement_rect` (`src/tui/ui/preview.rs:239`), `append_image_block`, `render_inline_images`, the `use ratatui_image::StatefulImage` import (`src/tui/ui/preview.rs:7`), and the `images_key` component of the preview-lines key.

The module goes with them: `src/tui/images.rs`, its declaration (`src/tui/mod.rs:6`) and its `images::init()` call site (`src/tui/mod.rs:67`).

Tests and fixtures that go with it: `golden_mail_view_inline_image_placeholders` (`src/tui/ui/golden_frames.rs:609-626`) and its snapshot file, and `tiny_image`, `drawable` and the four image tests in `src/tui/ui/preview.rs:1145-1247`.

`ratatui-image` and `image` (`Cargo.toml:75-76`) are used only from those sites; both dependencies go.

`parse::embed_inline_images` (`src/parse.rs:1037`) and `parse::inline_images` (`src/parse.rs:1078`) **stay**: the `b` / `tb` browser path and the `.html` companion depend on them.
The names are one word apart from `load_inline_images`, which is the one being deleted.

Bookkeeping: `CHANGELOG.md:198`, `docs/architecture.md:250` (a module-table row for the deleted `src/tui/images.rs`) and `:205` (attributing `parse.rs::inline_images` to #0010), `docs/lessons-learned.md:867`, and a reversal note on `docs/tickets/0010-inline-image-rendering.md`.
`docs/lessons-learned.md:55` is *not* affected: it covers `is_attachment_part` and the paperclip icon, which this ticket does not touch.

No keymap, `mp --help`, help-overlay, dump-keys or website change: no image binding exists in `src/tui/app/keymap.rs` and `website/src/pages/` has no inline-image page.

Re-measure after landing.

### Ticket C: retire auto-mark-read (#0087)

Remove `auto_mark_open_read` (`src/tui/actions.rs:3334`), its call site (`src/tui/mod.rs:317`), `App::take_message_to_auto_mark_read` (`src/tui/app/mod.rs:1195`) and the `auto_read_opened` field.

Add the mark-read trigger to the `Enter` / `e` open path (`KeyAction::OpenEditor`, `src/tui/app/keymap.rs:611`) and to the two production sites that assign `Focus::Preview` (`src/tui/app/types.rs:1383`): the `A::FocusForward` arm (`src/tui/app/keys.rs:297`) and the `A::FocusBackward` arm (`src/tui/app/keys.rs:306`).
Both reuse `set_read_flag`, so the local write and the owed `\Seen` op commit together as they do today.

Hook the two key actions, not the field assignment.
There is a third write of `Focus::Preview` at `src/tui/app/mod.rs:496`, restoring the saved mail-view focus on a switch back from Contacts or Calendar, and it is **out of scope**: a view switch is not an open, and triggering there would re-mark a message the user had manually `u`-toggled to unread every time they left and returned to Mail.
`zoom_target` (`src/tui/app/mod.rs:1059-1067`) reads `self.focus` and assigns nothing, so it is not a trigger site either.

Tests: seven assertions across four tests at `src/tui/app/keys.rs:3374-3417`, all of which call the deleted `take_message_to_auto_mark_read` and so are deletions rather than updates, plus the three tests at `src/tui/actions.rs:3418-3457`.

Bookkeeping: `CHANGELOG.md:81` carries the shipped #0087 claim this ticket falsifies.
`docs/lessons-learned.md:977-984` documents the #0087 mechanism by name, and `:131` cites auto-mark-on-preview in the #0004 snapshot-clobber analysis; both go stale.
A reversal note goes on `docs/tickets/0087-auto-mark-read-on-open.md`.

### Ticket D: retire the rich HTML render (#0091)

Remove `refresh_preview_html`, `PreviewHtml`, `load_message_html` (`src/tui/app/mod.rs:1364`), `render_html_body` (`src/tui/ui/preview.rs:679`), `style_for_annotations` (`:722`) and the `RichAnnotation` import (`:1`).
The preview then always takes the `wrap_and_style_body` branch, which is the existing plain path.

`store::read::load_html` (`src/store/read.rs:602`) **stays**: `b` / `tb` reads it on demand.
Deleting the preview caller is what removes the raw-RFC822 fallback parse from the per-keypress path.

Bookkeeping: `CHANGELOG.md:23`, a reversal note on `docs/tickets/0091-html-to-text-rendering.md`, and the tests calling `render_html_body` or `style_for_annotations` across `src/tui/ui/preview.rs:1317-1403`.

The `css` feature of `html2text` (`Cargo.toml:47`) stays: `html_to_plain` uses `use_doc_css()` and ingest still runs it.

Re-measure after landing.
With B and D both in, a keypress costs one store open, one indexed SELECT, one blob read, and one Markdown wrap.
That is the point at which the contingency below is decided.

## Contingency: cache and prefetch

Build only if the post-D measurement is still short of a frame budget.
Recorded here so the design work is not lost, and because the review found two correctness holes that any future attempt must close.

### Hole 1: `messages.id` is reused

`MessageRef` wraps `messages.id` (`src/tui/app/types.rs:39`), which is `INTEGER PRIMARY KEY` without `AUTOINCREMENT` (`src/store/schema.rs:206`).
SQLite assigns `max(rowid) + 1`, so an id freed by deleting the highest row is handed to the next insert.

Rows are deleted on ordinary paths: `delete_row` (`src/store/write.rs:120`), `apply_delete` in the pending-op path (`src/pending_ops.rs:204`) and the sync prune pass (`src/sync/engine.rs:319-327`).
A move does **not** free an id: `move_row` (`src/store/write.rs:89`) keeps the row and rewrites `mailbox` and `uid` only.

A cache keyed on `(account_index, MessageRef)` would therefore serve a deleted message's body under a new message's identity for the life of the process.
`mailbox_load_generation`, the third element of `BodyKey` (`src/tui/app/types.rs:522`), is what prevents this today; dropping it from the key is a correctness regression, not merely a cache-lifetime change.

Fix: `INTEGER PRIMARY KEY AUTOINCREMENT`, a `SCHEMA_VERSION` bump from 8 to 9, and a store rebuild on first open after upgrade.
The store is a rebuildable server-as-truth mirror, so this is a bump and not a migration.

### Hole 2: re-ingest mutates in place

`ingest_in_tx` looks up an existing row by `(account, mailbox, uid)`, rebinds through the `message_id` index after a UIDVALIDITY reset, and then UPDATEs that same row id including `body_blob` and `raw_blob` (`src/ingest.rs:230-323`).
Same `MessageRef`, different content.

`AUTOINCREMENT` fixes reuse, not mutation.
A cache therefore needs explicit invalidation for ingest-touched rows, hooked on the sync-completion path (`refresh_after_server_sync`, `src/tui/bg.rs:47-56`), which already knows which account was written.

`message_id` is not an alternative key: it is the RFC 5322 header set by the sending client, and `src/store/schema.rs:95` states the index is deliberately non-unique because the same header appears in Inbox and Archive after a move, in Sent and Inbox when you mail yourself, and across broken senders.
A blob hash is content-exact but requires a store read to compute, which is the work the cache exists to avoid.

### Shape, if built

One `PreviewCache` keyed `(account_index, MessageRef)`, holding the plain body and the rendered `Vec<Line<'static>>` with the width it was rendered at, LRU-evicted against a byte budget.

Byte accounting must walk spans and count `capacity`, adding a per-span and per-line constant: each `Span<'static>` carries a `Cow<'static, str>` and a `Style`, so a budget computed as summed string lengths under-counts several-fold.
Set the budget from a measurement rather than picking a round number.

Caching rendered `Line`s is safe with respect to the theme: `theme::init` is a `OnceLock` set once from `App::new` (`src/tui/app/mod.rs:232`) and never re-set in session.

Drafts do not enter this cache; they keep a generation-keyed slot invalidated by the existing fingerprint poll (`DRAFTS_POLL_INTERVAL`, `src/tui/mod.rs:39`), since a draft file is mutable by `$EDITOR` and by agents.
`PreviewInvite` keeps its own small slot.
An asynchronously arriving body (`prime_preview_body`, and the on-open re-fetch of an evicted blob in [#0085](../tickets/0085-on-open-body-refetch.md)) must invalidate and refill its entry.

Prefetch, if it follows: one worker per account holding a single `Store`, in four stages, being the selected message, the rest of the current mailbox outward from the cursor, the sibling mailboxes of the account, then the other accounts.
Stage 4 chains onto the existing per-account startup threads in `src/tui/mod.rs` rather than adding a second set beside them.
Results reach the UI thread batched over the existing `BgResult` channel, which needs no change (`src/tui/mod.rs:239`).
A mailbox or account switch reprioritises rather than discards, since entries are keyed on the message.

## Out of scope

Parking a `Store` per account to avoid the per-call `open_store`, blocked on `Connection` not being `Sync`.

Storing a canonical unwrapped rich render at ingest and re-wrapping locally: it would make a rich render a one-time artifact, but requires reimplementing html2text's table and list layout, and D3 removes the rich render anyway.

## Acceptance criteria

Ticket A: the baseline is recorded in the ticket under the pinned run conditions; a held `j` across twenty rows paints once per drained batch rather than once per key; a key typed while an action suspends the terminal into `$EDITOR` still reaches the editor, not the app.

Ticket B: no *image-attributable* store read, MIME walk or base64 decode occurs for a row with attachments; `b` / `tb` still renders images in the browser; attachment names still appear in the headers pane; `cargo test` passes with the image tests and their snapshot deleted.
The absolute form of that criterion belongs to D: `refresh_preview_html` still reads a store and walks the raw MIME tree between B and D.

Ticket C: scrolling through an unread inbox leaves every row unread and queues no `\Seen` ops; `Enter` / `e` and a focus move into the body pane each mark the message read exactly once.

Ticket D: an HTML-dominant message renders through `wrap_and_style_body`; a keypress performs one store open, one SELECT and one blob read; `b` / `tb` is unchanged.

Overall: the post-D measurement is recorded, and the contingency is either opened as a ticket with that number as its justification, or closed as unnecessary.

## Risks

The measurement is the load-bearing step, and it is manual.
A held-`j` wall-clock on a real account is the honest signal; a synthetic benchmark over a fixture store is not.

D3 is the one reversal a user could object to, and the objection is #0091's own original complaint.
It is reversible: `load_html` stays in the store layer, so restoring the rich render is re-adding a caller.

If the contingency is built, a prefetch worker that opens a store file before the UI thread does owns the first `PRAGMA integrity_check` for that path, and on failure the drop-and-rebuild (`remove_store_files`, `src/store/mod.rs:356-367`) while other connections may be live.

WAL is not a risk: `apply_pragmas` (`src/store/mod.rs:336-352`) sets WAL, a busy timeout and `synchronous = NORMAL`, so a reader never blocks the sync writer.
The only residual concern would be a long-lived read transaction pinning WAL frames, which autocommit reads do not open, so a prefetch pass must simply not wrap itself in a transaction.
