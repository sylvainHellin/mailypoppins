// Changing, cancelling and deleting an event (CAL-12, CAL-13, PERSO-255;
// clients/desktop/docs/shell.md, "Calendar"): `E` and the card's Edit open
// the event form with the event's values, and Save sends `event_update` with
// the fields that differ from what the form opened with, so a change made in
// Outlook to another field survives; `D` and the card's Cancel meeting or
// Delete ask first, a cancel with a comment for the attendees. Each runs as
// an `event_change` operation; the daemon refuses what the user may not do
// in its own sentence. A failed or vanished update opens the form again with
// what was typed; an update sets fields to values, so Save sends it again
// safely. A failed or vanished cancel or delete opens its confirmation again
// with the comment; the daemon counts a repeat of one that went through as
// done. Either comes back over the view when nothing else is open, else it
// is kept for Edit or `D` on the event and a notice says so. Pure functions over the model, which the reducer routes to, and at
// the bottom the commands.

import type { Dispatch } from "react";
import type { CalendarEvent, CalendarInfo, EventCancelResult, EventPatchParams } from "@/protocol/types";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import { cursorEntry, eventTitle, isOrganizer, staleCalendar } from "@/app/calendar";
import { addresses, eventProblem, localZone, openEventDialog } from "@/app/eventForm";
import type { AppState, EventCalendar, EventChangeRun, EventDialog, EventFields, EventRemoveDialog, OperationEnd } from "@/app/state";

const CHANGE_EARLY_CAP = 16;

export const NOTHING_CHANGED = "Nothing changed yet";
export const ORGANIZER_ONLY = "Only the organizer can change this event; the reminder and show-as are yours";
export const RESTARTED_CHANGE = "The daemon restarted before the change was confirmed; Save sends it again";
/** Why a vanished cancel or delete asks again: a repeat of one that went through counts as done. */
export const RESTARTED_REMOVE = "The daemon restarted before it was confirmed; confirming again is safe, and counts as done if it went through";

/** The fields an attendee may change on their own copy. */
const ATTENDEE_FIELDS = new Set(["reminder", "show_as"]);

const pad = (n: number) => String(n).padStart(2, "0");
/** A `datetime-local` value of `at` in this window's zone. */
const wallTime = (at: Date) => `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}T${pad(at.getHours())}:${pad(at.getMinutes())}`;
/** The day before `day`, `YYYY-MM-DD`. */
function dayBefore(day: string): string {
  const at = new Date(`${day}T12:00:00Z`);
  at.setUTCDate(at.getUTCDate() - 1);
  return at.toISOString().slice(0, 10);
}

/** The form's fields filled with `event`'s values, its times in this window's zone. */
export function editFields(event: CalendarEvent, calendar: EventCalendar): EventFields {
  const people = (role: string) =>
    event.attendees
      .filter((a) => a.role === role)
      .map((a) => a.email)
      .join(", ");
  const t = event.time;
  const allDay = t.kind === "all-day";
  const last = allDay && t.end > t.start ? dayBefore(t.end) : "";
  return {
    calendar: calendar.key,
    title: event.summary,
    allDay,
    start: allDay ? t.start : wallTime(new Date(t.start)),
    end: allDay ? (last > t.start ? last : "") : wallTime(new Date(t.end)),
    location: event.location ?? "",
    to: people("required"),
    optional: people("optional"),
    rooms: people("resource"),
    teams: event.is_online_meeting,
    reminder: event.reminder == null ? "" : String(event.reminder),
    noReminder: event.reminder == null,
    showAs: event.show_as,
    body: event.body ?? "",
  };
}

/** Why `event` cannot be changed from mp: an invitation mail's, one of a series, or one on a calendar that takes no writes; null when it can. */
export function unchangeable(event: CalendarEvent, calendars: readonly CalendarInfo[] | null): string | null {
  if (event.id.startsWith("imip:")) return "An invitation mail's event can only be answered (V)";
  if (event.kind !== "single" || event.series_master_id) return `${eventTitle(event)} is part of a recurring series, which mp cannot change yet`;
  const calendar = (calendars ?? []).find((c) => c.account === event.account && c.id === event.calendar);
  if (!calendar) return `${event.account} has no calendar ${event.calendar}`;
  if (!calendar.can_edit || !calendar.features.includes("write")) return `The calendar ${calendar.name} takes no changes from mp`;
  return null;
}

/** The edit form on `event`, or why it cannot be changed. */
export function editDialogFor(event: CalendarEvent, calendars: readonly CalendarInfo[] | null): EventDialog | string {
  if (event.cancelled) return `${eventTitle(event)} was cancelled by its organizer; Delete removes your copy`;
  const why = unchangeable(event, calendars);
  if (why) return why;
  const info = (calendars ?? []).find((c) => c.account === event.account && c.id === event.calendar);
  if (!info) return `${event.account} has no calendar ${event.calendar}`;
  const calendar: EventCalendar = {
    key: `${info.account}/${info.id}`,
    account: info.account,
    id: info.id,
    label: `${info.account}/${info.name}`,
    teams: info.features.includes("online-meeting"),
    freeBusy: info.features.includes("free-busy"),
  };
  const fields = editFields(event, calendar);
  return {
    fields,
    calendars: [calendar],
    transactionId: null,
    error: null,
    edit: { account: event.account, event: event.id, organizer: isOrganizer(event), online: event.is_online_meeting, original: fields },
  };
}

/**
 * What `D` does on `event`: cancel a meeting the user organizes, which tells
 * its attendees; delete an event that tells nobody (no attendees, or the
 * user's copy of a cancelled meeting); someone else's live meeting is
 * declined instead, which this says.
 */
export function removeDialogFor(event: CalendarEvent, calendars: readonly CalendarInfo[] | null): EventRemoveDialog | string {
  const why = unchangeable(event, calendars);
  if (why) return why;
  const meeting = event.attendees.length > 0;
  let cancel: boolean;
  if (event.cancelled) cancel = false;
  else if (isOrganizer(event)) cancel = meeting;
  else if (!event.organizer && !meeting) cancel = false;
  else return `${eventTitle(event)} is someone else's meeting: decline it (Respond) to tell the organizer`;
  return { account: event.account, event: event.id, summary: eventTitle(event), cancel, attendees: event.attendees.length };
}

/** The start and end as the daemon reads them; an all-day event with no last day lasts one day. */
function span(f: EventFields): { start: string; end?: string; duration?: string } {
  if (!f.allDay) return { start: f.start, end: f.end };
  return f.end.trim() ? { start: f.start, end: f.end.trim() } : { start: f.start, duration: "P1D" };
}

/** The attendees of the three lists, each in its role. */
function people(f: EventFields) {
  return [
    ...addresses(f.to).map((email) => ({ email, role: "required" })),
    ...addresses(f.optional).map((email) => ({ email, role: "optional" })),
    ...addresses(f.rooms).map((email) => ({ email, role: "resource" })),
  ];
}

/** The reminder as `event.update` takes it: minutes, or null for none. */
const reminderOf = (f: EventFields) => (f.noReminder || !f.reminder.trim() ? null : Number(f.reminder.trim()));

/** The fields an edit form changed against what it opened with, its times in `zone`; empty on a new-event form. */
export function changedFields(dialog: EventDialog, zone: string): EventPatchParams {
  const was = dialog.edit?.original;
  if (!was) return {};
  const f = dialog.fields;
  const fields: EventPatchParams = {};
  if (f.title.trim() !== was.title.trim()) fields.summary = f.title.trim();
  const now = span(f);
  const then = span(was);
  if (f.allDay !== was.allDay || now.start !== then.start || now.end !== then.end || now.duration !== then.duration) {
    Object.assign(fields, now, { all_day: f.allDay, zone });
  }
  if (f.location.trim() !== was.location.trim()) fields.location = f.location.trim() || null;
  if (f.body.trimEnd() !== was.body.trimEnd()) fields.body = f.body.trim() ? f.body : null;
  const key = (list: { email: string; role: string }[]) =>
    list
      .map((a) => `${a.email.toLowerCase()} ${a.role}`)
      .sort()
      .join(",");
  if (key(people(f)) !== key(people(was))) fields.attendees = people(f);
  if (f.teams && !was.teams) fields.online_meeting = true;
  if (reminderOf(f) !== reminderOf(was)) fields.reminder = reminderOf(f);
  if (f.showAs !== was.showAs) fields.show_as = f.showAs;
  return fields;
}

/** What an edit form lacks: the fields' own problem (a title only when the event had one), no change, or an attendee's change beyond the reminder and the show-as. */
export function editProblem(dialog: EventDialog): string | null {
  const hadTitle = dialog.edit ? dialog.edit.original.title.trim() !== "" : true;
  const problem = eventProblem(dialog.fields, dialog.calendars, hadTitle);
  if (problem) return problem;
  const fields = changedFields(dialog, localZone());
  const keys = Object.keys(fields);
  if (keys.length === 0) return NOTHING_CHANGED;
  if (!dialog.edit?.organizer && keys.some((k) => !ATTENDEE_FIELDS.has(k))) return ORGANIZER_ONLY;
  return null;
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/** The confirmation over the view; opening the one a failure kept, or a fresh one for its event, forgets the kept one. */
export function openEventRemove(s: AppState, dialog: EventRemoveDialog): AppState {
  const kept = s.keptEventRemove;
  const same = kept !== null && kept.account === dialog.account && kept.event === dialog.event;
  return {
    ...s,
    overlay: "event_remove",
    eventRemoveDialog: dialog,
    keptEventRemove: same ? null : kept,
    eventDialog: null,
    dialog: null,
    composeDialog: null,
    attachDialog: null,
    rsvpDialog: null,
    inviteDialog: null,
    signaturesDialog: null,
  };
}

export function eventChangeRequested(s: AppState, run: Omit<EventChangeRun, "operation_id">): AppState {
  return { ...s, eventChanges: [...s.eventChanges, { ...run, operation_id: null }] };
}

export function eventChanging(s: AppState): boolean {
  return s.eventChanges.some((r) => r.operation_id === null);
}

export function isEventChangeOperation(s: AppState, operationId: string): boolean {
  return s.eventChanges.some((r) => r.operation_id === operationId);
}

const VERB = { update: ["Updating", "Updated"], cancel: ["Cancelling", "Cancelled"], delete: ["Deleting", "Deleted"] } as const;

/** What a settled change says: what was done, and for a cancel who was told. */
export function changedText(run: EventChangeRun, result: unknown): string {
  const done = `${VERB[run.kind][1]} ${run.summary}`;
  if (run.kind !== "cancel") return done;
  const told = (result as EventCancelResult | null)?.notified ?? [];
  if (told.length === 0) return done;
  return told.length === 1 ? `${done}; told ${told[0]}` : `${done}; told ${told.length} people`;
}

/** An update's form back with `error`, over the view when nothing else is open; else kept for Edit on the event, which the notice says. */
function reopen(s: AppState, dialog: EventDialog, error: string, account: string, summary: string): AppState {
  const again: EventDialog = { ...dialog, error };
  if (s.overlay === null) return openEventDialog(s, again);
  return pushNotice({ ...s, keptEventDialog: again }, { kind: "failed", account, text: `${error}; Edit on ${summary} opens the form again with what you typed` });
}

/** A cancel's or a delete's confirmation back with `error` and its comment, over the view when nothing else is open; else kept for `D` on the event. */
function reopenRemove(s: AppState, dialog: EventRemoveDialog, error: string, kind: EventChangeRun["kind"]): AppState {
  const again: EventRemoveDialog = { ...dialog, error };
  if (s.overlay === null) return openEventRemove(s, again);
  const what = kind === "cancel" ? "Cancel meeting" : "Delete";
  return pushNotice({ ...s, keptEventRemove: again }, { kind: "failed", account: dialog.account, text: `${error}; ${what} on ${dialog.summary} asks again` });
}

/** A change of this window ended: say how; a failed or vanished one opens its form or confirmation again. */
function eventChangeEnded(s: AppState, run: EventChangeRun, end: OperationEnd): AppState {
  const next = staleCalendar({ ...s, eventChanges: s.eventChanges.filter((r) => r !== run) });
  const what = `${VERB[run.kind][0].toLowerCase()} ${run.summary}`;
  if ("dropped" in end) {
    if (run.dialog) return reopen(next, run.dialog, RESTARTED_CHANGE, run.account, run.summary);
    if (run.removal) return reopenRemove(next, run.removal, RESTARTED_REMOVE, run.kind);
    return pushNotice(next, { kind: "failed", account: run.account, text: `The daemon restarted while ${what}; Refresh shows whether it went through` });
  }
  if (end.state === "cancelled") {
    return pushNotice(next, { kind: "operation_cancelled", account: run.account, text: `Stopped waiting for ${what}; the server may still make the change` });
  }
  if (end.state !== "succeeded") {
    const error = end.error ?? end.state;
    if (run.dialog) return reopen(next, run.dialog, `The change did not go through: ${error}`, run.account, run.summary);
    if (run.removal) return reopenRemove(next, run.removal, `${VERB[run.kind][0]} ${run.summary} failed: ${error}`, run.kind);
    return pushNotice(next, { kind: "failed", account: run.account, text: `${VERB[run.kind][0]} ${run.summary} failed: ${error}` });
  }
  return pushNotice(next, { kind: "applied", account: run.account, text: changedText(run, end.result ?? null) });
}

/** An operation ended: a change this window awaits settles; while one is unanswered an unknown id waits for it. */
export function eventChangeSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.eventChanges.find((r) => r.operation_id === end.operation_id);
  if (run) return eventChangeEnded(s, run, end);
  if (eventChanging(s)) return { ...s, eventChangeEarly: [...s.eventChangeEarly, end].slice(-CHANGE_EARLY_CAP) };
  return s;
}

/** A change's start answered: the dialog closes and the id is awaited, or settled now if its end came first. */
export function eventChangeStarted(s: AppState, token: number, operationId: string): AppState {
  const run = s.eventChanges.find((r) => r.token === token);
  if (!run) return s;
  const started: EventChangeRun = { ...run, operation_id: operationId, dialog: run.dialog ? { ...run.dialog, error: null } : null };
  let next: AppState = { ...s, eventChanges: s.eventChanges.map((r) => (r === run ? started : r)) };
  if (next.overlay === "event" && run.dialog) next = { ...next, overlay: null, eventDialog: null };
  if (next.overlay === "event_remove" && run.removal) next = { ...next, overlay: null, eventRemoveDialog: null };
  const early = next.eventChangeEarly.find((e) => e.operation_id === operationId);
  next = { ...next, eventChangeEarly: eventChanging(next) ? next.eventChangeEarly.filter((e) => e !== early) : [] };
  return early ? eventChangeEnded(next, started, early) : next;
}

/** A change's start was refused: nothing changed, and its dialog, still open, shows why. */
export function eventChangeFailed(s: AppState, token: number): AppState {
  const next: AppState = { ...s, eventChanges: s.eventChanges.filter((r) => r.token !== token) };
  return eventChanging(next) ? next : { ...next, eventChangeEarly: [] };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

let nextChange = 1;

/** `E` in the Calendar view and the card's Edit: the form a failed update of the cursor event kept, else the event form on it, or why it cannot be changed. */
export function editEvent(s: AppState, dispatch: Dispatch<Action>): void {
  const event = cursorEntry(s)?.event;
  if (!event) return dispatch({ type: "notice", text: "No event under the cursor" });
  const kept = s.keptEventDialog;
  if (kept?.edit && kept.edit.account === event.account && kept.edit.event === event.id) {
    return dispatch({ type: "open_event_form", dialog: kept });
  }
  const dialog = editDialogFor(event, s.calendars.data);
  if (typeof dialog === "string") return dispatch({ type: "notice", text: dialog });
  dispatch({ type: "open_event_form", dialog });
}

/** `D` in the Calendar view and the card's Cancel meeting or Delete: the confirmation a failed cancel or delete of the cursor event kept, else a fresh one, or why not. */
export function removeEvent(s: AppState, dispatch: Dispatch<Action>): void {
  const event = cursorEntry(s)?.event;
  if (!event) return dispatch({ type: "notice", text: "No event under the cursor" });
  const kept = s.keptEventRemove;
  if (kept && kept.account === event.account && kept.event === event.id) return dispatch({ type: "open_event_remove", dialog: kept });
  const dialog = removeDialogFor(event, s.calendars.data);
  if (typeof dialog === "string") return dispatch({ type: "notice", text: dialog });
  dispatch({ type: "open_event_remove", dialog });
}

/** Save an edit form: what it lacks is refused here; a refusal of the daemon comes back as its sentence. Null once the update started. */
export async function submitEdit(dispatch: Dispatch<Action>, dialog: EventDialog): Promise<string | null> {
  const problem = editProblem(dialog);
  if (problem) return problem;
  const edit = dialog.edit;
  if (!edit) return "This form makes a new event";
  const token = nextChange++;
  const summary = dialog.fields.title.trim() || "(no title)";
  dispatch({ type: "event_change_requested", token, account: edit.account, summary, kind: "update", dialog, removal: null });
  try {
    const started = await cmd.eventUpdate({ account: edit.account, event: edit.event, fields: changedFields(dialog, localZone()) });
    dispatch({ type: "event_change_started", token, operation_id: started.operation_id });
    return null;
  } catch (e: unknown) {
    dispatch({ type: "event_change_failed", token });
    return asGuiError(e).message;
  }
}

/** Cancel or delete as the confirmation says; a refusal of the daemon comes back as its sentence. Null once it started; a failure later opens the confirmation again with `comment`. */
export async function submitRemove(dispatch: Dispatch<Action>, dialog: EventRemoveDialog, comment: string): Promise<string | null> {
  const token = nextChange++;
  const kind = dialog.cancel ? "cancel" : "delete";
  const removal: EventRemoveDialog = { ...dialog, comment, error: null };
  dispatch({ type: "event_change_requested", token, account: dialog.account, summary: dialog.summary, kind, dialog: null, removal });
  try {
    const started = dialog.cancel
      ? await cmd.eventCancel(dialog.account, dialog.event, comment.trim() || null)
      : await cmd.eventDelete(dialog.account, dialog.event);
    dispatch({ type: "event_change_started", token, operation_id: started.operation_id });
    return null;
  } catch (e: unknown) {
    dispatch({ type: "event_change_failed", token });
    return asGuiError(e).message;
  }
}
