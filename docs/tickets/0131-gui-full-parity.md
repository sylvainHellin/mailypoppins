---
id: 0131
title: M2 to M4 of the native GUI, full parity with the TUI
type: feature
priority: next
status: open
created: 2026-09-25
---

Third ticket of the [native GUI plan](../plans/native-gui.md), milestones "M2: mutations with the undo hold", "M3: compose through the external editor" and "M4: calendar, contacts, signatures and config", plus the read slices that land in M1 beside #0129.
Blocked on #0129 (M1), and #0130 (M5) is blocked on this ticket, because the milestones run in order.

## Work

- Implement every `GUI parity` row of [docs/parity-matrix.md](../parity-matrix.md) in the plan's milestones M1 (the read slices), M2, M3 and M4, each backed by the same daemon methods the TUI uses and naming the identifiers it closes.
- Replace each row's planned `GUI location`, which names its milestone today, with the surface and interaction that shipped.
- Add GUI interaction tests and cross-client scenarios as slices land, and keep TUI parity green.
- Update help and the website alongside any changed command or interaction.

## Carried from the daemon phases

- The Phase 0 gate line asking every GUI-parity capability for a target interaction was answered only by the deferral list in `BACKLOG.md`; filling the `GUI location` column is that half of the line.
- M3, drafts: `draft.set_recipients` is not built, so `DFT-11`'s recipient rewrite is client-side in the TUI.
- M3, attachments: there is no `message.attachments` method, so a client must know a part index before it can materialise a part; `w` on a server-only search hit forwards no attachments.
- M3, sending: `send.approved` and `send.outbox_retry` render the whole outcome at the end rather than per message, although the progress events already carry what a per-message view needs.
- M1, sync: `mp watch --mailbox` is INBOX-only, and the daemon has no periodic tick that keeps a store fresh with no client connected.
- M4, contacts: `contact.search`'s refused-rebuild guard is only logged daemon-side, so a client cannot say a rebuild was refused.
- M4, signatures: `signature.changed` has no `signature.removed` twin, so a deleted signature stays in every client until it re-bootstraps.
- The daemon reaches into `mp-tui` for `build_mailboxes` and `resolve_date`; both lift into `mp-core` before a third client depends on the arrow.

## Settled deferrals

The first release is dark-only (`OBS-07`), and auto-mark-read, which #0110 retired, is not restored; `BACKLOG.md` records both.
The inline images and the rich HTML render #0109 and #0111 retired from the TUI come back in the GUI reader, which loads `message.html` with its `cid:` images inlined as `data:` URIs in a sandboxed frame (`RD-05`).

## M2 landed

M2, mutations with the undo hold, landed on the `gui-m2` branch on 2026-09-30, in the commits after `a127ce3a`, each tagged `(#0131)`.
It closes `MSG-01` to `MSG-09`, `SND-04`, `SYN-06` and the archive half of `LST-09`, whose `GUI location` now names the surface that shipped.

What shipped:

- Tauri commands `message_archive`, `message_delete`, `message_move`, `message_set_flag`, `message_set_read`, `draft_discard`, `send_hold_status`, `send_cancel_hold` and `sync_trigger`.
- Every message and draft command takes a batch, sends one daemon call per row in order with `settle: false`, fails a row alone on a `-32602` refusal, and stops the batch on an error about the whole batch.
- Fixture handlers for all of them, with a mutation journal, the drain 1.5 s after an account's last mutation, the `rollback` and `rollback:<n>` simulations, the `hold` simulation, and a seeded 60 s hold in the bootstrap.
- A frontend model of what the daemon has not confirmed: pending changes per row and per axis, the list generation guard with the pending overlay on every list answer, marks, holds, syncs, and activity notices capped at 20.
- The keys `a`, `d`, `u`, `*`, `M`, `v`, `Ctrl+a`, Escape for the marks, `ss`, `sS` and `X`, a palette entry for each action, and a DESKTOP section in the key help.
- The archive and delete confirmation, the move picker, row controls (mark box, unread and flag toggles, a pending mark), the "N marked" count and the reader toolbar.
- The activity area with live regions, and the send-hold countdown with Cancel.
- Mark read on an explicit open: Enter, a double-click, or Tab into the reader.
- 168 vitest tests (79 at M1) and 78 Rust tests (57 at M1), plus two ignored: the live-daemon test and the ts-rs export `pnpm gen:types` runs.

Decisions taken in M2:

- Archive asks for confirmation as delete does, mirroring the TUI.
- There is no undo for archive, delete or move, because the daemon has none; a rollback notice is the only undo surface.
- `u` cancels the newest held send while one is live, as the TUI's `u` does, and toggles read otherwise.
- `X` dismisses the newest activity notice; the TUI binds `X` only as the continuation of `cX`, so a bare `X` is free.
- Quick and full sync (`ss`, `sS`) and draft discard ship in M2, since `SYN-06` and `MSG-02` need them.
- The outbox view waits for M3, and `SYN-06` ships without a queue depth.

Review findings fixed before the close-out:

- Only a `-32602` refusal fails one row of a batch; any other error fails the whole batch.
- The fixture's `moved_to.selector` keeps the daemon's percent-encoded bracketed Message-ID.
- A pending change keeps one saved state per axis of its row, so a flag and a read in flight fail each on its own.
- Every restore re-reads the list and the counts, since the saved index is a guess once the list changed.
- The notices and the marked count render into live regions mounted empty, before their first text.
- The model keeps the newest 20 notices, and `X` and the palette dismiss the newest one or all of them.
- A `u` while every live hold is already being cancelled says nothing, as the TUI re-queues its cancel silently.
- The hold-cancel test skips a tick that lands after the cancel.

Known limits carried:

- App keys stop inside the cross-origin reader frame until a click returns focus to the app.
- List windowing is off, so every row of a mailbox is mounted, and `Ctrl+a` over a large one marks them all.
- A row moved into a mailbox the window shows appears there only after the drain's `state.invalidate`.

Open for M3:

- The outbox view (`SND-06`), and with it a queue depth for `SYN-06`.
- A `message.fetch` command for server-only search hits (`LST-09`'s `f`), which the palette badges "later" today.
- Search hits streamed from the server are not laid under the pending changes, so a hit for a row still leaving can show.
- A hold card's "Sent" and "Send cancelled" line is a `role="status"` that mounts with its text, which a screen reader may not announce.
- The fixture's `home` account fails its sync login by design, as its sync health says, so `ss` there always ends in a failure notice.

## Exit gate

- Every GUI-parity row is implemented and validated, or carries a settled deferral recorded in `BACKLOG.md`.
- Every current TUI user capability has a GUI path.
- CLI automation, diagnostics, daemon-administration and migration-only classifications carry an explicit rationale.
- TUI parity remains green.
