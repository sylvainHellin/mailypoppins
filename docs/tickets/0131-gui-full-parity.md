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

## Exit gate

- Every GUI-parity row is implemented and validated, or carries a settled deferral recorded in `BACKLOG.md`.
- Every current TUI user capability has a GUI path.
- CLI automation, diagnostics, daemon-administration and migration-only classifications carry an explicit rationale.
- TUI parity remains green.
