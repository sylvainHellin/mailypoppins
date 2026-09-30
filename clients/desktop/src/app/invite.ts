// A new invitation (clients/desktop/docs/shell.md, "Calendar", "New
// invitation"): the form's own checks, the sends this window awaits as
// `send_invite`, and what their ends say. Pure functions over the model,
// which the reducer routes to, and at the bottom the commands they run.

import type { Dispatch } from "react";
import type { SendOutcome } from "@/protocol/types";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import { staleInvitations } from "@/app/rsvp";
import { markStale, type AppState, type InviteSendRun, type OperationEnd } from "@/app/state";

const INVITE_EARLY_CAP = 16;

/** What the form lacks, the Rust layer's sentences (`calendar.rs`). */
export const NO_SUBJECT = "An invitation needs a subject";
export const NO_START = "An invitation needs a start";
export const NO_RECIPIENT = "An invitation needs at least one recipient in To or Cc";

const addressed = (v: string | null | undefined) => (v ?? "").split(/[,;]/).some((a) => a.trim() !== "");

/** The first thing the form lacks, in the daemon's order: a subject, a start, one recipient; null when it can be sent. */
export function inviteProblem(fields: cmd.InviteFields): string | null {
  if (fields.subject.trim() === "") return NO_SUBJECT;
  if (fields.start.trim() === "") return NO_START;
  if (!addressed(fields.to) && !addressed(fields.cc)) return NO_RECIPIENT;
  return null;
}

/** The account a new invitation sends from: the Calendar view's, else the selection's. */
export function inviteAccount(s: AppState): string | null {
  return (s.view === "calendar" ? s.calendarView?.account : null) ?? s.selection.account;
}

export function openInviteDialog(s: AppState, account: string): AppState {
  return { ...s, overlay: "invite", inviteDialog: { account }, dialog: null, composeDialog: null, attachDialog: null, rsvpDialog: null };
}

export function inviteSendRequested(s: AppState, run: Omit<InviteSendRun, "operation_id">): AppState {
  return { ...s, inviteSends: [...s.inviteSends, { ...run, operation_id: null }] };
}

export function inviteSending(s: AppState): boolean {
  return s.inviteSends.some((r) => r.operation_id === null);
}

export function isInviteOperation(s: AppState, operationId: string): boolean {
  return s.inviteSends.some((r) => r.operation_id === operationId);
}

/** What a settled invitation says, by how many recipients took it. */
export function inviteResult(subject: string, outcome: SendOutcome | null): { kind: "applied" | "send_failed" | "send_partial"; text: string } {
  const recipients = outcome?.recipients ?? [];
  const took = recipients.filter((r) => r.delivered).length;
  if (recipients.length > 0 && took === 0) {
    return { kind: "send_failed", text: `The invitation ${subject} reached no recipient; the outbox says why` };
  }
  if (took < recipients.length) {
    return { kind: "send_partial", text: `The invitation ${subject} reached ${took} of ${recipients.length} recipients; the outbox names who never got it` };
  }
  return { kind: "applied", text: `Sent the invitation ${subject}` };
}

/** An invitation of this window ended: say how; the agenda, the cards and the outbox counts read again. */
function inviteEnded(s: AppState, run: InviteSendRun, end: OperationEnd): AppState {
  const base: AppState = { ...s, inviteSends: s.inviteSends.filter((r) => r !== run) };
  const next = { ...staleInvitations(base, run.account), accounts: markStale(base.accounts) };
  const account = run.account;
  if ("dropped" in end) {
    return pushNotice(next, { kind: "send_failed", account, text: `The invitation ${run.subject} was interrupted; check the outbox` });
  }
  if (end.state !== "succeeded") {
    return pushNotice(next, { kind: "send_failed", account, text: `The invitation ${run.subject} failed: ${end.error ?? end.state}` });
  }
  const { kind, text } = inviteResult(run.subject, (end.result ?? null) as SendOutcome | null);
  return pushNotice(next, { kind, account, text });
}

/** An operation ended: an invitation this window awaits settles; while one is unanswered an unknown id waits for it. */
export function inviteSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.inviteSends.find((r) => r.operation_id === end.operation_id);
  if (run) return inviteEnded(s, run, end);
  if (inviteSending(s)) return { ...s, inviteSendEarly: [...s.inviteSendEarly, end].slice(-INVITE_EARLY_CAP) };
  return s;
}

/** `send_invite` answered: the form closes, and the id is awaited, or settled now if its end came first. */
export function inviteSendStarted(s: AppState, token: number, operationId: string): AppState {
  const run = s.inviteSends.find((r) => r.token === token);
  if (!run) return s;
  const started: InviteSendRun = { ...run, operation_id: operationId };
  let next: AppState = { ...s, inviteSends: s.inviteSends.map((r) => (r === run ? started : r)) };
  if (next.overlay === "invite") next = { ...next, overlay: null, inviteDialog: null };
  const early = next.inviteSendEarly.find((e) => e.operation_id === operationId);
  next = { ...next, inviteSendEarly: inviteSending(next) ? next.inviteSendEarly.filter((e) => e !== early) : [] };
  return early ? inviteEnded(next, started, early) : next;
}

/** `send_invite` was refused: nothing was sent, and the form, still open, shows why. */
export function inviteSendFailed(s: AppState, token: number): AppState {
  const next: AppState = { ...s, inviteSends: s.inviteSends.filter((r) => r.token !== token) };
  return inviteSending(next) ? next : { ...next, inviteSendEarly: [] };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

let nextInvite = 1;

/** The palette's "New invitation" and the Calendar view's toolbar button: the form, for the view's or the selection's account. */
export function newInvitation(s: AppState, dispatch: Dispatch<Action>): void {
  const account = inviteAccount(s);
  if (!account) {
    dispatch({ type: "notice", text: "No account is selected" });
    return;
  }
  dispatch({ type: "open_invite", account });
}

/**
 * Send the form: what it lacks is refused here, without a call; a refusal
 * of the Rust layer or the daemon comes back as its sentence, for the form
 * to show. Null once the send started, and the form closes.
 */
export async function submitInvite(dispatch: Dispatch<Action>, account: string, fields: cmd.InviteFields): Promise<string | null> {
  const problem = inviteProblem(fields);
  if (problem) return problem;
  const token = nextInvite++;
  const subject = fields.subject.trim();
  dispatch({ type: "invite_send_requested", token, account, subject });
  try {
    const { operation_id } = await cmd.sendInvite(account, fields);
    dispatch({ type: "invite_send_started", token, operation_id });
    return null;
  } catch (e: unknown) {
    dispatch({ type: "invite_send_failed", token });
    return asGuiError(e).message;
  }
}
