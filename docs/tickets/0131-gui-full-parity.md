---
id: 0131
title: Phase 9 of the native GUI, full parity with the TUI
type: feature
priority: next
status: open
created: 2026-09-25
---

Fourth ticket of the GUI half of the [native GUI plan](../plans/native-gui.md), sections "Full GUI capability rollout" and "Phase 9: Full GUI parity".
Blocked on #0130; slices that need no composition can start after #0129.

## Work

- Implement every `GUI parity` row of [docs/parity-matrix.md](../parity-matrix.md) in the plan's seventeen vertical slices, each backed by the same daemon methods the TUI uses and naming the identifiers it closes.
- Fill each row's `GUI location`, which reads `TBD (Phase 9)` today.
- Add GUI interaction tests and cross-client scenarios as slices land, and keep TUI parity green.
- Update help and the website alongside any changed command or interaction.

## Carried from the daemon phases

- The Phase 0 gate line asking every GUI-parity capability for a target interaction was answered only by the deferral list in `BACKLOG.md`; filling the `GUI location` column is that half of the line.
- Slice 7, drafts: `draft.set_recipients` is not built, so `DFT-11`'s recipient rewrite is client-side in the TUI.
- Slice 9, attachments: there is no `message.attachments` method, so a client must know a part index before it can materialise a part; `w` on a server-only search hit forwards no attachments.
- Slice 10, sending: `send.approved` and `send.outbox_retry` render the whole outcome at the end rather than per message, although the progress events already carry what a per-message view needs.
- Slice 11, sync: `mp watch --mailbox` is INBOX-only, and the daemon has no periodic tick that keeps a store fresh with no client connected.
- Slice 12, contacts: `contact.search`'s refused-rebuild guard is only logged daemon-side, so a client cannot say a rebuild was refused.
- Slice 14, signatures: `signature.changed` has no `signature.removed` twin, so a deleted signature stays in every client until it re-bootstraps.
- The daemon reaches into `mp-tui` for `build_mailboxes` and `resolve_date`; both lift into `mp-core` before a third client depends on the arrow.

## Settled deferrals

The first release is dark-only (`OBS-07`), and inline images, auto-mark-read and the rich HTML preview retired by #0109, #0110 and #0111 are not restored; `BACKLOG.md` records both.

## Exit gate

- Every GUI-parity row is implemented and validated, or carries a settled deferral recorded in `BACKLOG.md`.
- Every current TUI user capability has a GUI path.
- CLI automation, diagnostics, daemon-administration and migration-only classifications carry an explicit rationale.
- TUI parity remains green.
