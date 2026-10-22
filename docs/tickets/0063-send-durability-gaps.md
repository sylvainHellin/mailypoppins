---
id: 0063
title: Send durability gaps (partial-recipient success, unresumable Graph pending_send rows)
type: bug
priority: later
status: open
created: 2026-08-06
---

From the architecture review synthesis, Tier 3: [2026-08-06_architecture-review-synthesis](../../.agents/handoff/2026-08-06_architecture-review-synthesis.md).
Effort: M each.
Shipped 2026-08-07: scope items 1, 2 and 4 and the double-submit admission gate, on the SMTP path.
Shipped offline 2026-10-22: scope item 3, the Graph half, with the staged send that keeps a large Graph reply threaded (PERSO-144 follow-up); see [The Graph half](#the-graph-half-2026-10-22).
The ticket stays open for the live checks listed there, under the Graph backend section of the [BACKLOG](../../BACKLOG.md).

Two holes the durable outbox of [#0037](0037-sqlite-store-engine-skeleton.md) left open.
Both lose a delivery quietly, which is the failure mode the outbox exists to prevent.

## Evidence

- Partial recipient success is recorded as full success: `src/send.rs:79-83` `submit_outcome` returns `SubmitOutcome::Accepted` as soon as `any_succeeded()` is true.
  The reasoning at `send.rs:74-78` is correct for the Sent-copy question it was written to answer, but the failed recipients recorded at `send.rs:68` `failed()` are never retried by anything and never surface after the status line is gone.
- Graph submissions stuck in `pending_send` can never be resumed: `src/send.rs:1262-1274` returns early for `AuthMethod::Graph` because the row holds RFC822 bytes while the Graph transport sends structured JSON.
  The row stays visible in `mp outbox list` and is only recoverable by a human re-sending from the draft.
- The stranded-submission path (`src/outbox.rs:672-702` `sweep_pending_sends`) parks ambiguous rows as failed, which is correct, but it is the only automated handling either case gets.
- Nothing stops one draft being submitted twice concurrently, so the TUI can send it twice: `Action::Send` on the cursor draft and an `Action::SendApproved` batch already in flight both reach `send::send_draft` on their own background thread, and an approved draft under the cursor is by definition also in the batch.
  There is no dedup on the way in.
  `build_draft_message` mints a fresh `Message-ID` per build (`message_id(None)`, `src/send.rs`), and `DurableSend::begin` enqueues on that id with no key on the draft itself, so the second run looks like an unrelated message to the outbox and to the Sent-copy search that a retry uses to avoid duplicating.
  Whichever settle runs second retires a file the other one already moved.
  Pre-existing and unchanged by [#0058](0058-send-path-unification.md), which unified the four copies of this orchestration without adding an admission gate; noted here because the fix belongs with the outbox, not with the callers.
  The cheap half is an in-process guard on the set of drafts a send is running for; the durable half is keying the outbox row on the draft so a resume cannot double-submit either.

## Scope

1. Record the per-recipient verdict durably, not just in the status line: extend the `outbox` envelope column or add a per-recipient table, so a partially delivered message names its undelivered recipients after a restart.
2. Retry the failed recipients only, with the existing backoff, and reach a terminal state that the user can see.
3. Give the Graph path a resumable representation: either store the structured payload alongside the RFC822 bytes, or reconstruct the Graph message from the stored bytes at resume time.
   **Decided** (2026-10-22): reconstruct from the stored bytes.
   Since PERSO-144 the Graph transport sends MIME, so the row's bytes plus the envelope's recipient roles (for the `Bcc:` header) are the whole message, and the resume plans it exactly as the live send did.
4. Keep the exactly-once marker semantics intact; a retry must not deliver twice to a recipient that already got a 250.

## Acceptance criteria

- A send where one of two recipients is rejected leaves an outbox row that names the rejected recipient and reaches a terminal state without manual intervention. **Met.**
- No recipient receives the message twice across a retry. **Met**, by `Envelope::outstanding()` being the only thing a resubmission attempts.
- A Graph account with a `pending_send` row left by a crash resumes automatically on the next run, or fails with an explicit, actionable reason. **Met offline**, live checks pending.
- Tests cover the partial-failure and the Graph-resume path offline. **Met**: the partial-failure, retry, recovery and admission-gate paths in `tests/outbox_integration.rs`, the Graph crash windows in `src/send/graph_submit/tests.rs` against an in-memory mailbox.

## What shipped

The verdicts are per recipient, and they are durable.
`Envelope` (the row's `envelope` column, no schema change) now carries `delivered:` and `rejected:` lines beside the addresses, `SubmitOutcome::PerRecipient` carries one verdict per recipient from the SMTP loop, and `record_submission` folds the set into the row: a recipient with no verdict parks it in `failed`, a recipient that can still be tried keeps it in `pending_send` under the existing backoff, and once nothing is outstanding it goes on to `done` if anybody took it and to `failed` if the server refused them all.
A resubmission attempts `Envelope::outstanding()` rather than the whole address list, so a recipient that answered 250 is never spoken to twice, including across an operator `mp outbox retry`.
A 5xx is now a rejection of that recipient rather than an ambiguous whole-row failure, which is what makes the partial case reach a terminal state on its own.

A message that reached some recipients and not others keeps a note in `last_error` for good.
That note is what keeps the row listed by `mp outbox list` after it is `done` (shown as `partial`, with the refused addresses named) and counted in `OutboxCounts::partial`, which the TUI badge renders as `OUTBOX n (1 partial)`.
Discarding the row is how a human closes it.

The admission gate is two halves, as the evidence above describes.
The envelope carries the draft key (frontmatter `id`, or the path for a draft that has none), `outbox::enqueue` refuses a draft that already has a `pending_send` or `sent_pending_append` row with `AlreadyInFlight`, and `send_draft` holds a process-wide slot per draft for the length of the send, so the TUI's cursor send and the approved batch it is also in cannot both submit it.
A `failed` or `done` row does not hold the gate: a deliberate re-send after a human has looked is the user's business.

One related hole closed on the way: `send_durably` marked the row as entering submission and then let a pre-transport error out of `submit` (an unparseable envelope sender, a transport that would not build) propagate with the marker still set, so the next resume read that marker as "died inside the SMTP session" and parked a message that had never been sent.
Both send paths now record such a failure as the clean pre-submission one it is, which puts the marker back to NULL.

What is *not* closed: `lettre` does not let a caller tell a TCP connect failure (clean, nothing was sent) from an i/o error on an established connection (ambiguous), so both still park the row in `failed` for a human, as they did before this ticket.
Sending while the SMTP server is unreachable therefore still needs `mp outbox retry` rather than resolving itself.

## Review follow-up (2026-08-07)

The review of the shipped commit passed and left four findings, all fixed in one follow-up:

- `build_draft_message` validated the normalised `from:` and then stored the raw one, so a `from: Doe, Jane <j@x.com>` enqueued cleanly and failed every submission it would ever get at the pre-submission step; the admission gate then refused each re-send of that draft and `retry` refuses a `pending_send` row, leaving `mp outbox discard` as the only exit.
  The built message now carries the normalised address, which is the one `submit` parses.
- The enqueue transaction began DEFERRED while reading to decide whether it may write, so under WAL the loser of a two-process race hit `SQLITE_BUSY_SNAPSHOT` at the INSERT.
  That error landed in the non-durable fallback and the message was sent with no outbox row and no gate.
  The transaction now begins IMMEDIATE (`Store::immediate_transaction`), and the fallback is narrowed: a busy store is reported to the user as retryable, and only a store that will not open at all still buys a non-durable send.
- The durable gate compared the decoded draft key, flattened by the envelope encoding, against the raw in-memory one, so a path-fallback key holding a tab or a trailing space never matched and the gate silently missed.
  Both sides are flattened now.
- The in-process gate was keyed by draft alone, so two accounts each holding a hand-written draft with the same frontmatter `id:` refused each other's send.
  It is keyed by account and draft, like the durable half.

Four further properties were judged worth writing down rather than changing:

- Downgrading to a binary older than this ticket skips the `delivered:` lines it does not know about and resubmits the whole recipient list, so a downgrade between a partial send and its resume can deliver twice.
  Upgrading back restores the recorded set; the lines survive the older binary untouched.
- `synchronous = NORMAL` (a #0037 property, unchanged here) means the marker and the verdicts survive a process crash, not an OS or power crash.
  A power cut inside the SMTP session can therefore still lose the marker that would have parked the row.
- The retryable bucket has no attempt cap: a `Kind::Client` condition that is permanent in practice keeps the row retrying at the 900s backoff ceiling for as long as the user leaves it there.
  It stays visible in `mp outbox list` throughout, and discarding it is the human's call.
- `mp outbox retry` still refuses a `pending_send` row by design, which is what makes the `from:` bug above a dead end rather than an inconvenience; with the normalisation fixed, no known path enqueues a row that cannot be submitted.

## The Graph half (2026-10-22)

A Graph submission is made of more than one request once the message is large, so "was the transport entered" needs a finer answer than for SMTP.
`src/send/graph_submit.rs` gives it one:

- The marker is committed immediately before the one request that delivers (`/me/sendMail`, or `POST /me/messages/{id}/send`), never before creating or filling a draft, which deliver nothing.
- The Graph id of a draft the send created goes into the envelope (`graph-draft:` line, no schema change) as soon as it exists, and leaves it when the draft is discarded.
- A row with no marker never delivered: the resume discards whatever an interrupted attempt left in Drafts (found by our `Message-ID`, which a MIME draft keeps, or by the recorded id), then submits afresh from the bytes; a copy already in Sent Items (an operator retry of a message that did arrive) is recorded as sent instead.
- A row with a marker lost the answer to its delivering request: a copy in Sent Items means delivered.
  Otherwise, 600 s after its send, the recorded draft is read by its Graph id (`isDraft`, `parentFolderId`): still a draft in Drafts means the send never took, and that same draft is sent rather than a new one; a draft in the Outbox is waited for up to an hour; no longer a draft means sent; gone, or no recorded draft, means nobody can tell, and the row is parked in `failed` with that reason.
- A delivering request counts as refused, and the row as cleanly failed, only on a 4xx or on Graph's throttling answers (429, a 503 with `Retry-After`).
  `sendMail` and `/send` answer 202 and finish later, so any other 5xx can follow a send Exchange took; it records nothing, like a timeout, and keeps the draft.
  A 404 on a draft's `/send` is read the same way, because a draft whose earlier send was taken late has left Drafts.
- A live delivering request that times out or gets such a 5xx records nothing, so the row stays `pending_send` with its marker and the next resume settles it the same way; the status line says "submitted, not yet confirmed".
- The resume claims the same in-process slot `send_draft` holds, so it never touches a row whose draft a send in this process is still filling (a row with no marker yet); the SMTP resume claims it too.
- A resume that records a row as sent retires the draft file it was built from, on both transports, as the live send does: marked `sent` once anyone has the message, removed once every recipient took it.
  Before the review of 2026-10-22 a file whose live send had failed stayed `approved` after the resume sent it, and `mp send-approved` sent it a second time under a fresh `Message-ID`.
  As a second guard, `send_draft` refuses a draft whose newest outbox row is `done`, unless the file carries that row's `Message-ID` (a re-send after a partial delivery, re-approved by hand).
- `mp outbox discard` on a Graph row that is not `done` deletes the row at once, offline too, and commits in the same transaction a server cleanup naming the row's `Message-ID` and recorded draft (`graph_cleanups`, carried across a store rebuild); the next Graph resume removes those drafts (`permanentDelete`) and then the cleanup, and never deletes an item that has left Drafts.
  A row a send in the daemon is working on is refused, since that send may be about to create the draft the cleanup looks for.
- A send whose marker does not commit does not go out (both transports): a MIME send is held back, a staged send discards its draft, and the row stays submittable with no marker.
  As a second guard, a Graph row with no marker whose recorded draft is no longer a plain draft in Drafts is given a marker and settled against the mailbox instead of being resent.
- `send_draft` refuses a draft whose newest row failed after its send started (`submission_started_at` set): that send may have arrived, so only `mp outbox retry <id>` (same `Message-ID`, and on Graph a Sent Items check first) or `mp outbox discard <id>` releases it.
- Every request of the staged send carries `Prefer: IdType="ImmutableId"`: a default Outlook id changes when the item moves, so without it the recorded draft answered 404 once `/send` moved it to the Outbox or Sent Items, and the resume's "on its way" and "sent" readings could never happen.

The large-message threading fix rides on the same staged send: a message too large for one MIME `sendMail` is a draft created from its own MIME with the largest attachments taken out, the attachments added one request each (a POST under 2.5 MB, an upload session above), then `send`.
Chosen over `createReply` on the parent because it keeps our `Message-ID` (which the outbox, the local Sent copy and the resume's lookup all key on), needs no Graph id of the parent, and serves forwards and new messages too; `internetMessageHeaders` was never an option, as Graph takes only `x-` names there.
Only a body that alone exceeds one request still goes as JSON, staged the same way, without threading headers and with an Exchange `Message-ID`.
That draft carries ours in a `singleValueExtendedProperty` (`MESSAGE_ID_PROPERTY`), which `$filter` can read, so the lookups find a JSON draft an interrupted create left behind, and its sent copy, by property as well as by `internetMessageId`.

### Live checks still owed (EVOQS)

- A reply with a 5 MB attachment to a message in the inbox: it arrives in the same Outlook conversation as its parent, on the sender's and on an external recipient's side, with our `Message-ID`, `In-Reply-To` and `References` in its headers.
- The MIME draft created by `POST /me/messages` reports our `Message-ID` as its `internetMessageId`, before and after `send` (the resume's lookup depends on it).
- A draft that was sent no longer answers the Drafts-folder lookup, and the Sent Items copy has `isDraft` false.
- Kill the daemon during the attachment upload of a large send, restart it: one message arrives, and Drafts holds nothing of it.
- A JSON draft (a body over 3.5 MB): `POST /me/messages` accepts the `singleValueExtendedProperties` entry, the `$filter` on it finds the draft, and finds the Sent Items copy after `send`.
- `GET /me/messages/{id}?$select=isDraft,parentFolderId` on a draft that was sent answers 404 or `isDraft` false, never a draft in Drafts.
- `mp outbox discard` on a parked Graph row whose draft is still in Drafts answers at once; after the next sync tick nothing of it is in Drafts or in Deleted Items.
- With `Prefer: IdType="ImmutableId"`: the id `POST /me/messages` answers for a draft still reads the item (`isDraft` true, `parentFolderId` the Outbox) while `/send` is under way, and reads the Sent Items copy (`isDraft` false) once it is sent; `GET /me/mailFolders/drafts?$select=id` and the items' `parentFolderId` come back in the same form, so the Drafts comparison holds.
- A row recorded by an earlier build holds a default (mutable) draft id; `permanentDelete` and `GET /me/messages/{id}` still accept it with the `Prefer` header set.
