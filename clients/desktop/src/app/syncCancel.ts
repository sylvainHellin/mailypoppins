// Cancelling a sync this window started (PERSO-321): the activity area's
// card and `sx` call `operation_cancel`, the call the TUI's `sx` and the
// CLI's ^C make. The daemon settles the pass `cancelled` at once and the pass
// stops at its next boundary, keeping what it ingested, which it announces to
// every window itself (PERSO-133); its end's words are `syncCancelledText`
// in the reducer's pending.ts. The words are the ones mp-client's
// `format::sync_cancelled_line` and `sync_cancel_too_late_line` give the TUI
// and the CLI.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import type { AppState } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

/** What a cancel that came too late says: the sync had already ended. */
export function syncCancelTooLateText(account: string): string {
  return `The sync of ${account} had already ended: nothing was cancelled`;
}

/** What `sx` says with no sync of this window running. */
export const NO_SYNC_TO_CANCEL = "No sync to cancel: none started from this window is running";

/**
 * Cancel one sync this window awaits. It stays awaited, so its `cancelled`
 * finish ends it and says what stays; one the daemon answers
 * `already_settled` says nothing was cancelled, and its own end takes the
 * card away. Resolves false when the cancel itself failed, which a notice
 * says.
 */
export async function cancelSync(dispatch: Dispatch<Action>, operationId: string, account: string): Promise<boolean> {
  try {
    const outcome = await cmd.operationCancel(operationId);
    if (outcome === "already_settled") dispatch({ type: "notice", text: syncCancelTooLateText(account) });
    return true;
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The cancel failed: ${asGuiError(e).message}`, level: "error" });
    return false;
  }
}

/**
 * `sx` and the palette's "Cancel the running sync": every sync this window
 * awaits. Another window's sync is not this one's to stop, so with none of
 * its own running it says so.
 */
export function cancelSyncs(s: AppState, dispatch: Dispatch<Action>): void {
  const running = Object.entries(s.syncs);
  if (running.length === 0) {
    dispatch({ type: "notice", text: NO_SYNC_TO_CANCEL });
    return;
  }
  for (const [operationId, run] of running) void cancelSync(dispatch, operationId, run.account);
}
