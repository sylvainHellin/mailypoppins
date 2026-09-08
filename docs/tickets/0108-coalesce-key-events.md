---
id: 0108
title: Baseline preview-latency measurement and key-event coalescing
type: perf
priority: now
status: done
created: 2026-09-08
---

Ticket A of [docs/plans/preview-latency.md](../plans/preview-latency.md).
It carries the baseline measurement the other three tickets (B, C, D) are judged against, and the one behaviour change of the four: crossterm key events are drained before the loop paints.

## Problem

Navigating the message list with `j` / `k` has a visible delay on every keypress, and holding the key makes it worse rather than smoother.

The per-keypress cost is diagnosed in the plan: four one-slot memos invalidated by every cursor move, two to four `open_store` calls, a raw RFC822 read and MIME walk for HTML and for inline images, a full re-render of the preview body, and an auto-mark-read write transaction at the bottom of the same iteration.

On top of that, `poll_event` (`src/tui/event.rs`) returns one event per loop iteration and `run_loop` paints once per iteration, so a held `j` builds a backlog behind the terminal's key repeat rate: the app paints N frames for N repeats, each frame paying the whole cost above, and the cursor lags the key.

Two things are missing before any of that can be judged:

- No number. Nothing in the loop is instrumented, so "it feels slow" is the only signal, and the plan's removals would land unmeasured.
- No classifier for terminal-suspending actions. `edit_file` has ten call sites in `src/tui/actions.rs` and no `Action` predicate marks them, so a drain cannot know when to stop and hand the remaining keystrokes to `$EDITOR`.

## Approach

### Instrumentation

`TimingSpan` (`src/timing.rs`) already exists and already logs with a `[TIMING]` prefix at `info`, which the file logger records (it is initialised at `LevelFilter::Debug`).

- Wrap `terminal.draw` in `run_loop` in a span named `tui_draw`. One paint is one `start` line plus one `done` line in the log.
- `store_open` (`src/store/mod.rs`, in `Store::open`) needed no change: contrary to the plan's note, the success path already calls `span.mark("validated")` and the span logs `done` on drop, so a successful open is already three `[TIMING] store_open` lines and is countable as-is.

### Coalescing

Drain the pending crossterm events before drawing, so a held `j` advances the cursor by the whole backlog and paints once.

The drain is incremental, not a bulk read: `poll(0)`, read one event, run `app.update`, then stop if `app.pending_actions` now holds an action that will suspend the terminal.
Whether a key yields such an action is a property of `pending_actions` after `app.update` and depends on `pending_prefix`, focus, overlay and guards, so it cannot be decided from the raw event.
It is also unrecoverable: an event that has left the kernel tty buffer cannot be put back, and `suspend_terminal` only leaves the alternate screen while `edit_file` spawns with inherited stdin, so a bulk drain would eat the keys the editor is owed.

The drain is bounded twice, by a batch cap and by a time budget, so a flooded queue cannot starve the paint.
Event order is preserved, which is what `pending_prefix` (the `g` and `c` leaders) and `Resize` both depend on.

`Action::suspends_terminal()` is explicit scope of this ticket: an exhaustive match, no wildcard arm, so a new action has to be classified rather than defaulting to "safe to batch".

### Interim behaviour change (spent since [#0110](0110-retire-auto-mark-read.md))

`auto_mark_open_read` ran once per loop iteration, so coalescing twenty keypresses into one iteration marked only the final row of the batch read instead of all twenty.
That was expected between A and C, not a regression, and it is recorded here so a bisect of that window does not read it as one.
Ticket C retired auto-mark-read outright, so from #0110 onward the drain has no mark to coalesce away.

## Measurement

Manual, and out of scope for the implementation: a held-`j` wall-clock on a real account is the honest signal, and a synthetic benchmark over a fixture store is not.
Sylvain fills the fields below, once before Ticket B (baseline, with this ticket's two commits installed) and again after each of B, C and D.

### Run conditions (pin these, or the number is not reproducible)

| Field | Value |
| --- | --- |
| Build | release, installed with `cargo install --path .` (never a debug build) |
| Commit | TBD (`git rev-parse --short HEAD`) |
| Terminal emulator | TBD (name and version) |
| OS key-repeat delay | TBD ms |
| OS key-repeat rate | TBD /s |
| Account | TBD |
| Mailbox | TBD |
| Rows walked | TBD (name the twenty subjects or the first and last, so the same rows are walked next time) |
| Page cache | warm: run one full pass and discard it, then measure the second |

Take the two commits separately to get a before and an after of the coalescing alone:
`git checkout <commit 1>; cargo install --path .` measures the uncoalesced loop with the instrument in place, then the same for commit 2.

### Figure 1: keypress to painted frame

One `j` onto an inbox row with an HTML body and attachments, from a settled UI (no sync running, no spinner).

```sh
rg '\[TIMING\] tui_draw' "$(ls -1 ~/.local/share/mailypoppins/logs/mailypoppins-*.log | tail -1)" | tail -20
```

Use the path the build actually writes to if the data dir is overridden; the file is `<data_dir>/logs/mailypoppins-YYYY-MM-DD.log`.
The `done: N ms` line of the frame that followed the keypress is the figure.

Store opens charged to that keypress, counted between the two `tui_draw` lines:

```sh
rg '\[TIMING\] store_open .* done' "$(ls -1 ~/.local/share/mailypoppins/logs/mailypoppins-*.log | tail -1)" | tail -20
```

- Figure 1, before: TBD ms per frame, TBD store opens per keypress.
- Figure 1, after: TBD ms per frame, TBD store opens per keypress.

### Figure 2: held `j` across twenty rows

Hold `j` until the cursor has walked the twenty named rows, then stop.
Wall-clock by hand (phone stopwatch is fine) and, independently, from the log:

```sh
rg '\[TIMING\] tui_draw (start|done)' "$(ls -1 ~/.local/share/mailypoppins/logs/mailypoppins-*.log | tail -1)" | tail -60
```

First `start` to last `done` of the burst is the elapsed time; the number of `done` lines in the burst is the frames painted.
Coalescing should cut the frame count well below twenty while leaving the twenty cursor moves intact.

- Figure 2, before: TBD s, TBD frames.
- Figure 2, after: TBD s, TBD frames.

## Acceptance

- The baseline is recorded above under the pinned run conditions.
- A held `j` across twenty rows paints once per drained batch rather than once per key, and still lands on the twentieth row.
- A key typed while an action suspends the terminal into `$EDITOR` reaches the editor, not the app.
- `Action::suspends_terminal()` is true for every action whose handler reaches `edit_file`.

## Done (2026-09-08)

- `9df192f` Instrument the draw pass and store opens (#0108).
- The commit carrying this line, `Coalesce pending key events before each draw (#0108)`, adds the drain, `Action::suspends_terminal()` and its two tests.

Install either commit on its own to take the before and after of the coalescing; the instrument is in both.

Actions classified as terminal-suspending: `EditCurrent`, `Reply`, `NewDraft`, `OpenLogFile`, `OpenConfigFile`, `OpenEventSource`, `SendContactVcard`, `ComposeEditSignature`, `EditSignatureFile`, `ComposeWizardSubmit`, `SearchResultOpen`, `SearchResultReply`, `SearchResultForward`.
The ten `edit_file` call sites the plan lists are all reachable from exactly those thirteen, and `suspend_terminal` has no caller that is not paired with one of them, so there is no external viewer or pager to cover.

The measurement fields above are still `TBD`: taking them is manual and was left to Sylvain.

## Links

- Plan: [docs/plans/preview-latency.md](../plans/preview-latency.md), Ticket A.
- Follow-on tickets from the same plan: B (retire inline image rendering, reverses [#0010](0010-inline-image-rendering.md)), C (retire auto-mark-read, reverses [#0087](0087-auto-mark-read-on-open.md)), D (retire the rich HTML render, reverses [#0091](0091-html-to-text-rendering.md)).
