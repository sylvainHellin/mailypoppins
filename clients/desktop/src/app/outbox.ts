// The outbox (clients/desktop/docs/shell.md, "Outbox"): each account's
// `outbox_list` as a Loadable, the view that replaces the list pane's
// content, the retry and the discard of one row, and the queue depth.
// Pure functions over the model; the reducer routes to them.

import type { OutboxListing, OutboxRetryOutcome, OutboxRow } from "@/protocol/types";
import type { GuiError } from "@/lib/gui-types";
import type { ActionId } from "@/keymap/catalog";
import { pushNotice } from "@/app/pending";
import {
  emptyLoadable,
  markStale,
  type AppState,
  type Loadable,
  type MutationDialog,
  type OperationEnd,
  type OutboxAction,
} from "@/app/state";

const OUTBOX_EARLY_CAP = 16;

/** The sidebar's outbox line: what waits, what failed, what went to only some recipients. */
export type OutboxSummary = { queued: number; failed: number; partial: number };

/** Whether the daemon admits a retry of a row in this state (`send.outbox_retry`). */
export function retryable(row: OutboxRow): boolean {
  return row.state === "failed" || row.state === "sent_pending_append";
}

/** The chip a row shows. A `done` row is listed only when it is partial, which is never a failure (SND-08). */
export function stateLabel(row: OutboxRow): string {
  if (row.partial) return "Partly delivered";
  switch (row.state) {
    case "pending_send":
      return "Queued";
    case "failed":
      return "Failed";
    case "sent_pending_append":
      return "Sent, copy owed";
    default:
      return row.state;
  }
}

/** The account's outbox Loadable, or a fresh one. */
function loadableOf(s: AppState, account: string): Loadable<OutboxListing> {
  return s.outbox[account] ?? emptyLoadable<OutboxListing>();
}

/** Re-read `account`'s outbox, creating the Loadable when it has none. */
export function staleOutbox(s: AppState, account: string): AppState {
  const l = s.outbox[account];
  return { ...s, outbox: { ...s.outbox, [account]: l ? markStale(l) : emptyLoadable<OutboxListing>() } };
}

/** A bootstrap re-reads every outbox this window has read. */
export function staleAllOutboxes(s: AppState): AppState {
  const names = new Set(s.bootstrap?.snapshot.accounts.map((a) => a.name) ?? []);
  const outbox: AppState["outbox"] = {};
  for (const [account, l] of Object.entries(s.outbox)) if (names.has(account)) outbox[account] = markStale(l);
  const view = s.outboxView && names.has(s.outboxView.account) ? s.outboxView : null;
  return { ...s, outbox, outboxView: view };
}

export function outboxLoaded(s: AppState, account: string, gen: number, listing: OutboxListing): AppState {
  const l = loadableOf(s, account);
  return { ...s, outbox: { ...s.outbox, [account]: { ...l, data: listing, loadedGen: gen, error: null } } };
}

export function outboxFailed(s: AppState, account: string, gen: number, error: GuiError): AppState {
  const l = loadableOf(s, account);
  return { ...s, outbox: { ...s.outbox, [account]: { ...l, loadedGen: gen, error } } };
}

/** The listing without `rowId`, its counts moved with it. */
function withoutRow(listing: OutboxListing, rowId: number): OutboxListing {
  const row = listing.rows.find((r) => r.id === rowId);
  if (!row) return listing;
  const open = row.state === "pending_send" || row.state === "sent_pending_append" ? 1 : 0;
  return {
    ...listing,
    rows: listing.rows.filter((r) => r !== row),
    counts: {
      open: listing.counts.open - open,
      failed: listing.counts.failed - (row.state === "failed" ? 1 : 0),
      partial: listing.counts.partial - (row.partial ? 1 : 0),
    },
  };
}

/** The rows a discard of this window has not taken away yet. */
function discarding(s: AppState, account: string): ReadonlySet<number> {
  return new Set(s.outboxActions.filter((a) => a.kind === "discard" && a.account === account).map((a) => a.row_id));
}

/** The rows the view shows: the listing's, less those being discarded. */
export function outboxRows(s: AppState, account: string): OutboxRow[] {
  const rows = s.outbox[account]?.data?.rows ?? [];
  const gone = discarding(s, account);
  return gone.size === 0 ? rows : rows.filter((r) => !gone.has(r.id));
}

/** The action of this window running on a row, if any. */
export function rowAction(s: AppState, account: string, rowId: number): OutboxAction | null {
  return s.outboxActions.find((a) => a.account === account && a.row_id === rowId) ?? null;
}

/**
 * The sidebar's counts: the listing's once loaded, less the rows a discard
 * is taking away; before that, what `list_accounts` or the bootstrap said,
 * which has no partial count.
 */
export function outboxSummary(s: AppState, account: string): OutboxSummary {
  const listing = s.outbox[account]?.data;
  if (listing) {
    const gone = discarding(s, account);
    const hidden = listing.rows.filter((r) => gone.has(r.id));
    const minus = (pred: (r: OutboxRow) => boolean) => hidden.filter(pred).length;
    return {
      queued: listing.counts.open - minus((r) => r.state === "pending_send" || r.state === "sent_pending_append"),
      failed: listing.counts.failed - minus((r) => r.state === "failed"),
      partial: listing.counts.partial - minus((r) => r.partial),
    };
  }
  const info = s.accounts.data?.find((a) => a.name === account)?.outbox ?? s.bootstrap?.snapshot.outbox[account];
  return { queued: info?.queued ?? 0, failed: info?.failed ?? 0, partial: 0 };
}

/** "1 queued, 2 failed, 1 partly delivered", the parts that are not 0. */
export function summaryText(o: OutboxSummary): string {
  return [
    o.queued > 0 ? `${o.queued} queued` : null,
    o.failed > 0 ? `${o.failed} failed` : null,
    o.partial > 0 ? `${o.partial} partly delivered` : null,
  ]
    .filter(Boolean)
    .join(", ");
}

/** Whether the loaded listing already counts `rowId` as open (queued, or owed its Sent copy). */
function openInListing(s: AppState, account: string, rowId: number): boolean {
  const row = s.outbox[account]?.data?.rows.find((r) => r.id === rowId);
  return row !== undefined && (row.state === "pending_send" || row.state === "sent_pending_append");
}

/**
 * What waits to reach a server (SYN-06): the queued outbox rows of every
 * account, the sends and retries of this window still running, and the
 * optimistic changes whose commands have not answered. A retry of a row the
 * listing already counts as open (a `sent_pending_append` row) is counted
 * once, in the outbox.
 */
export function queueDepth(s: AppState): { outbox: number; sending: number; changes: number; total: number } {
  const accounts = s.bootstrap?.snapshot.accounts.map((a) => a.name) ?? [];
  const outbox = accounts.reduce((n, a) => n + outboxSummary(s, a).queued, 0);
  const retries = s.outboxActions.filter((a) => a.kind === "retry" && !openInListing(s, a.account, a.row_id));
  const sending = s.sends.length + retries.length;
  const changes = Object.keys(s.pending).length;
  return { outbox, sending, changes, total: outbox + sending + changes };
}

/** "3 waiting for the server: 1 in the outbox, 1 sending, 1 change", or null at 0. */
export function queueText(d: ReturnType<typeof queueDepth>): string | null {
  if (d.total === 0) return null;
  const parts = [
    d.outbox > 0 ? `${d.outbox} in the outbox` : null,
    d.sending > 0 ? `${d.sending} sending` : null,
    d.changes > 0 ? `${d.changes} ${d.changes === 1 ? "change" : "changes"}` : null,
  ].filter(Boolean);
  return `${d.total} waiting for the server: ${parts.join(", ")}`;
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/** Open `account`'s outbox in the list pane; its listing is read on the first open. */
export function openOutbox(s: AppState, account: string): AppState {
  const next = s.outbox[account] ? s : staleOutbox(s, account);
  const same = s.outboxView?.account === account;
  return { ...next, outboxView: { account, cursor: same ? s.outboxView!.cursor : null } };
}

export function closeOutbox(s: AppState): AppState {
  return s.outboxView ? { ...s, outboxView: null } : s;
}

/**
 * The actions that read the mailbox selection or its marks, which the view
 * hides: while it shows, a key for one does nothing and the palette or the
 * menu answers {@link CLOSE_OUTBOX_FIRST}. Moves (`j`, `k`, `J`, `K`, `G`,
 * `gg`, the pages) reach the view's own cursor through `move_selection`, and
 * `cn` is left out, since a new draft needs no row and goes to the view's
 * account.
 */
const SELECTION_ACTIONS: ReadonlySet<ActionId> = new Set<ActionId>([
  "open_message",
  "copy_selector",
  "archive",
  "delete",
  "move",
  "toggle_flag",
  "toggle_read",
  "mark_toggle",
  "mark_range",
  "mark_all",
  "mark_clear",
  "reply",
  "reply_all",
  "forward",
  "open_editor",
  "edit_recipients",
  "approve",
  "demote",
  "send",
  "send_all",
]);

export const CLOSE_OUTBOX_FIRST = "Close the outbox first (Escape): this acts on the mailbox selection";

/** Whether the outbox view hides what `id` acts on. */
export function hiddenByOutbox(s: AppState, id: ActionId): boolean {
  return s.outboxView !== null && SELECTION_ACTIONS.has(id);
}

/** The row under the view's cursor: the one it names, else the first. */
export function cursorRow(s: AppState): OutboxRow | null {
  const view = s.outboxView;
  if (!view) return null;
  const rows = outboxRows(s, view.account);
  return rows.find((r) => r.id === view.cursor) ?? rows[0] ?? null;
}

/** Move the view's cursor, as the list's `move_selection` does. */
export function moveOutboxCursor(s: AppState, to: number | "first" | "last", relative: boolean): AppState {
  const view = s.outboxView;
  if (!view) return s;
  const rows = outboxRows(s, view.account);
  if (rows.length === 0) return s;
  const cur = Math.max(0, rows.findIndex((r) => r.id === (cursorRow(s)?.id ?? -1)));
  let idx: number;
  if (to === "first") idx = 0;
  else if (to === "last") idx = rows.length - 1;
  else idx = relative ? cur + to : to;
  idx = Math.max(0, Math.min(rows.length - 1, idx));
  return { ...s, outboxView: { ...view, cursor: rows[idx].id }, focusSeq: s.focusSeq + 1 };
}

export function selectOutboxRow(s: AppState, rowId: number): AppState {
  const view = s.outboxView;
  if (!view) return s;
  return { ...s, outboxView: { ...view, cursor: rowId } };
}

// ---------------------------------------------------------------------------
// The confirmations
// ---------------------------------------------------------------------------

/** Why a row may already have done what a discard or a retry is about, or null. */
function discardWarning(row: OutboxRow): string | null {
  if (row.partial) return "It went to some recipients; discarding it drops the note of who never got it.";
  if (row.state === "sent_pending_append") {
    return `It was delivered; discarding it gives up its copy in ${row.target_mailbox ?? "the Sent mailbox"}.`;
  }
  if (row.never_submitted) return "It was never submitted; discarding it means it is never sent.";
  return "It ended without a verdict, so it may already have been delivered.";
}

/** The confirmation of a retry, or why the row cannot be retried. */
export function retryDialog(account: string, row: OutboxRow): MutationDialog | string {
  if (!retryable(row)) {
    return `Only a failed row, or one whose Sent copy is owed, can be retried; row ${row.id} is ${stateLabel(row).toLowerCase()}`;
  }
  if (row.state === "sent_pending_append") {
    return {
      kind: "outbox_retry",
      account,
      row_id: row.id,
      title: `File the Sent copy of row ${row.id}?`,
      detail: row.message_id,
      warning: null,
    };
  }
  return {
    kind: "outbox_retry",
    account,
    row_id: row.id,
    title: `Send row ${row.id} again?`,
    detail: row.message_id,
    warning: "A failed submission may already have been delivered: check that it did not arrive, or its recipients get it twice.",
  };
}

export function discardDialog(account: string, row: OutboxRow): MutationDialog {
  return {
    kind: "outbox_discard",
    account,
    row_id: row.id,
    title: `Discard row ${row.id}?`,
    detail: row.message_id,
    warning: discardWarning(row),
  };
}

// ---------------------------------------------------------------------------
// Retry and discard
// ---------------------------------------------------------------------------

export function outboxActionRequested(s: AppState, action: Omit<OutboxAction, "operation_id">): AppState {
  let next: AppState = { ...s, outboxActions: [...s.outboxActions, { ...action, operation_id: null }] };
  // A discarded row leaves the view at once: the cursor goes to the next row.
  const view = next.outboxView;
  if (action.kind === "discard" && view && view.account === action.account && cursorRow(s)?.id === action.row_id) {
    const before = outboxRows(s, action.account);
    const at = before.findIndex((r) => r.id === action.row_id);
    const to = before[at + 1] ?? before[at - 1] ?? null;
    next = { ...next, outboxView: { ...view, cursor: to?.id ?? null } };
  }
  return next;
}

function withoutAction(s: AppState, action: OutboxAction): AppState {
  return { ...s, outboxActions: s.outboxActions.filter((a) => a !== action) };
}

function retryStarting(s: AppState): boolean {
  return s.outboxActions.some((a) => a.kind === "retry" && a.operation_id === null);
}

/** What a settled retry says: the CLI's "row N is gone" or "row N is now …", in the GUI's words. */
export function retryResult(outcome: OutboxRetryOutcome): { kind: "applied" | "send_failed" | "send_partial"; text: string } {
  const id = outcome.row_id;
  const copies = outcome.completed > 0 ? "; its Sent copy is filed" : "";
  switch (outcome.state) {
    case null:
      return { kind: "applied", text: `Outbox row ${id} sent${copies}` };
    case "failed":
      return { kind: "send_failed", text: `Outbox row ${id} failed again; the outbox says why` };
    case "sent_pending_append":
      return { kind: "applied", text: `Outbox row ${id} sent; its Sent copy is still owed` };
    case "done":
      return { kind: "send_partial", text: `Outbox row ${id} partly delivered; the outbox names who never got it` };
    case "pending_send":
      return { kind: "applied", text: `Outbox row ${id} is queued again; the next sync sends it` };
    default:
      return { kind: "applied", text: `Outbox row ${id} is now ${outcome.state}` };
  }
}

/** A retry of this window ended: say how, and re-read the outbox. */
function retryEnded(s: AppState, action: OutboxAction, end: OperationEnd): AppState {
  const next = staleOutbox(withoutAction(s, action), action.account);
  const account = action.account;
  if ("dropped" in end) {
    return pushNotice(next, { kind: "send_failed", account, text: `The retry of outbox row ${action.row_id} was interrupted; check the outbox` });
  }
  if (end.state !== "succeeded") {
    const why = end.error ?? end.state;
    return pushNotice(next, { kind: "send_failed", account, text: `The retry of outbox row ${action.row_id} failed: ${why}` });
  }
  const outcome = (end.result ?? { row_id: action.row_id, state: null, completed: 0 }) as OutboxRetryOutcome;
  const { kind, text } = retryResult(outcome);
  return pushNotice(next, { kind, account, text });
}

/**
 * An operation ended. A retry this window awaits settles; while an
 * `outbox_retry` is unanswered, an end of an unknown id is held for it.
 */
export function outboxSignal(s: AppState, end: OperationEnd): AppState {
  const action = s.outboxActions.find((a) => a.kind === "retry" && a.operation_id === end.operation_id);
  if (action) return retryEnded(s, action, end);
  if (retryStarting(s)) return { ...s, outboxEarly: [...s.outboxEarly, end].slice(-OUTBOX_EARLY_CAP) };
  return s;
}

export function isOutboxOperation(s: AppState, operationId: string): boolean {
  return s.outboxActions.some((a) => a.kind === "retry" && a.operation_id === operationId);
}

export { retryStarting };

/** `outbox_retry` answered: await the id, or settle it now if its end came first. */
export function outboxRetryStarted(s: AppState, token: number, operationId: string): AppState {
  const action = s.outboxActions.find((a) => a.token === token);
  if (!action) return s;
  const started: OutboxAction = { ...action, operation_id: operationId };
  let next: AppState = { ...s, outboxActions: s.outboxActions.map((a) => (a === action ? started : a)) };
  const early = next.outboxEarly.find((e) => e.operation_id === operationId);
  next = { ...next, outboxEarly: retryStarting(next) ? next.outboxEarly.filter((e) => e !== early) : [] };
  return early ? retryEnded(next, started, early) : next;
}

/** `outbox_retry` or `outbox_discard` was refused: the row is as it was, and a notice says why. */
export function outboxActionFailed(s: AppState, token: number, message: string): AppState {
  const action = s.outboxActions.find((a) => a.token === token);
  if (!action) return s;
  let next = staleOutbox(withoutAction(s, action), action.account);
  if (!retryStarting(next)) next = { ...next, outboxEarly: [] };
  const verb = action.kind === "retry" ? "retry" : "discard";
  return pushNotice(next, { kind: "send_failed", account: action.account, text: `The ${verb} of outbox row ${action.row_id} was refused: ${message}` });
}

/** `outbox_discard` answered: the row is gone, and the listing is read again. */
export function outboxDiscarded(s: AppState, token: number, messageId: string): AppState {
  const action = s.outboxActions.find((a) => a.token === token);
  if (!action) return s;
  // Out of the listing now, so it does not come back before the reload lands.
  const l = s.outbox[action.account];
  const data = l?.data ? withoutRow(l.data, action.row_id) : null;
  const dropped: AppState = l && data ? { ...s, outbox: { ...s.outbox, [action.account]: { ...l, data } } } : s;
  const next = staleOutbox(withoutAction(dropped, action), action.account);
  return pushNotice(next, { kind: "applied", account: action.account, text: `Discarded outbox row ${action.row_id} (${messageId})` });
}
