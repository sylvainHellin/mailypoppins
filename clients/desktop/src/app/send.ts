// Sending (clients/desktop/docs/shell.md, "Send"): `x` sends the draft under
// the cursor and `cX` every approved draft of the account, each after the
// TUI's confirmation, and always with the daemon's hold, whose window is
// `email.send_hold_secs`. The hold card and the outcome are the reducer's.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { createMutations } from "@/app/mutations";
import { conversationDraft, draftsShown, filteredDrafts, sendingRefusal, type AppState, type MutationDialog } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

function notice(dispatch: Dispatch<Action>, text: string): void {
  dispatch({ type: "notice", text });
}

/** The sidebar label of the shown mailbox, what the TUI's "In …" names. */
function mailboxLabel(s: AppState): string {
  const { account, mailbox } = s.selection;
  if (!account || !mailbox) return "Mail";
  const listed = s.mailboxes[account]?.data?.mailboxes.find((m) => m.slug === mailbox)?.label;
  return listed ?? s.bootstrap?.snapshot.mailboxes[account]?.find((m) => m.slug === mailbox)?.label ?? mailbox;
}

/** The fields of a draft the send confirmation names. */
type SendRow = { id: string; status: string; to: string | null; subject: string | null };

/** The TUI's confirmation: a `draft` status warns that confirming approves it too. */
function sendDialog(account: string, row: SendRow): MutationDialog {
  return {
    kind: "send",
    targets: [{ account, draft: row.id }],
    subject: row.subject,
    title: row.status === "draft" ? "Draft is not approved. Approve and send?" : "Send this email?",
    detail: `To: ${row.to ?? ""} - ${row.subject ?? ""}`,
  };
}

/**
 * `x`: the draft under the cursor of the Drafts list, one draft whatever
 * is marked, as the TUI's `selected_email`, or a conversation's draft member
 * (PERSO-146), whose status and recipients a `draft_preview` reads first.
 */
export function sendCursor(s: AppState, dispatch: Dispatch<Action>): void {
  const member = draftsShown(s) ? null : conversationDraft(s);
  if (member && s.selection.account) return void sendMember(s, dispatch, s.selection.account, member);
  if (!draftsShown(s) || !s.selection.draft || !s.selection.account) {
    if (s.selection.message || s.selection.hit) notice(dispatch, "Send needs a draft; received mail has nothing to send");
    return;
  }
  const account = s.selection.account;
  const row = filteredDrafts(s.messages.data, "").find((d) => d.id === s.selection.draft);
  if (!row) return;
  const busy = sendingRefusal(s, [{ account, draft: row.id }]);
  if (busy) return notice(dispatch, busy);
  dispatch({ type: "open_dialog", dialog: sendDialog(account, row) });
}

/** `x` on a conversation's draft member: the confirmation, once `draft_preview` read the draft. */
async function sendMember(s: AppState, dispatch: Dispatch<Action>, account: string, id: string): Promise<void> {
  const busy = sendingRefusal(s, [{ account, draft: id }]);
  if (busy) return notice(dispatch, busy);
  try {
    const p = await cmd.draftPreview(account, id);
    dispatch({ type: "open_dialog", dialog: sendDialog(account, { id, status: p.status, to: p.to, subject: p.subject }) });
  } catch (e: unknown) {
    notice(dispatch, `Could not read ${id}: ${asGuiError(e).message}`);
  }
}

/**
 * `cX`, Drafts only: every approved draft of the account, in one daemon
 * operation. The drafts the list shows approved are "sending" until it settles.
 */
export function sendAll(s: AppState, dispatch: Dispatch<Action>): void {
  if (!draftsShown(s) || !s.selection.account) {
    notice(dispatch, "Send all approved (c X) is only available in Drafts");
    return;
  }
  const account = s.selection.account;
  const targets = filteredDrafts(s.messages.data, "")
    .filter((d) => d.status === "approved")
    .map((d) => ({ account, draft: d.id }));
  const busy = sendingRefusal(s, targets);
  if (busy) return notice(dispatch, busy);
  const dialog: MutationDialog = {
    kind: "send_approved",
    account,
    targets,
    title: "Send all approved emails?",
    detail: `In ${mailboxLabel(s)}`,
  };
  dispatch({ type: "open_dialog", dialog });
}

/** The send confirmation's OK. Marks stay: the TUI's `x` and `cX` leave its selection alone. */
export function runSend(dialog: Extract<MutationDialog, { kind: "send" | "send_approved" }>, dispatch: Dispatch<Action>): void {
  dispatch({ type: "overlay", overlay: null });
  const m = createMutations(dispatch);
  if (dialog.kind === "send") {
    const [t] = dialog.targets;
    void m.sendDraft(t.account, t.draft, dialog.subject);
  } else {
    void m.sendApproved(dialog.account, dialog.targets.map((t) => t.draft));
  }
}
