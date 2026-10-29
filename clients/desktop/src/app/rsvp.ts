// The invitations (clients/desktop/docs/reader.md, "Invitations"): the
// reader's invitation cards as Loadables, the respond choice (`tv`, the
// Calendar view's `V`, the cards' buttons) and the answers this window
// awaits, all through `event.respond` (PERSO-200). Which answers an account
// takes is the daemon's call; what is refused here is what no answer could
// mean. Pure functions over the model, which the reducer routes to, and at
// the bottom the commands they run. The New invitation form's Graph refusal
// lives here too, as the cards' did before the daemon answered them.

import type { Dispatch } from "react";
import type { CalendarEvent, EventFrontmatter, EventRespondResult } from "@/protocol/types";
import type { GuiError, InviteCard } from "@/lib/gui-types";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import { cursorEntry, eventTitle, IMIP_PREFIX, isImip, isOrganizer, staleCalendar } from "@/app/calendar";
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
export const NOT_INVITATION = "This event is not an invitation; there is nothing to answer";
export const SERVER_ONLY = "This search hit has no local copy to RSVP from";
export const BUSY = "A reply to this invitation is being sent";

function isRequest(event: EventFrontmatter | null): boolean {
  return event?.method?.toUpperCase() === "REQUEST";
}

/**
 * Why the reader's invitation can not be answered, or null when it can: not
 * a `REQUEST`, cancelled, superseded (the TUI's `rsvp_refusal`), then the
 * user's own invitation (the Sent mailbox).
 */
export function rsvpRefusal(event: EventFrontmatter | null, organizer: boolean): string | null {
  if (!isRequest(event)) return NOT_REQUEST;
  if (event?.cancelled) return CANCELLED;
  if (event?.superseded) return SUPERSEDED;
  if (organizer) return ORGANIZER;
  return null;
}

/**
 * Why a calendar event can not be answered, the TUI's
 * `open_rsvp_overlay_for`: cancelled, the user organizes it, a server event
 * that invites nobody.
 */
export function eventRsvpRefusal(e: CalendarEvent): string | null {
  if (e.cancelled) return CANCELLED;
  if (isOrganizer(e)) return ORGANIZER;
  if (e.own_response === "none" && !isImip(e)) return NOT_INVITATION;
  return null;
}

/** The event an invitation card answers: its server event, else the mail's own. */
export function cardEventId(card: InviteCard | null, rowId: number): string {
  return card?.event_id ?? `${IMIP_PREFIX}${rowId}`;
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
  return s.invites[key] ? s : { ...s, invites: { ...s.invites, [key]: emptyLoadable<InviteCard | null>() } };
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

/** A calendar change: the Calendar view and every card of the account read again. */
export function staleInvitations(s: AppState, account: string): AppState {
  return staleInvites(staleCalendar(s), account);
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

function inviteOf(s: AppState, key: string): Loadable<InviteCard | null> {
  return s.invites[key] ?? emptyLoadable<InviteCard | null>();
}

export function inviteLoaded(s: AppState, key: string, gen: number, card: InviteCard | null): AppState {
  const l = inviteOf(s, key);
  return { ...s, invites: { ...s.invites, [key]: { ...l, data: card, loadedGen: gen, error: null } } };
}

export function inviteFailed(s: AppState, key: string, gen: number, error: GuiError): AppState {
  const l = inviteOf(s, key);
  return { ...s, invites: { ...s.invites, [key]: { ...l, loadedGen: gen, error } } };
}

export function inviteRefusalLoaded(s: AppState, account: string, refusal: string | null): AppState {
  return s.inviteRefusals[account] === refusal ? s : { ...s, inviteRefusals: { ...s.inviteRefusals, [account]: refusal } };
}

/** The account whose Graph refusal the New invitation form needs, while it is open and not asked yet. */
export function refusalsWanted(s: AppState): string[] {
  const account = s.inviteDialog?.account;
  return account && !(account in s.inviteRefusals) ? [account] : [];
}

// ---------------------------------------------------------------------------
// The RSVPs
// ---------------------------------------------------------------------------

/** What the notices call a response. */
export const RESPONSE_LABEL: Record<RsvpResponse, string> = { accept: "Accept", tentative: "Tentative", decline: "Decline" };

/** The answer this window is sending for an event, if any. */
export function rsvpOf(s: AppState, account: string, event: string): RsvpRun | null {
  return s.rsvps.find((r) => r.account === account && r.event === event) ?? null;
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
 * What a settled answer says: "Replied <response> to <summary>", then
 * whether the organizer heard of it, the TUI's `rsvp_line`: a reply mail no
 * recipient took yet waits in the outbox, an answer the user chose not to
 * send says so, and a server answer that should have reached the organizer
 * and did not says that.
 */
export function rsvpText(run: RsvpRun, settled: EventRespondResult | null): string {
  const response = settled?.response || run.response;
  const text = `Replied ${response} to ${run.summary}`;
  if (!settled || settled.delivered) return text;
  if (!settled.send_response) return `${text}; no response sent to the organizer`;
  return settled.via === "imip" ? `${text}; queued in the outbox` : `${text}, but the organizer could not be told`;
}

/** An RSVP of this window ended: say how; the agenda and the account's cards read again. */
function rsvpEnded(s: AppState, run: RsvpRun, end: OperationEnd): AppState {
  let next: AppState = staleInvitations({ ...s, rsvps: s.rsvps.filter((r) => r !== run) }, run.account);
  const account = run.account;
  if ("dropped" in end) {
    return pushNotice(next, { kind: "send_failed", account, text: `The RSVP to ${run.summary} was interrupted; check the outbox` });
  }
  if (end.state === "cancelled") {
    // The daemon does not stop a reply it already handed to the transport.
    return pushNotice(
      { ...next, accounts: markStale(next.accounts) },
      { kind: "operation_cancelled", account, text: `Stopped waiting for the RSVP to ${run.summary}; the daemon may still send it, check the outbox` },
    );
  }
  if (end.state !== "succeeded") {
    return pushNotice(next, { kind: "send_failed", account, text: `RSVP failed: ${end.error ?? end.state}` });
  }
  const settled = (end.result ?? null) as EventRespondResult | null;
  // A reply mail waiting in the outbox changed its counts.
  if (settled && !settled.delivered && settled.via === "imip") next = { ...next, accounts: markStale(next.accounts) };
  return pushNotice(next, { kind: "applied", account, text: rsvpText(run, settled) });
}

/** An operation ended: an RSVP this window awaits settles; while one is unanswered an unknown id waits for it. */
export function rsvpSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.rsvps.find((r) => r.operation_id === end.operation_id);
  if (run) return rsvpEnded(s, run, end);
  if (rsvpStarting(s)) return { ...s, rsvpEarly: [...s.rsvpEarly, end].slice(-RSVP_EARLY_CAP) };
  return s;
}

/** `event_respond` answered: await the id, or settle it now if its end came first. */
export function rsvpStarted(s: AppState, token: number, operationId: string): AppState {
  const run = s.rsvps.find((r) => r.token === token);
  if (!run) return s;
  const started: RsvpRun = { ...run, operation_id: operationId };
  let next: AppState = { ...s, rsvps: s.rsvps.map((r) => (r === run ? started : r)) };
  const early = next.rsvpEarly.find((e) => e.operation_id === operationId);
  next = { ...next, rsvpEarly: rsvpStarting(next) ? next.rsvpEarly.filter((e) => e !== early) : [] };
  return early ? rsvpEnded(next, started, early) : next;
}

/** `event_respond` was refused: nothing was sent. */
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

/** What an answer addresses: the event, and what its notices call it. */
export type RsvpTarget = { account: string; event: string; summary: string };

/**
 * Send `response` to an event, awaited as `rsvp`: one `event.respond`, with
 * the comment (a blank one is none) and, on a server event, whether the
 * organizer is told; an invitation mail's answer is its reply mail, so it
 * names no choice.
 */
export async function sendRsvp(
  dispatch: Dispatch<Action>,
  target: RsvpTarget,
  response: RsvpResponse,
  options: { comment?: string; sendResponse?: boolean | null } = {},
): Promise<void> {
  const token = nextRsvp++;
  dispatch({ type: "rsvp_requested", token, account: target.account, event: target.event, response, summary: target.summary });
  const comment = options.comment?.trim() || null;
  const sendResponse = target.event.startsWith(IMIP_PREFIX) ? null : (options.sendResponse ?? null);
  try {
    const { operation_id } = await cmd.eventRespond(target.account, target.event, response, comment, sendResponse);
    dispatch({ type: "rsvp_started", token, operation_id });
  } catch (e: unknown) {
    dispatch({ type: "rsvp_failed", token, error: asGuiError(e) });
  }
}

function busy(s: AppState, account: string, event: string): string | null {
  return rsvpOf(s, account, event) ? BUSY : null;
}

/**
 * `tv`: the respond choice for the selected email, after the TUI's guards:
 * an email that is no invitation, one in the Sent mailbox, a search hit the
 * store has no copy of, then the card's refusals. The card is the reader's
 * when it is loaded, else read now; it names the server event the answer
 * goes to on an account with a calendar backend.
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
  let card: InviteCard | null;
  const loaded = s.invites[readerKey(account, m.row_id)];
  if (loaded && loaded.loadedGen > 0 && loaded.gen === loaded.loadedGen && !loaded.error) {
    card = loaded.data;
  } else {
    try {
      card = await cmd.inviteGet(account, m.row_id);
    } catch (e: unknown) {
      return say(`RSVP failed: ${asGuiError(e).message}`);
    }
  }
  const event = cardEventId(card, m.row_id);
  const refusal = rsvpRefusal(card?.event ?? null, false) ?? busy(s, account, event);
  if (refusal) return say(refusal);
  const summary = card?.event.summary?.trim() || subject || "(no subject)";
  dispatch({ type: "open_rsvp", dialog: { account, event, summary, server: card?.event_id != null } });
}

/** The respond choice for a calendar event, after the TUI's guards; null when it opened, else why not. */
export function rsvpDialogFor(s: AppState, e: CalendarEvent): RsvpDialog | string {
  const refusal = eventRsvpRefusal(e) ?? busy(s, e.account, e.id);
  if (refusal) return refusal;
  return { account: e.account, event: e.id, summary: eventTitle(e), server: !isImip(e) };
}

/** The Calendar view's `V`: the respond choice for the cursor event. */
export function openEventRsvp(s: AppState, dispatch: Dispatch<Action>): void {
  const entry = s.calendarView ? cursorEntry(s) : null;
  if (!entry) {
    dispatch({ type: "notice", text: "The calendar has no event to answer" });
    return;
  }
  const dialog = rsvpDialogFor(s, entry.event);
  if (typeof dialog === "string") {
    dispatch({ type: "notice", text: dialog });
    return;
  }
  dispatch({ type: "open_rsvp", dialog });
}
