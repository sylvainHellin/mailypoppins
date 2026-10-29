// The Calendar view (clients/desktop/docs/shell.md, "Calendar", PERSO-200):
// the events of the shown days from `calendar_view`, every account merged,
// and the calendars from `calendar_list`, each a Loadable; the three layouts
// (agenda, day, week), the days each shows, the part of a day an event
// covers, the cursor's entries and the columns of overlapping events on the
// grids. Pure functions over the model, which the reducer routes to, the
// TUI's rules (clients/tui/src/app/calendar.rs), and at the bottom the
// commands the view runs.

import type { Dispatch } from "react";
import type { CalendarEvent, CalendarInfo, CalendarViewResult } from "@/protocol/types";
import { asGuiError, fixtureNotice, type GuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { emptyLoadable, markStale, type AppState, type CalendarLayout, type CalendarView, type Loadable } from "@/app/state";
import { withNotice } from "@/app/activity";

/** A local date, `YYYY-MM-DD`. */
export type Day = string;

/** How many days after its anchor the agenda shows, the TUI's `AGENDA_DAYS`. */
export const AGENDA_DAYS = 14;

/** The jump-to-date field `gt` focuses. */
export const CALENDAR_JUMP_ID = "mp-calendar-jump";

/** The event card Enter focuses. */
export const CALENDAR_CARD_ID = "mp-calendar-card";

/** The prefix of an invitation mail's event id, `IMIP_EVENT_PREFIX`. */
export const IMIP_PREFIX = "imip:";

/** What the daemon's `calendar.view` answered, and the days it was asked for. */
export type CalendarLoad = { days: string; result: CalendarViewResult };

// ---------------------------------------------------------------------------
// Days
// ---------------------------------------------------------------------------

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

/** The local date of an instant. */
export function dayOf(at: Date): Day {
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
}

/** Today, as the view's anchor spells it. */
export function today(now: Date = new Date()): Day {
  return dayOf(now);
}

/** Whether `raw` is a real date written `YYYY-MM-DD`. */
export function parseDay(raw: string): Day | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(raw.trim());
  if (!m) return null;
  const at = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return dayOf(at) === raw.trim() ? raw.trim() : null;
}

/** Local midnight of `day`. */
export function midnight(day: Day): Date {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d);
}

export function addDays(day: Day, n: number): Day {
  const at = midnight(day);
  at.setDate(at.getDate() + n);
  return dayOf(at);
}

/** 0 for Monday to 6 for Sunday. */
export function weekday(day: Day): number {
  return (midnight(day).getDay() + 6) % 7;
}

/** The first and the last day, both included, `layout` shows anchored on `anchor`, the TUI's `CalendarLayout::days`. */
export function layoutDays(layout: CalendarLayout, anchor: Day): { first: Day; last: Day } {
  if (layout === "day") return { first: anchor, last: anchor };
  if (layout === "agenda") return { first: anchor, last: addDays(anchor, AGENDA_DAYS) };
  const monday = addDays(anchor, -weekday(anchor));
  return { first: monday, last: addDays(monday, 6) };
}

/** Every day from `first` to `last`. */
export function daysBetween(first: Day, last: Day): Day[] {
  const out: Day[] = [];
  for (let d = first; d <= last; d = addDays(d, 1)) out.push(d);
  return out;
}

/** How many days the previous and next keys move the anchor: a day on the day grid, a week otherwise. */
export function layoutStep(layout: CalendarLayout): number {
  return layout === "day" ? 1 : 7;
}

/** An instant as RFC 3339 with the local offset, what `calendar.view` takes. */
export function localInstant(at: Date): string {
  const offset = -at.getTimezoneOffset();
  const sign = offset >= 0 ? "+" : "-";
  const abs = Math.abs(offset);
  return `${dayOf(at)}T${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/** The viewer's IANA zone, which places an all-day event's midnights. */
export function viewerZone(): string | null {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || null;
  } catch {
    return null;
  }
}

/** The `calendar.view` range of the view's days: local midnight of the first to local midnight after the last. */
export function viewRange(view: Pick<CalendarView, "layout" | "anchor">): { start: string; end: string; days: string } {
  const { first, last } = layoutDays(view.layout, view.anchor);
  return { start: localInstant(midnight(first)), end: localInstant(midnight(addDays(last, 1))), days: `${first}..${last}` };
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
export const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** `Wed 14 Oct 2099`. */
export function dayLabel(day: Day): string {
  const [y, m, d] = day.split("-").map(Number);
  return `${WEEKDAYS[weekday(day)]} ${d} ${MONTHS[m - 1]} ${y}`;
}

/** What the view's header says it shows. */
export function rangeLabel(layout: CalendarLayout, anchor: Day): string {
  const { first, last } = layoutDays(layout, anchor);
  if (layout === "day") return dayLabel(first);
  return `${dayLabel(first)} to ${dayLabel(last)}`;
}

// ---------------------------------------------------------------------------
// Events on days
// ---------------------------------------------------------------------------

/** The part of one day an event covers, the TUI's `DayPart`. */
export type DayPart =
  | { kind: "all-day" }
  | {
      kind: "timed";
      /** Minutes from the day's midnight, clamped to the day. */
      from: number;
      to: number;
      /** It started on an earlier day. */
      fromBefore: boolean;
      /** It runs on past this day's end. */
      pastMidnight: boolean;
    };

const minutesInto = (at: Date, open: Date) => Math.round((at.getTime() - open.getTime()) / 60_000);

/**
 * What part of `day` `event` covers, or null when it does not fall on that
 * day, the TUI's `day_part`: an all-day event covers its dates up to the
 * exclusive end, a timed one every day its half-open interval touches, and a
 * zero-length one the day of its start.
 */
export function dayPart(event: CalendarEvent, day: Day): DayPart | null {
  const t = event.time;
  if (t.kind === "all-day") {
    const covers = t.end > t.start ? t.start <= day && day < t.end : day === t.start;
    return covers ? { kind: "all-day" } : null;
  }
  const start = new Date(t.start);
  const end = new Date(t.end);
  if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return null;
  const open = midnight(day);
  const close = midnight(addDays(day, 1));
  const span = minutesInto(close, open);
  if (end <= start) {
    if (start < open || start >= close) return null;
    const at = minutesInto(start, open);
    return { kind: "timed", from: at, to: at, fromBefore: false, pastMidnight: false };
  }
  if (start >= close || end <= open) return null;
  return {
    kind: "timed",
    from: Math.max(0, minutesInto(start, open)),
    to: Math.min(span, minutesInto(end, open)),
    fromBefore: start < open,
    pastMidnight: end > close,
  };
}

const clock = (minutes: number) => `${pad(Math.floor(minutes / 60))}:${pad(minutes % 60)}`;

/** The agenda's time column: `all day`, or `HH:MM-HH:MM` with `24:00` where the event runs on past midnight. */
export function partLabel(part: DayPart): string {
  if (part.kind === "all-day") return "all day";
  const until = part.pastMidnight ? "24:00" : clock(part.to);
  return part.from === part.to && !part.pastMidnight ? clock(part.from) : `${clock(part.from)}-${until}`;
}

/** One selectable row of the view: an event on one of the shown days it covers. */
export type CalendarEntry = { day: Day; event: CalendarEvent; part: DayPart; key: string };

/** The cursor's key of an event on a day: an event over several days is an entry under each. */
export function entryKey(event: CalendarEvent, day: Day): string {
  return `${event.account}\u0000${event.id}\u0000${day}`;
}

/**
 * The cursor's entries for `layout` anchored on `anchor`, the TUI's
 * `calendar_entries`: each event under every shown day it covers, day by
 * day, the all-day ones first on their day and the rest in the daemon's order.
 */
export function calendarEntries(layout: CalendarLayout, anchor: Day, events: readonly CalendarEvent[]): CalendarEntry[] {
  const { first, last } = layoutDays(layout, anchor);
  const out: CalendarEntry[] = [];
  for (const day of daysBetween(first, last)) {
    const onDay: CalendarEntry[] = [];
    for (const event of events) {
      const part = dayPart(event, day);
      if (part) onDay.push({ day, event, part, key: entryKey(event, day) });
    }
    out.push(...onDay.filter((e) => e.part.kind === "all-day"), ...onDay.filter((e) => e.part.kind === "timed"));
  }
  return out;
}

/** One timed block of a grid day: where it starts and ends, and its column among those it overlaps. */
export type GridBlock = { entry: CalendarEntry; from: number; to: number; column: number; columns: number };

/** The shortest block a grid draws, so a zero-length event stays clickable. */
export const MIN_BLOCK_MINUTES = 20;

/**
 * The timed entries of one day as grid blocks: a cluster of events that
 * overlap one another shares the width, each in the first column free at its
 * start, side by side.
 */
export function gridBlocks(entries: readonly CalendarEntry[]): GridBlock[] {
  const timed = entries
    .filter((e) => e.part.kind === "timed")
    .map((entry) => {
      const p = entry.part as Extract<DayPart, { kind: "timed" }>;
      return { entry, from: p.from, to: Math.max(p.to, p.from + MIN_BLOCK_MINUTES) };
    })
    .sort((a, b) => a.from - b.from || b.to - a.to);
  const out: GridBlock[] = [];
  let cluster: GridBlock[] = [];
  let ends: number[] = [];
  let clusterEnd = -1;
  const close = () => {
    for (const b of cluster) b.columns = ends.length;
    out.push(...cluster);
    cluster = [];
    ends = [];
  };
  for (const t of timed) {
    if (cluster.length > 0 && t.from >= clusterEnd) close();
    let column = ends.findIndex((end) => end <= t.from);
    if (column === -1) {
      column = ends.length;
      ends.push(t.to);
    } else {
      ends[column] = t.to;
    }
    cluster.push({ ...t, column, columns: 0 });
    clusterEnd = Math.max(cluster.length === 1 ? t.to : clusterEnd, t.to);
  }
  close();
  return out;
}

// ---------------------------------------------------------------------------
// What an event says
// ---------------------------------------------------------------------------

/** An event's title: its summary, else its invitation mail's subject. */
export function eventTitle(e: CalendarEvent): string {
  const summary = e.summary.trim();
  return summary || e.source?.subject || "(no title)";
}

/** Whether the user organizes `e`, so there is nothing to answer. */
export function isOrganizer(e: CalendarEvent): boolean {
  return e.is_organizer || e.own_response === "organizer";
}

export type StatusBadge = "cancelled" | "organizer" | "accepted" | "declined" | "tentative" | "no reply";

/**
 * The badge an entry carries: cancelled first, then organizer, then the
 * user's own answer; an event that invites nobody has none.
 */
export function statusBadge(e: CalendarEvent): StatusBadge | null {
  if (e.cancelled) return "cancelled";
  if (isOrganizer(e)) return "organizer";
  switch (e.own_response) {
    case "accepted":
    case "declined":
    case "tentative":
      return e.own_response;
    case "needs-action":
      return "no reply";
    default:
      return null;
  }
}

/** The calendar `e` sits in, from the listed calendars. */
export function calendarOf(calendars: readonly CalendarInfo[] | null, e: CalendarEvent): CalendarInfo | null {
  return calendars?.find((c) => c.account === e.account && c.id === e.calendar) ?? null;
}

/** The tag that names an entry's account and calendar, `work/Invitations`. */
export function calendarTag(calendars: readonly CalendarInfo[] | null, e: CalendarEvent): string {
  return `${e.account}/${calendarOf(calendars, e)?.name ?? (e.calendar === "imip" ? "Invitations" : e.calendar)}`;
}

/** Whether `e` came from an invitation mail, answered by a reply mail. */
export function isImip(e: Pick<CalendarEvent, "id">): boolean {
  return e.id.startsWith(IMIP_PREFIX);
}

// ---------------------------------------------------------------------------
// The loads
// ---------------------------------------------------------------------------

/** The days the view shows now, the key its load must carry. */
export function wantedDays(s: AppState): string | null {
  return s.calendarView ? viewRange(s.calendarView).days : null;
}

/** Read the view's events and the calendars again, if this window has read them. */
export function staleCalendar(s: AppState): AppState {
  return { ...s, calendar: markStale(s.calendar), calendars: markStale(s.calendars) };
}

export function calendarLoaded(s: AppState, gen: number, load: CalendarLoad): AppState {
  let next: AppState = { ...s, calendar: { ...s.calendar, data: load, loadedGen: gen, error: null } };
  const view = next.calendarView;
  if (view?.refreshing && gen === next.calendar.gen) {
    next = withNotice({ ...next, calendarView: { ...view, refreshing: false } }, `Calendar refreshed (${load.result.events.length} events)`);
  }
  return next;
}

/**
 * A failed read keeps the events it had, so an `r` that fails says so in the
 * notice line: the view shows its error only while it has none.
 */
export function calendarFailed(s: AppState, gen: number, error: GuiError): AppState {
  const view = s.calendarView;
  const prevGen = s.calendar.gen;
  const next: AppState = { ...s, calendar: { ...s.calendar, loadedGen: gen, error } };
  if (!view?.refreshing || gen !== prevGen) return next;
  return withNotice({ ...next, calendarView: { ...view, refreshing: false } }, `Calendar refresh failed: ${error.message}`);
}

export function calendarsLoaded(s: AppState, gen: number, calendars: CalendarInfo[]): AppState {
  return { ...s, calendars: { ...s.calendars, data: calendars, loadedGen: gen, error: null } };
}

export function calendarsFailed(s: AppState, gen: number, error: GuiError): AppState {
  return { ...s, calendars: { ...s.calendars, loadedGen: gen, error } };
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/** Show the Calendar view: on today's agenda the first time, as the TUI opens it. */
export function openCalendar(s: AppState, now: Date = new Date()): AppState {
  if (s.calendarView) return s;
  const view: CalendarView = { layout: "agenda", anchor: today(now), cursor: null, refreshing: false, calendarsOpen: false };
  return { ...s, calendarView: view, calendar: emptyLoadable<CalendarLoad>(), calendars: emptyLoadable<CalendarInfo[]>() };
}

/** The events the view holds: the last answer, whatever days it was for. */
export function viewEvents(s: AppState): CalendarEvent[] {
  return s.calendar.data?.result.events ?? [];
}

/** The entries the view shows now. */
export function calendarRows(s: AppState): CalendarEntry[] {
  const view = s.calendarView;
  if (!view) return [];
  return calendarEntries(view.layout, view.anchor, viewEvents(s));
}

/** The entry under the cursor: the one it names, else the first on or after the anchor, else the last. */
export function cursorEntry(s: AppState, rows: CalendarEntry[] = calendarRows(s)): CalendarEntry | null {
  const view = s.calendarView;
  if (!view || rows.length === 0) return null;
  return rows.find((e) => e.key === view.cursor) ?? rows.find((e) => e.day >= view.anchor) ?? rows[rows.length - 1];
}

/** Move the cursor, as the list's `move_selection` does. */
export function moveCalendarCursor(s: AppState, to: number | "first" | "last", relative: boolean): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const rows = calendarRows(s);
  if (rows.length === 0) return s;
  const current = cursorEntry(s, rows);
  const cur = Math.max(0, rows.findIndex((e) => e.key === current?.key));
  let idx: number;
  if (to === "first") idx = 0;
  else if (to === "last") idx = rows.length - 1;
  else idx = relative ? cur + to : to;
  idx = Math.max(0, Math.min(rows.length - 1, idx));
  return { ...s, calendarView: { ...view, cursor: rows[idx].key }, focusSeq: s.focusSeq + 1 };
}

export function selectCalendarEntry(s: AppState, key: string): AppState {
  const view = s.calendarView;
  return view ? { ...s, calendarView: { ...view, cursor: key } } : s;
}

/** Show the view anchored on `day`, the cursor on its first entry; a new range is read. */
export function calendarGoTo(s: AppState, day: Day): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const next: CalendarView = { ...view, anchor: day, cursor: null };
  const moved = viewRange(next).days !== viewRange(view).days;
  return { ...s, calendarView: next, calendar: moved ? markStale(s.calendar) : s.calendar, focusSeq: s.focusSeq + 1 };
}

/** The previous or the next day or week, the TUI's `h` and `l`. */
export function calendarShift(s: AppState, forward: boolean): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const step = layoutStep(view.layout);
  return calendarGoTo(s, addDays(view.anchor, forward ? step : -step));
}

/**
 * Switch to `layout`, anchored on the day under the cursor so the view keeps
 * showing what the user was looking at, with the TUI's status line.
 */
export function calendarSetLayout(s: AppState, layout: CalendarLayout): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const current = cursorEntry(s);
  const next: CalendarView = { ...view, layout, anchor: current?.day ?? view.anchor, cursor: current?.key ?? null };
  const moved = viewRange(next).days !== viewRange(view).days;
  return withNotice({ ...s, calendarView: next, calendar: moved ? markStale(s.calendar) : s.calendar }, `Calendar: ${layout} layout`);
}

/** `c`: the calendars panel shown or hidden; it reads the calendars again when it opens. */
export function toggleCalendarsPanel(s: AppState): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const open = !view.calendarsOpen;
  return { ...s, calendarView: { ...view, calendarsOpen: open }, calendars: open ? markStale(s.calendars) : s.calendars };
}

/** `r`: read the events and the calendars again, and say how many events came once they land. */
export function refreshCalendar(s: AppState): AppState {
  const view = s.calendarView;
  if (!view) return s;
  return { ...staleCalendar(s), calendarView: { ...view, refreshing: true } };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/** The invitation mail behind an event: its account, row and selector, or why there is none. */
export function eventMail(e: CalendarEvent): { account: string; row_id: number; selector: string; mailbox: string | null } | string {
  if (!e.source) return "This event is the server's; no invitation mail stands behind it";
  const parts = e.source.selector.startsWith("mp://") ? e.source.selector.slice("mp://".length).split("/") : [];
  return { account: e.account, row_id: e.source.row_id, selector: e.source.selector, mailbox: parts.length >= 3 ? parts[1] : null };
}

/**
 * `e`: the cursor event's invitation `invite.ics` in the external editor,
 * the TUI's `Action::OpenEventSource`; a server event says it has none.
 */
export async function openEventSource(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const entry = s.calendarView ? cursorEntry(s) : null;
  if (!entry) {
    dispatch({ type: "notice", text: "The calendar has no event to open" });
    return;
  }
  const mail = eventMail(entry.event);
  if (typeof mail === "string") {
    dispatch({ type: "notice", text: mail });
    return;
  }
  try {
    const launch = await cmd.inviteSourceOpen(mail.account, mail.row_id);
    dispatch({ type: "notice", text: fixtureNotice(launch) ?? `Opened the invite.ics of ${eventTitle(entry.event)} in ${launch.editor}` });
  } catch (e: unknown) {
    const error = asGuiError(e);
    dispatch({ type: "notice", text: error.kind === "not_found" ? error.message : `Open failed: ${error.message}` });
  }
}

/** The event card's "Open mail" (the TUI card's `m`): the invitation mail in the Mail view. */
export function openEventMail(event: CalendarEvent, dispatch: Dispatch<Action>): void {
  const mail = eventMail(event);
  if (typeof mail === "string") return dispatch({ type: "notice", text: mail });
  if (!mail.mailbox) return dispatch({ type: "notice", text: "The invitation's selector names no mailbox" });
  dispatch({ type: "open_event_mail", account: mail.account, mailbox: mail.mailbox, row_id: mail.row_id, selector: mail.selector });
}

/** The cursor event's invitation mail, from a key or the palette. */
export function openCursorEventMail(s: AppState, dispatch: Dispatch<Action>): void {
  const entry = s.calendarView ? cursorEntry(s) : null;
  if (!entry) return dispatch({ type: "notice", text: "The calendar has no event to open" });
  openEventMail(entry.event, dispatch);
}

/** Show or hide a calendar; the daemon's `calendar.changed` reads the view again. */
export async function setCalendarVisible(dispatch: Dispatch<Action>, calendar: CalendarInfo, visible: boolean): Promise<void> {
  try {
    await cmd.calendarSetVisibility(calendar.account, calendar.id, visible);
    dispatch({ type: "notice", text: `${visible ? "Showing" : "Hiding"} ${calendar.account}/${calendar.name}` });
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `Cannot change ${calendar.name}: ${asGuiError(e).message}` });
  }
}

/** Whether the view's loads are for `days`; another answer may still show while its own is read. */
export function loadIsFor(l: Loadable<CalendarLoad>, days: string | null): boolean {
  return days !== null && l.data?.days === days;
}
