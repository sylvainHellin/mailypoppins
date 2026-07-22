---
id: 0039
title: Durable pending_ops mutation queue (archive/delete/move/mark-read/send) with retry
type: refactor
priority: later
status: open
created: 2026-07-14
---

Stage 3 of the data-access-layer redesign. Plan: [data-access-layer](../plans/data-access-layer.md).

Make every state-changing operation durable.
Each mutation applies to the local store immediately, enqueues a `pending_ops` row, and the engine drains it against the server with retry and backoff, marking done or failed with feedback to the UI.
This replaces today's fire-and-forget optimistic model and delivers the sent-durability fix (retried APPEND-to-Sent).
Lands behind the still-present file layer so a queue bug cannot lose mail.

## Scope

1. `pending_ops(id, kind, target_message_id, payload, state, attempts, last_error, created)` drained by the engine; kinds: archive, delete, move, mark-read/unread, send.
2. TUI `Action` handlers enqueue an op + apply the local store change, instead of spawning a per-op IMAP task. `BgResult` carries op state transitions back to `App::update`.
3. Retry with exponential backoff; surfaced UI state for pending/failed ops (outbox semantics for send: "sent" = delivered + APPENDed).
4. Crash-safety: ops replay on startup; guard against duplicate apply and stuck rows.

## Acceptance criteria

- A mutation reflects instantly in the UI and is confirmed or retried in the background with visible feedback.
- Kill the process mid-op; on restart the op replays exactly once and converges.
- Send failure and Sent-APPEND failure both retry rather than silently dropping.
- Test harness decision made (IMAP mock vs accepted live-server validation) and documented.

## Unblocks

- [#0040](0040-drop-file-layer-cutover.md) (file layer can be removed once mutations are durable in the store).
