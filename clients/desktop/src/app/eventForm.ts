// A new event (CAL-11, PERSO-255; clients/desktop/docs/shell.md, "Calendar",
// "New event"): the event form's fields and checks, the creates this window
// awaits as `event_create`, and what their ends say. The form goes to the
// daemon's `event.create` as typed, the times read in this window's zone; the
// daemon checks every field again, and its refusal lands on the form with
// what was typed. A create that failed or vanished in a daemon restart opens
// the form again with the transaction id it was sent with, so Retry sends the
// same one and the server makes the event at most once. Pure functions over
// the model, which the reducer routes to, and at the bottom the commands.

import type { Dispatch } from "react";
import type { CalendarInfo, EventCreateParams, EventWriteResult } from "@/protocol/types";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import { cursorEntry, staleCalendar, today } from "@/app/calendar";
import type { AppState, EventCalendar, EventCreateRun, EventDialog, EventFields, OperationEnd } from "@/app/state";

const EVENT_EARLY_CAP = 16;

export const NO_TITLE = "The event needs a title";
export const NO_CALENDAR = "No calendar takes new events: no account has a server calendar mp writes to";
export const RESTARTED = "The daemon restarted before the event was confirmed; Retry sends the same transaction id, which creates it at most once";

/** The calendars `calendar_list` named that take a new event: writable, on a backend that writes; each account's default first. */
export function writableCalendars(calendars: CalendarInfo[] | null | undefined): EventCalendar[] {
  const order = (c: CalendarInfo) => (calendars ?? []).findIndex((x) => x.account === c.account);
  return (calendars ?? [])
    .filter((c) => c.can_edit && c.features.includes("write"))
    .sort((a, b) => order(a) - order(b) || Number(b.is_default) - Number(a.is_default))
    .map((c) => ({
      key: `${c.account}/${c.id}`,
      account: c.account,
      id: c.id,
      label: `${c.account}/${c.name}`,
      teams: c.features.includes("online-meeting"),
      freeBusy: c.features.includes("free-busy"),
    }));
}

/** This window's IANA zone, which reads the form's times. */
export function localZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

/** A fresh form on `day` from 09:00 to 10:00, in `account`'s default calendar when it has one. */
export function freshFields(calendars: EventCalendar[], account: string | null, day: string): EventFields {
  const calendar = calendars.find((c) => c.account === account) ?? calendars[0];
  return {
    calendar: calendar?.key ?? "",
    title: "",
    allDay: false,
    start: `${day}T09:00`,
    end: `${day}T10:00`,
    location: "",
    to: "",
    optional: "",
    rooms: "",
    teams: false,
    reminder: "",
    noReminder: false,
    body: "",
  };
}

const DAY = /^\d{4}-\d{2}-\d{2}$/;
const TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/;

/** The addresses of a comma or semicolon separated list. */
export function addresses(list: string): string[] {
  return list
    .split(/[,;]/)
    .map((a) => a.trim())
    .filter(Boolean);
}

/** What the fields lack or get wrong, the first thing only; null when the form can be sent. The daemon checks it all again. */
export function eventProblem(fields: EventFields, calendars: EventCalendar[]): string | null {
  if (!calendars.some((c) => c.key === fields.calendar)) return NO_CALENDAR;
  if (fields.title.trim() === "") return NO_TITLE;
  if (fields.allDay) {
    if (!DAY.test(fields.start)) return "The first day is YYYY-MM-DD";
    if (fields.end && !DAY.test(fields.end)) return "The last day is YYYY-MM-DD, or empty for one day";
    if (fields.end && fields.end < fields.start) return "The last day comes before the first";
  } else {
    if (!TIME.test(fields.start)) return "The start is a date and a time";
    if (!TIME.test(fields.end)) return "The end is a date and a time";
    if (fields.end <= fields.start) return "The end comes before the start";
  }
  for (const [what, list] of [
    ["attendee", fields.to],
    ["optional attendee", fields.optional],
    ["room", fields.rooms],
  ] as const) {
    const bad = addresses(list).find((a) => !a.includes("@"));
    if (bad) return `The ${what} "${bad}" is not an email address`;
  }
  if (!fields.noReminder && fields.reminder.trim() && !/^\d+$/.test(fields.reminder.trim())) {
    return "The reminder is a number of minutes, or empty for the mailbox default";
  }
  return null;
}

/** `event_create`'s params for the form, its times in `zone`. */
export function eventParams(dialog: EventDialog, zone: string): EventCreateParams {
  const f = dialog.fields;
  const calendar = dialog.calendars.find((c) => c.key === f.calendar);
  const text = (v: string) => (v.trim() ? v.trim() : undefined);
  const attendees = [
    ...addresses(f.to).map((email) => ({ email, role: "required" })),
    ...addresses(f.optional).map((email) => ({ email, role: "optional" })),
    ...addresses(f.rooms).map((email) => ({ email, role: "resource" })),
  ];
  const reminder = f.noReminder ? null : f.reminder.trim() ? Number(f.reminder.trim()) : undefined;
  const event: EventCreateParams["event"] = {
    summary: f.title.trim(),
    start: f.start,
    zone,
    all_day: f.allDay,
    attendees,
  };
  const end = f.allDay ? text(f.end) : f.end;
  if (end) event.end = end;
  const location = text(f.location);
  if (location) event.location = location;
  if (f.body.trim()) event.body = f.body;
  if (f.teams && calendar?.teams) event.online_meeting = true;
  if (reminder !== undefined) event.reminder = reminder;
  const params: EventCreateParams = { account: calendar?.account ?? "", calendar: calendar?.id, event };
  if (dialog.transactionId) params.transaction_id = dialog.transactionId;
  return params;
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

export function openEventDialog(s: AppState, dialog: EventDialog): AppState {
  return {
    ...s,
    overlay: "event",
    eventDialog: dialog,
    keptEventDialog: null,
    dialog: null,
    composeDialog: null,
    attachDialog: null,
    rsvpDialog: null,
    inviteDialog: null,
    signaturesDialog: null,
  };
}

export function eventCreateRequested(s: AppState, run: Omit<EventCreateRun, "operation_id">): AppState {
  return { ...s, eventCreates: [...s.eventCreates, { ...run, operation_id: null }] };
}

export function eventCreating(s: AppState): boolean {
  return s.eventCreates.some((r) => r.operation_id === null);
}

export function isEventCreateOperation(s: AppState, operationId: string): boolean {
  return s.eventCreates.some((r) => r.operation_id === operationId);
}

/** The form back with `error`: over the view when nothing else is open, else kept for New event. */
function reopen(s: AppState, dialog: EventDialog, error: string, account: string): AppState {
  const again: EventDialog = { ...dialog, error };
  if (s.overlay === null) return openEventDialog(s, again);
  return pushNotice({ ...s, keptEventDialog: again }, { kind: "failed", account, text: `${error}; New event opens the form again` });
}

/** What a settled create says: the event and who the server invited. */
export function createdText(result: EventWriteResult | null, summary: string): string {
  const name = result?.event.summary || summary;
  const invited = result?.invited ?? [];
  if (invited.length === 0) return `Created ${name}`;
  if (invited.length === 1) return `Created ${name}; invited ${invited[0]}`;
  return `Created ${name}; invited ${invited.length} people`;
}

/** A create of this window ended: say how; a failure or a vanished one opens the form again. */
function eventCreateEnded(s: AppState, run: EventCreateRun, end: OperationEnd): AppState {
  const next = staleCalendar({ ...s, eventCreates: s.eventCreates.filter((r) => r !== run) });
  if ("dropped" in end) return reopen(next, run.dialog, RESTARTED, run.account);
  if (end.state === "cancelled") {
    return pushNotice(next, {
      kind: "operation_cancelled",
      account: run.account,
      text: `Stopped waiting for ${run.summary}; the server may still create it, and Retry would not make a second one`,
    });
  }
  if (end.state !== "succeeded") return reopen(next, run.dialog, `The event was not created: ${end.error ?? end.state}`, run.account);
  const result = (end.result ?? null) as EventWriteResult | null;
  return pushNotice(next, { kind: "applied", account: run.account, text: createdText(result, run.summary) });
}

/** An operation ended: a create this window awaits settles; while one is unanswered an unknown id waits for it. */
export function eventCreateSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.eventCreates.find((r) => r.operation_id === end.operation_id);
  if (run) return eventCreateEnded(s, run, end);
  if (eventCreating(s)) return { ...s, eventCreateEarly: [...s.eventCreateEarly, end].slice(-EVENT_EARLY_CAP) };
  return s;
}

/** `event_create` answered: the form closes with the transaction id kept on the run, and the id is awaited, or settled now if its end came first. */
export function eventCreateStarted(s: AppState, token: number, operationId: string, transactionId: string): AppState {
  const run = s.eventCreates.find((r) => r.token === token);
  if (!run) return s;
  const started: EventCreateRun = { ...run, operation_id: operationId, dialog: { ...run.dialog, transactionId, error: null } };
  let next: AppState = { ...s, eventCreates: s.eventCreates.map((r) => (r === run ? started : r)) };
  if (next.overlay === "event") next = { ...next, overlay: null, eventDialog: null };
  const early = next.eventCreateEarly.find((e) => e.operation_id === operationId);
  next = { ...next, eventCreateEarly: eventCreating(next) ? next.eventCreateEarly.filter((e) => e !== early) : [] };
  return early ? eventCreateEnded(next, started, early) : next;
}

/** `event_create` was refused: nothing was created, and the form, still open, shows why. */
export function eventCreateFailed(s: AppState, token: number): AppState {
  const next: AppState = { ...s, eventCreates: s.eventCreates.filter((r) => r.token !== token) };
  return eventCreating(next) ? next : { ...next, eventCreateEarly: [] };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

let nextCreate = 1;

/**
 * "New event", `n` in the Calendar view and the palette: the form a failed
 * create kept, else a fresh one on the cursor's day (today outside the
 * Calendar view), in the selected account's default calendar.
 */
export async function newEvent(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  if (s.keptEventDialog) {
    dispatch({ type: "open_event_form", dialog: s.keptEventDialog });
    return;
  }
  // Outside the Calendar view the list may not be read yet.
  let listed = s.calendars.data;
  if (!listed) {
    try {
      listed = await cmd.calendarList();
    } catch (e: unknown) {
      dispatch({ type: "notice", text: `The calendars did not load: ${asGuiError(e).message}` });
      return;
    }
  }
  const calendars = writableCalendars(listed);
  if (calendars.length === 0) {
    dispatch({ type: "notice", text: NO_CALENDAR });
    return;
  }
  const day = (s.view === "calendar" && s.calendarView ? cursorEntry(s)?.day : null) ?? s.calendarView?.anchor ?? today();
  dispatch({
    type: "open_event_form",
    dialog: { fields: freshFields(calendars, s.selection.account, day), calendars, transactionId: null, error: null },
  });
}

/**
 * Send the form: what it lacks is refused here, without a call; a refusal
 * of the daemon comes back as its sentence, for the form to show. Null once
 * the create started, and the form closes.
 */
export async function submitEvent(dispatch: Dispatch<Action>, dialog: EventDialog): Promise<string | null> {
  const problem = eventProblem(dialog.fields, dialog.calendars);
  if (problem) return problem;
  const params = eventParams(dialog, localZone());
  const token = nextCreate++;
  dispatch({ type: "event_create_requested", token, account: params.account, summary: params.event.summary, dialog });
  try {
    const started = await cmd.eventCreate(params);
    dispatch({ type: "event_create_started", token, operation_id: started.operation_id, transaction_id: started.transaction_id });
    return null;
  } catch (e: unknown) {
    dispatch({ type: "event_create_failed", token });
    return asGuiError(e).message;
  }
}
