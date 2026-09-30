// The mutation dispatch: every message and draft mutation, the hold cancel
// and the sync trigger, as async functions over the typed commands. Each
// mutation applies at once, calls its command one account at a time in the
// order given, and reconciles from the answer (clients/desktop/docs/shell.md,
// "Mutations and pending state").

import { useMemo, type Dispatch } from "react";
import * as cmd from "@/lib/commands";
import { asGuiError, type MovedTo, type MutationBatch, type SyncMode } from "@/lib/gui-types";
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
  cancelHold(operation_id: string): Promise<void>;
  sync(account: string, mode: SyncMode): Promise<void>;
};

let nextBatch = 1;

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
    async cancelHold(operation_id) {
      dispatch({ type: "hold_cancel_requested", operation_id });
      try {
        const answer = await cmd.sendCancelHold(operation_id);
        dispatch({ type: "hold_cancel_answered", operation_id, cancelled: answer.cancelled });
      } catch (e: unknown) {
        dispatch({ type: "hold_cancel_failed", operation_id, error: asGuiError(e) });
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
