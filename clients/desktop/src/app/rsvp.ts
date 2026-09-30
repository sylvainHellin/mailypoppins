// The invitations (clients/desktop/docs/reader.md, "Invitations"): the
// reader's invitation cards as Loadables, each account's Graph refusal, the
// RSVP choice (`tv`, the agenda's `V`, the card's buttons) and the RSVPs this
// window awaits. Pure functions over the model, which the reducer routes
// to, and at the bottom the commands they run.

import type { Dispatch } from "react";
import type { AgendaEvent, EventFrontmatter } from "@/protocol/types";
import type { GuiError, RsvpSettled } from "@/lib/gui-types";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import { cursorEvent, staleCalendar } from "@/app/calendar";
import {
  emptyLoadable,
  markStale,
  readerKey,
  type AppState,
  type Loadable,
  type OperationEnd,
  type RsvpDialog,
  type RsvpResponse,
  type RsvpRun,
} from "@/app/state";

const RSVP_EARLY_CAP = 16;

// ---------------------------------------------------------------------------
// Why an invitation cannot be answered, the TUI's words
// ---------------------------------------------------------------------------

/** The TUI's `rsvp_refusal` and its organizer guard, each em-dash a semicolon. */
export const NOT_REQUEST = "Only received invitations (REQUEST) can be RSVP'd";
export const CANCELLED = "This event was cancelled by the organizer; nothing to RSVP";
export const SUPERSEDED = "A newer version of this invitation has arrived; RSVP from that one";
export const ORGANIZER = "You are the organizer of this invite; nothing to RSVP";
export const NOT_INVITE = "Not a calendar invite";
export const SERVER_ONLY = "This search hit has no local copy to RSVP from";

function isRequest(event: EventFrontmatter | null): boolean {
  return event?.method?.toUpperCase() === "REQUEST";
}

/**
 * Why the reader's invitation can not be answered, or null when it can: not
 * a `REQUEST`, cancelled, superseded (the TUI's `rsvp_refusal`), then the
 * user's own invitation (the Sent mailbox), then the account's Graph refusal.
 */
export function rsvpRefusal(event: EventFrontmatter | null, organizer: boolean, graph: string | null): string | null {
  if (!isRequest(event)) return NOT_REQUEST;
  if (event?.cancelled) return CANCELLED;
  if (event?.superseded) return SUPERSEDED;
  if (organizer) return ORGANIZER;
  return graph;
}

/**
 * Why an agenda row can not be answered, the TUI's agenda order: cancelled,
 * the user organizes it, not a `REQUEST`; then the Graph refusal.
 */
export function agendaRsvpRefusal(row: AgendaEvent, graph: string | null): string | null {
  if (row.cancelled || row.event.cancelled) return CANCELLED;
  if (row.is_organizer) return ORGANIZER;
  if (!isRequest(row.event)) return NOT_REQUEST;
  return graph;
}

// ---------------------------------------------------------------------------
// The cards and the refusals
// ---------------------------------------------------------------------------

/** Whether `mailbox` of `account` is its Sent mailbox, whose invitations are the user's own. */
export function isSentMailbox(s: AppState, account: string, mailbox: string): boolean {
  const role =
    s.mailboxes[account]?.data?.mailboxes.find((m) => m.slug === mailbox)?.role ??
    s.bootstrap?.snapshot.mailboxes[account]?.find((m) => m.slug === mailbox)?.role;
  return role === "sent" || (role === undefined && mailbox === "sent");
}

/** The reader shows an invitation: its card is read, once. */
export function wantInvite(s: AppState, account: string, rowId: number): AppState {
  const key = readerKey(account, rowId);
  return s.invites[key] ? s : { ...s, invites: { ...s.invites, [key]: emptyLoadable<EventFrontmatter | null>() } };
}

/** Every card of `account` this window read goes stale with its agenda. */
export function staleInvites(s: AppState, account: string): AppState {
  let changed = false;
  const invites: AppState["invites"] = {};
  for (const [key, l] of Object.entries(s.invites)) {
    const next = key.startsWith(`${account}#`) ? markStale(l) : l;
    if (next !== l) changed = true;
    invites[key] = next;
  }
  return changed ? { ...s, invites } : s;
}

/** A calendar change: the agenda and every card of the account read again. */
export function staleInvitations(s: AppState, account: string): AppState {
  return staleInvites(staleCalendar(s, account), account);
}

/**
 * A bootstrap: row ids are per daemon instance, so another instance forgets
 * every card and the Graph refusals (the configuration may have moved); the
 * same one reads its cards again.
 */
export function bootstrapInvites(s: AppState, sameInstance: boolean): AppState {
  if (!sameInstance) return { ...s, invites: {}, inviteRefusals: {}, rsvpDialog: s.overlay === "rsvp" ? null : s.rsvpDialog, overlay: s.overlay === "rsvp" ? null : s.overlay };
  const invites: AppState["invites"] = {};
  for (const [key, l] of Object.entries(s.invites)) invites[key] = markStale(l);
  return { ...s, invites };
}

function inviteOf(s: AppState, key: string): Loadable<EventFrontmatter | null> {
  return s.invites[key] ?? emptyLoadable<EventFrontmatter | null>();
}

export function inviteLoaded(s: AppState, key: string, gen: number, event: EventFrontmatter | null): AppState {
  const l = inviteOf(s, key);
  return { ...s, invites: { ...s.invites, [key]: { ...l, data: event, loadedGen: gen, error: null } } };
}

export function inviteFailed(s: AppState, key: string, gen: number, error: GuiError): AppState {
  const l = inviteOf(s, key);
  return { ...s, invites: { ...s.invites, [key]: { ...l, loadedGen: gen, error } } };
}

export function inviteRefusalLoaded(s: AppState, account: string, refusal: string | null): AppState {
  return s.inviteRefusals[account] === refusal ? s : { ...s, inviteRefusals: { ...s.inviteRefusals, [account]: refusal } };
}

/** The accounts whose Graph refusal the window needs: the reader's invitation's, the agenda's, the New invitation form's. */
export function refusalsWanted(s: AppState): string[] {
  const out = new Set<string>();
  const meta = s.reader.meta;
  if (meta?.invite && s.view === "mail") out.add(meta.account);
  if (s.view === "calendar" && s.calendarView) out.add(s.calendarView.account);
  if (s.rsvpDialog) out.add(s.rsvpDialog.account);
  if (s.inviteDialog) out.add(s.inviteDialog.account);
  return [...out].filter((a) => !(a in s.inviteRefusals));
}

// ---------------------------------------------------------------------------
// The RSVPs
// ---------------------------------------------------------------------------

/** What the notices call a response. */
export const RESPONSE_LABEL: Record<RsvpResponse, string> = { accept: "Accept", tentative: "Tentative", decline: "Decline" };

/** The RSVP this window is sending for a row, if any. */
export function rsvpOf(s: AppState, account: string, rowId: number): RsvpRun | null {
  return s.rsvps.find((r) => r.account === account && r.row_id === rowId) ?? null;
}

export function openRsvpDialog(s: AppState, dialog: RsvpDialog): AppState {
  return { ...s, overlay: "rsvp", rsvpDialog: dialog, dialog: null, composeDialog: null, attachDialog: null, inviteDialog: null, signaturesDialog: null };
}

export function rsvpRequested(s: AppState, run: Omit<RsvpRun, "operation_id">): AppState {
  const next = { ...s, rsvps: [...s.rsvps, { ...run, operation_id: null }] };
  return next.overlay === "rsvp" ? { ...next, overlay: null, rsvpDialog: null } : next;
}

export function rsvpStarting(s: AppState): boolean {
  return s.rsvps.some((r) => r.operation_id === null);
}

export function isRsvpOperation(s: AppState, operationId: string): boolean {
  return s.rsvps.some((r) => r.operation_id === operationId);
}

/**
 * What a settled RSVP says: "Replied <response> to <summary>", and when no
 * recipient took it yet, that it waits in the outbox.
 */
export function rsvpText(run: RsvpRun, settled: RsvpSettled | null): string {
  const response = settled?.response || run.response;
  const text = `Replied ${response} to ${run.summary}`;
  return settled && !settled.delivered ? `${text}; queued in the outbox` : text;
}

/** An RSVP of this window ended: say how; the agenda and the account's cards read again. */
function rsvpEnded(s: AppState, run: RsvpRun, end: OperationEnd): AppState {
  let next: AppState = staleInvitations({ ...s, rsvps: s.rsvps.filter((r) => r !== run) }, run.account);
  const account = run.account;
  if ("dropped" in end) {
    return pushNotice(next, { kind: "send_failed", account, text: `The RSVP to ${run.summary} was interrupted; check the outbox` });
  }
  if (end.state !== "succeeded") {
    return pushNotice(next, { kind: "send_failed", account, text: `RSVP failed: ${end.error ?? end.state}` });
  }
  const settled = (end.result ?? null) as RsvpSettled | null;
  // A reply waiting in the outbox changed its counts.
  if (settled && !settled.delivered) next = { ...next, accounts: markStale(next.accounts) };
  return pushNotice(next, { kind: "applied", account, text: rsvpText(run, settled) });
}

/** An operation ended: an RSVP this window awaits settles; while one is unanswered an unknown id waits for it. */
export function rsvpSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.rsvps.find((r) => r.operation_id === end.operation_id);
  if (run) return rsvpEnded(s, run, end);
  if (rsvpStarting(s)) return { ...s, rsvpEarly: [...s.rsvpEarly, end].slice(-RSVP_EARLY_CAP) };
  return s;
}

/** `calendar_rsvp` answered: await the id, or settle it now if its end came first. */
export function rsvpStarted(s: AppState, token: number, operationId: string): AppState {
  const run = s.rsvps.find((r) => r.token === token);
  if (!run) return s;
  const started: RsvpRun = { ...run, operation_id: operationId };
  let next: AppState = { ...s, rsvps: s.rsvps.map((r) => (r === run ? started : r)) };
  const early = next.rsvpEarly.find((e) => e.operation_id === operationId);
  next = { ...next, rsvpEarly: rsvpStarting(next) ? next.rsvpEarly.filter((e) => e !== early) : [] };
  return early ? rsvpEnded(next, started, early) : next;
}

/** `calendar_rsvp` was refused: nothing was sent. */
export function rsvpStartFailed(s: AppState, token: number, message: string): AppState {
  const run = s.rsvps.find((r) => r.token === token);
  if (!run) return s;
  let next: AppState = { ...s, rsvps: s.rsvps.filter((r) => r !== run) };
  if (!rsvpStarting(next)) next = { ...next, rsvpEarly: [] };
  return pushNotice(next, { kind: "send_failed", account: run.account, text: `RSVP failed: ${message}` });
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

let nextRsvp = 1;

/** Send `response` to the invitation of a row, awaited as `rsvp`. */
export async function sendRsvp(
  dispatch: Dispatch<Action>,
  target: { account: string; row_id: number; summary: string },
  response: RsvpResponse,
): Promise<void> {
  const token = nextRsvp++;
  dispatch({ type: "rsvp_requested", token, account: target.account, row_id: target.row_id, response, summary: target.summary });
  try {
    const { operation_id } = await cmd.calendarRsvp(target.account, target.row_id, response);
    dispatch({ type: "rsvp_started", token, operation_id });
  } catch (e: unknown) {
    dispatch({ type: "rsvp_failed", token, error: asGuiError(e) });
  }
}

function busy(s: AppState, account: string, rowId: number): string | null {
  return rsvpOf(s, account, rowId) ? "A reply to this invitation is being sent" : null;
}

/**
 * `tv`: the RSVP choice for the selected email, after the TUI's guards: an
 * email that is no invitation, one in the Sent mailbox, a search hit the
 * store has no copy of, then the card's refusals. The event is the reader's
 * card when it is loaded, else read now.
 */
export async function openRsvp(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const say = (text: string) => dispatch({ type: "notice", text });
  const account = s.search?.account ?? s.selection.account;
  if (!account) return;
  const hit = s.search && s.selection.hit ? s.search.hits.find((h) => h.key === s.selection.hit) : undefined;
  if (hit && !s.selection.message) return say(hit.is_invite ? SERVER_ONLY : NOT_INVITE);
  const m = s.selection.message;
  if (!m) return;
  const list = s.messages.data?.kind === "messages" && s.messages.data.account === account ? s.messages.data : null;
  const row = list?.rows.find((r) => r.id === m.row_id);
  const searchHit = s.search?.hits.find((h) => h.account === account && h.row_id === m.row_id);
  const meta = s.reader.meta && s.reader.key === readerKey(account, m.row_id) ? s.reader.meta : null;
  const invite = row?.is_invite ?? searchHit?.is_invite ?? meta?.invite ?? false;
  if (!invite) return say(NOT_INVITE);
  const mailbox = searchHit?.mailbox ?? meta?.mailbox ?? (s.search ? null : s.selection.mailbox);
  if (mailbox && isSentMailbox(s, account, mailbox)) return say(ORGANIZER);
  const subject = row?.subject ?? searchHit?.subject ?? meta?.subject ?? "";
  let event: EventFrontmatter | null;
  const card = s.invites[readerKey(account, m.row_id)];
  if (card && card.loadedGen > 0 && card.gen === card.loadedGen && !card.error) {
    event = card.data;
  } else {
    try {
      event = await cmd.inviteGet(account, m.row_id);
    } catch (e: unknown) {
      return say(`RSVP failed: ${asGuiError(e).message}`);
    }
  }
  const refusal = rsvpRefusal(event, false, s.inviteRefusals[account] ?? null) ?? busy(s, account, m.row_id);
  if (refusal) return say(refusal);
  const summary = event?.summary?.trim() || subject || "(no subject)";
  dispatch({ type: "open_rsvp", dialog: { account, row_id: m.row_id, summary } });
}

/** The agenda's `V`: the RSVP choice for the cursor row, after the TUI's agenda guards. */
export function openAgendaRsvp(s: AppState, dispatch: Dispatch<Action>): void {
  const view = s.calendarView;
  const row = view ? cursorEvent(s) : null;
  if (!view || !row) {
    dispatch({ type: "notice", text: "The agenda has no event to answer" });
    return;
  }
  const refusal = agendaRsvpRefusal(row, s.inviteRefusals[view.account] ?? null) ?? busy(s, view.account, row.row_id);
  if (refusal) {
    dispatch({ type: "notice", text: refusal });
    return;
  }
  const summary = row.event.summary?.trim() || row.subject || "(no subject)";
  dispatch({ type: "open_rsvp", dialog: { account: view.account, row_id: row.row_id, summary } });
}
