// The mutation dispatch: every message and draft mutation, the hold cancel
// and the sync trigger, as async functions over the typed commands. Each
// mutation applies at once, calls its command one account at a time in the
// order given, and reconciles from the answer (clients/desktop/docs/shell.md,
// "Mutations and pending state").

import { useMemo, type Dispatch } from "react";
import * as cmd from "@/lib/commands";
import {
  asGuiError,
  type DraftStatusFailure,
  type MovedTo,
  type MutationBatch,
  type SendRefusal,
  type SyncMode,
} from "@/lib/gui-types";
import type { Action } from "@/app/reducer";
import { useDispatch } from "@/app/store";
import type { MessageTarget, MutationKind, Target } from "@/app/state";

/** How many rows a call confirmed and how many it put back. */
export type MutationOutcome = { done: number; failed: number };

export type Mutations = {
  archive(rows: MessageTarget[]): Promise<MutationOutcome>;
  /** Delete; `delete` is a reserved word. */
  remove(rows: MessageTarget[]): Promise<MutationOutcome>;
  /** `destination` is a mailbox slug or its sidebar label. */
  move(rows: MessageTarget[], destination: string): Promise<MutationOutcome>;
  setFlag(rows: MessageTarget[], flagged: boolean): Promise<MutationOutcome>;
  setRead(rows: MessageTarget[], read: boolean): Promise<MutationOutcome>;
  /** Discard local drafts; an approved draft is refused and comes back. */
  discardDrafts(account: string, ids: string[]): Promise<MutationOutcome>;
  /** Approve drafts, or put them back to draft; a file that does not parse is refused alone. */
  setDraftStatus(account: string, ids: string[], approve: boolean): Promise<MutationOutcome>;
  cancelHold(operation_id: string): Promise<void>;
  sync(account: string, mode: SyncMode): Promise<void>;
  /**
   * Send one draft with the daemon's hold, approving it first when it is
   * not yet (the TUI's `x`); the draft is "sending" until the send settles.
   */
  sendDraft(account: string, id: string, subject: string | null): Promise<void>;
  /** Send every approved draft of `account` (the TUI's `cX`); `ids` are those the list shows. */
  sendApproved(account: string, ids: string[]): Promise<void>;
  /** Retry one outbox row: an operation the row shows as retrying until it ends. */
  retryOutboxRow(account: string, row_id: number): Promise<void>;
  /** Discard one outbox row: it leaves the view at once and comes back if refused. */
  discardOutboxRow(account: string, row_id: number): Promise<void>;
};

let nextBatch = 1;
let nextSend = 1;
let nextOutbox = 1;

/** The `draft.invalid` payload a `send_draft` refusal carries, if any. */
function refusalInvalid(e: unknown): NonNullable<SendRefusal["invalid"]> | null {
  return typeof e === "object" && e !== null && "invalid" in e ? ((e as SendRefusal).invalid ?? null) : null;
}

/** The rows grouped by account, in first-seen order, each group in the order given. */
function byAccount<T extends { account: string }>(rows: T[]): [string, T[]][] {
  const groups = new Map<string, T[]>();
  for (const r of rows) {
    const g = groups.get(r.account);
    if (g) g.push(r);
    else groups.set(r.account, [r]);
  }
  return [...groups.entries()];
}

type Call = (account: string, rowIds: number[]) => Promise<MutationBatch>;

async function mutateRows(
  dispatch: Dispatch<Action>,
  kind: MutationKind,
  rows: MessageTarget[],
  call: Call,
  opts: { destination?: string; value?: boolean } = {},
): Promise<MutationOutcome> {
  const outcome: MutationOutcome = { done: 0, failed: 0 };
  const value = opts.value ?? null;
  // Every row changes at once; the calls then go one account after the other.
  const batches = byAccount(rows).map(([account, group]) => {
    const batch = nextBatch++;
    const targets: Target[] = group.map((r) => ({ account, row_id: r.row_id }));
    dispatch({ type: "mutation_apply", batch, kind, targets, destination: opts.destination ?? null, value });
    return { account, group, batch, targets };
  });
  for (const { account, group, batch, targets } of batches) {
    try {
      const answer = await call(account, group.map((r) => r.row_id));
      const movedTo: MovedTo | null = answer.done.find((d) => d.moved_to)?.moved_to ?? null;
      dispatch({
        type: "mutation_settled",
        batch,
        kind,
        account,
        done: answer.done.map((d) => ({ account, row_id: d.row_id })),
        failed: answer.failed.map((f) => ({ target: { account, row_id: f.row_id }, reason: f.error.message })),
        value,
        moved_to: movedTo,
      });
      outcome.done += answer.done.length;
      outcome.failed += answer.failed.length;
    } catch (e: unknown) {
      dispatch({ type: "mutation_failed", batch, kind, account, targets, error: asGuiError(e) });
      outcome.failed += group.length;
    }
  }
  return outcome;
}

async function discard(dispatch: Dispatch<Action>, account: string, ids: string[]): Promise<MutationOutcome> {
  if (ids.length === 0) return { done: 0, failed: 0 };
  const batch = nextBatch++;
  const targets: Target[] = ids.map((draft) => ({ account, draft }));
  dispatch({ type: "mutation_apply", batch, kind: "discard", targets });
  try {
    const answer = await cmd.draftDiscard(account, ids);
    dispatch({
      type: "mutation_settled",
      batch,
      kind: "discard",
      account,
      done: answer.done.map((d) => ({ account, draft: d.id })),
      failed: answer.failed.map((f) => ({ target: { account, draft: f.id }, reason: f.error.message })),
    });
    return { done: answer.done.length, failed: answer.failed.length };
  } catch (e: unknown) {
    dispatch({ type: "mutation_failed", batch, kind: "discard", account, targets, error: asGuiError(e) });
    return { done: 0, failed: ids.length };
  }
}

/**
 * Why a draft's approve or demote was refused. A file that does not parse
 * (`-32010`) says where and why, from the `draft.invalid` payload.
 */
export function statusRefusal(f: DraftStatusFailure): string {
  if (!f.invalid) return f.error.message;
  const why = f.invalid.diagnostics
    .map((d) => (d.line !== null ? `line ${d.line}: ${d.message}` : d.message))
    .join("; ");
  return `does not parse: ${why} (${f.invalid.path})`;
}

async function draftStatus(dispatch: Dispatch<Action>, account: string, ids: string[], approve: boolean): Promise<MutationOutcome> {
  if (ids.length === 0) return { done: 0, failed: 0 };
  const kind: MutationKind = approve ? "approve" : "demote";
  const batch = nextBatch++;
  const targets: Target[] = ids.map((draft) => ({ account, draft }));
  dispatch({ type: "mutation_apply", batch, kind, targets });
  try {
    const answer = await (approve ? cmd.draftApprove : cmd.draftDemote)(account, ids);
    dispatch({
      type: "mutation_settled",
      batch,
      kind,
      account,
      done: answer.done.map((d) => ({ account, draft: d.id })),
      failed: answer.failed.map((f) => ({ target: { account, draft: f.id }, reason: statusRefusal(f) })),
    });
    return { done: answer.done.length, failed: answer.failed.length };
  } catch (e: unknown) {
    dispatch({ type: "mutation_failed", batch, kind, account, targets, error: asGuiError(e) });
    return { done: 0, failed: ids.length };
  }
}

export function createMutations(dispatch: Dispatch<Action>): Mutations {
  return {
    archive: (rows) => mutateRows(dispatch, "archive", rows, cmd.messageArchive),
    remove: (rows) => mutateRows(dispatch, "delete", rows, cmd.messageDelete),
    move: (rows, destination) =>
      mutateRows(dispatch, "move", rows, (a, ids) => cmd.messageMove(a, ids, destination), { destination }),
    setFlag: (rows, flagged) =>
      mutateRows(dispatch, "flag", rows, (a, ids) => cmd.messageSetFlag(a, ids, flagged), { value: flagged }),
    setRead: (rows, read) =>
      mutateRows(dispatch, "read", rows, (a, ids) => cmd.messageSetRead(a, ids, read), { value: read }),
    discardDrafts: (account, ids) => discard(dispatch, account, ids),
    setDraftStatus: (account, ids, approve) => draftStatus(dispatch, account, ids, approve),
    async cancelHold(operation_id) {
      dispatch({ type: "hold_cancel_requested", operation_id });
      try {
        const answer = await cmd.sendCancelHold(operation_id);
        dispatch({ type: "hold_cancel_answered", operation_id, cancelled: answer.cancelled });
      } catch (e: unknown) {
        dispatch({ type: "hold_cancel_failed", operation_id, error: asGuiError(e) });
      }
    },
    async sendDraft(account, id, subject) {
      const token = nextSend++;
      dispatch({ type: "send_requested", token, kind: "draft", account, drafts: [id], subject });
      try {
        // `hold: true` always: the daemon's `email.send_hold_secs` decides, 0 meaning none.
        const started = await cmd.sendDraft(account, id, true);
        dispatch({ type: "send_started", token, operation_id: started.operation_id, held: started.held });
      } catch (e: unknown) {
        dispatch({ type: "send_failed", token, error: asGuiError(e), invalid: refusalInvalid(e) });
      }
    },
    async sendApproved(account, ids) {
      const token = nextSend++;
      dispatch({ type: "send_requested", token, kind: "approved", account, drafts: ids, subject: null });
      try {
        const started = await cmd.sendApproved(account, true);
        dispatch({ type: "send_started", token, operation_id: started.operation_id, held: started.held });
      } catch (e: unknown) {
        dispatch({ type: "send_failed", token, error: asGuiError(e), invalid: null });
      }
    },
    async retryOutboxRow(account, row_id) {
      const token = nextOutbox++;
      dispatch({ type: "outbox_action_requested", token, kind: "retry", account, row_id });
      try {
        const { operation_id } = await cmd.outboxRetry(account, row_id);
        dispatch({ type: "outbox_retry_started", token, operation_id });
      } catch (e: unknown) {
        dispatch({ type: "outbox_action_failed", token, error: asGuiError(e) });
      }
    },
    async discardOutboxRow(account, row_id) {
      const token = nextOutbox++;
      dispatch({ type: "outbox_action_requested", token, kind: "discard", account, row_id });
      try {
        const answer = await cmd.outboxDiscard(account, row_id);
        dispatch({ type: "outbox_discarded", token, message_id: answer.message_id });
      } catch (e: unknown) {
        dispatch({ type: "outbox_action_failed", token, error: asGuiError(e) });
      }
    },
    async sync(account, mode) {
      dispatch({ type: "sync_requested" });
      try {
        const { operation_id } = await cmd.syncTrigger(account, mode);
        dispatch({ type: "sync_started", operation_id, account, mode });
      } catch (e: unknown) {
        dispatch({ type: "sync_failed", account, error: asGuiError(e) });
      }
    },
  };
}

/** The mutations bound to the store's dispatch. */
export function useMutations(): Mutations {
  const dispatch = useDispatch();
  return useMemo(() => createMutations(dispatch), [dispatch]);
}
