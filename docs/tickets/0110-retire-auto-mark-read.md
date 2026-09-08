---
id: 0110
title: Retire auto-mark-read in favour of an explicit open
type: refactor
priority: now
status: done
created: 2026-09-08
---

Ticket C of [docs/plans/preview-latency.md](../plans/preview-latency.md), and a deliberate reversal of [#0087](0087-auto-mark-read-on-open.md).

## Problem

`auto_mark_open_read` ran at the bottom of every `run_loop` iteration. For a newly selected unread row it opened the store and committed a write transaction: the local `\Seen` update plus the owed server op. That is a store open and a write on the UI thread, on the same keypress that already pays for four preview memos.

The cost is the smaller half of it. Scrolling an unread inbox with `j` / `k` marked every row the cursor passed as read and queued a `\Seen` op for each, which is wrong behaviour independently of what it costs: the user was triaging, not reading, and the state converged on the server before they had decided anything.

#0087's premise was that the preview always shows `selected_email()`, so "opening a message" is just the cursor landing on a new row. That premise is what makes the feature wrong: with a preview that follows the cursor, there is no cursor move that is not an open.

## Approach

Keep the write path, replace the trigger. The mark now rides on an act the user performs deliberately.

- `auto_mark_open_read`, its `run_loop` call site, `App::take_message_to_auto_mark_read` and the `App::auto_read_opened` tracker go. Nothing needs a once-per-open tracker any more: a trigger that fires on a keypress rather than on every loop iteration cannot fire twice for one open.
- `actions::mark_open_read` replaces it, reusing `set_read_flag` so the local write and the owed `\Seen` op commit together (#0039) and converge on the next sync (#0004). It is what `Action::MarkAsRead` runs, and it guards on the same things the manual `u` toggle does: no `MessageRef` (a Drafts row or any non-message entry) and an already-read row are both no-ops.
- Three triggers. `Action::EditCurrent` (`Enter` / `e`) calls `mark_open_read` directly on its received-row branch, so the mark happens on the real open rather than on a keypress that a guard swallows or that lands on a draft. The `A::FocusForward` and `A::FocusBackward` arms push `Action::MarkAsRead` when they land on `Focus::Preview` in the mail view; a key handler cannot open a store, so it queues the action and `actions.rs` performs the write, the same route `u` takes.
- `App::queue_mark_open_read` carries the keys-side guard, so an already-read row, a draft or a non-Mail cursor queues nothing at all rather than an action that would still cost a store open before deciding it had nothing to do.

Not triggers. The `Focus::Preview` restore on a view switch back from Contacts or Calendar (`App::load_from_mail_view`) is out of scope: a view switch is not an open, and marking there would re-mark a message the user had manually `u`-toggled to unread every time they left and returned to Mail. `zoom_target` reads `self.focus` and assigns nothing. `J` / `K` (`A::NextMessage`) does not change focus, so it marks nothing, which is the deliberate consequence of the whole ticket: the plain `j` / `k` workflow with the preview following marks nothing read.

## Acceptance

- Scrolling through an unread inbox leaves every row unread and queues no `\Seen` ops.
- `Enter` / `e` and a focus move into the body pane each mark the message read exactly once, through `set_read_flag`, with one owed server op.
- `u` still toggles either way, and a row toggled back to unread is not re-marked until the next explicit open.
- Drafts, non-message entries and already-read rows are no-ops on every trigger.
- `cargo test` passes with the four `take_message_to_auto_mark_read` tests and the three `auto_mark_open_read` tests deleted.

## Done (2026-09-08)

Everything matched the plan except the trigger site for the open path. The plan named `KeyAction::OpenEditor` in `src/tui/app/keymap.rs`; hooking there would mark a row the handler then declines (a draft, a parse-skipped row), so the mark went on the `Action::EditCurrent` handler's received-row branch instead, where `selected_email_ref()` has already resolved.

`Action::MarkAsRead` needed no new plumbing: the arm, its `set_read_flag` -> `queue_read_flag` -> `apply_set_read` route and its `suspends_terminal` classification all existed and had had no producer since #0087 moved the trigger into `run_loop`. This ticket gives it two.

The interim note of [#0108](0108-coalesce-key-events.md) (coalescing marks only the last row of a batch read) is spent: there is nothing left for the drain to coalesce a mark out of. It is dropped from the CHANGELOG entry and kept in #0108's ticket as history.

No `mp --help`, help-overlay or website change was needed: no keymap description and no page under `website/src/pages/` ever claimed a message is marked read automatically.

Follow-up, same day: `Action::MarkAsRead` carries the `MessageRef` the open resolved. It shipped without a payload and `mark_open_read` re-read `selected_email()` at drain time, so under the #0108 coalescing a `Tab` and a `J` in one batch marked the row the cursor ended on instead of the one that was opened. `queue_mark_open_read` and the `Action::EditCurrent` received-row branch now both hand it a ref, matching the `BatchToggleRead(msgs)` shape.

The measurement fields of #0108 are re-taken after this lands.

## Links

- Plan: [docs/plans/preview-latency.md](../plans/preview-latency.md), Ticket C and Decision D1.
- Reverses: [#0087](0087-auto-mark-read-on-open.md).
- Write path reused: [#0039](0039-pending-ops-queue.md), converging per [#0004](0004-fix-read-unread-sync.md).
- Sibling tickets from the same plan: A ([#0108](0108-coalesce-key-events.md), shipped), B ([#0109](0109-retire-inline-image-rendering.md), shipped), D (retire the rich HTML render, reverses [#0091](0091-html-to-text-rendering.md)).
