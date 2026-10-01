// The Calendar view (clients/desktop/docs/shell.md, "Calendar"): each
// account's `calendar_events` as a Loadable, the TUI's past/upcoming rule,
// the view's cursor, and what makes an agenda stale. Pure functions over the
// model, which the reducer routes to, and at the bottom the one command the
// view runs, the source of an entry in the editor.

import type { Dispatch } from "react";
import type { AgendaEvent } from "@/protocol/types";
import { asGuiError, fixtureNotice, type GuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import type { Action } from "@/app/reducer";
import { emptyLoadable, markStale, type AppState, type CalendarView, type Loadable } from "@/app/state";
import { withNotice } from "@/app/activity";

/**
 * Now as the agenda's sort keys spell it, UTC `YYYY-MM-DDTHH:MM:SS`
 * (`mp_core::calendar::now_sort_key`).
 */
export function nowSortKey(now: Date = new Date()): string {
  return now.toISOString().slice(0, 19);
}

/**
 * The rows the agenda shows, the TUI's `recompute_calendar_visible`
 * (clients/tui/src/app/mod.rs): every row when `showPast`, else an undated
 * row always, and a dated one while its end (its start when the end is
 * unknown) is not yet past. A running event stays until it ends.
 */
export function visibleEvents(events: readonly AgendaEvent[], showPast: boolean, now: string = nowSortKey()): AgendaEvent[] {
  if (showPast) return [...events];
  return events.filter((e) => {
    if (e.start_sort === "") return true;
    const horizon = e.end_sort === "" ? e.start_sort : e.end_sort;
    return horizon >= now;
  });
}

/** An agenda row's title: the event's summary, else the email's subject. */
export function eventTitle(e: AgendaEvent): string {
  const summary = e.event.summary?.trim();
  return summary ? summary : e.subject || "(no subject)";
}

/**
 * The badge an agenda row carries, the TUI's `status_badge`: cancelled
 * first, then organizer, then the user's own reply.
 */
export function statusBadge(e: AgendaEvent): "cancelled" | "organizer" | "accepted" | "declined" | "tentative" | "no reply" {
  if (e.cancelled) return "cancelled";
  if (e.is_organizer) return "organizer";
  switch (e.event.rsvp) {
    case "accepted":
    case "declined":
    case "tentative":
      return e.event.rsvp;
    default:
      return "no reply";
  }
}

/** The status line `t` writes, the TUI's words. */
export function scopeNotice(showPast: boolean): string {
  return `Calendar: showing ${showPast ? "all events" : "upcoming events"}`;
}

// ---------------------------------------------------------------------------
// The agendas
// ---------------------------------------------------------------------------

function loadableOf(s: AppState, account: string): Loadable<AgendaEvent[]> {
  return s.calendar[account] ?? emptyLoadable<AgendaEvent[]>();
}

/** Read `account`'s agenda again, if this window has read it. */
export function staleCalendar(s: AppState, account: string): AppState {
  const l = s.calendar[account];
  return l ? { ...s, calendar: { ...s.calendar, [account]: markStale(l) } } : s;
}

/**
 * A bootstrap reads every agenda this window has read again, and forgets
 * those of accounts the snapshot no longer has; the view closes with them.
 */
export function staleAllCalendars(s: AppState): AppState {
  const names = new Set(s.bootstrap?.snapshot.accounts.map((a) => a.name) ?? []);
  const calendar: AppState["calendar"] = {};
  for (const [account, l] of Object.entries(s.calendar)) if (names.has(account)) calendar[account] = markStale(l);
  const view = s.calendarView && names.has(s.calendarView.account) ? s.calendarView : null;
  return { ...s, calendar, calendarView: view };
}

/** Forget a removed account's agenda. */
export function dropCalendar(s: AppState, account: string): AppState {
  if (!(account in s.calendar) && s.calendarView?.account !== account) return s;
  const calendar = { ...s.calendar };
  delete calendar[account];
  return { ...s, calendar, calendarView: s.calendarView?.account === account ? null : s.calendarView };
}

export function calendarLoaded(s: AppState, account: string, gen: number, events: AgendaEvent[]): AppState {
  const l = loadableOf(s, account);
  let next: AppState = { ...s, calendar: { ...s.calendar, [account]: { ...l, data: events, loadedGen: gen, error: null } } };
  const view = next.calendarView;
  if (view?.refreshing && view.account === account && gen === next.calendar[account].gen) {
    const count = visibleEvents(events, view.showPast).length;
    next = withNotice({ ...next, calendarView: { ...view, refreshing: false } }, `Calendar refreshed (${count} events)`);
  }
  return next;
}

/**
 * A failed read keeps the rows it had, so an `r` that fails says so in the
 * notice line: the view shows its error only while it has no rows.
 */
export function calendarFailed(s: AppState, account: string, gen: number, error: GuiError): AppState {
  const l = loadableOf(s, account);
  const view = s.calendarView;
  const next: AppState = { ...s, calendar: { ...s.calendar, [account]: { ...l, loadedGen: gen, error } } };
  if (!view?.refreshing || view.account !== account || gen !== l.gen) return next;
  return withNotice({ ...next, calendarView: { ...view, refreshing: false } }, `Calendar refresh failed: ${error.message}`);
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/**
 * Show `account`'s agenda: its Loadable is created on the first open, and
 * the view keeps its scope; the cursor stays on the same account only.
 */
export function openCalendar(s: AppState, account: string): AppState {
  const next = s.calendar[account] ? s : { ...s, calendar: { ...s.calendar, [account]: emptyLoadable<AgendaEvent[]>() } };
  const prev = s.calendarView;
  if (prev?.account === account) return next;
  const view: CalendarView = { account, cursor: null, showPast: prev?.showPast ?? false, refreshing: false };
  return { ...next, calendarView: view };
}

/** While the Calendar view shows, it follows the selection's account. */
export function followCalendar(s: AppState): AppState {
  if (s.view !== "calendar") return s;
  const account = s.selection.account;
  if (!account || (s.calendarView?.account === account && s.calendar[account])) return s;
  return openCalendar(s, account);
}

/** The rows the view shows now. */
export function calendarRows(s: AppState): AgendaEvent[] {
  const view = s.calendarView;
  if (!view) return [];
  return visibleEvents(s.calendar[view.account]?.data ?? [], view.showPast);
}

/** The row under the view's cursor: the one it names, else the first. */
export function cursorEvent(s: AppState, rows: AgendaEvent[] = calendarRows(s)): AgendaEvent | null {
  const cursor = s.calendarView?.cursor ?? null;
  return rows.find((e) => e.row_id === cursor) ?? rows[0] ?? null;
}

/** Move the view's cursor, as the list's `move_selection` does. */
export function moveCalendarCursor(s: AppState, to: number | "first" | "last", relative: boolean): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const rows = calendarRows(s);
  if (rows.length === 0) return s;
  const cur = Math.max(0, rows.findIndex((e) => e.row_id === cursorEvent(s, rows)?.row_id));
  let idx: number;
  if (to === "first") idx = 0;
  else if (to === "last") idx = rows.length - 1;
  else idx = relative ? cur + to : to;
  idx = Math.max(0, Math.min(rows.length - 1, idx));
  return { ...s, calendarView: { ...view, cursor: rows[idx].row_id }, focusSeq: s.focusSeq + 1 };
}

export function selectCalendarRow(s: AppState, rowId: number): AppState {
  const view = s.calendarView;
  return view ? { ...s, calendarView: { ...view, cursor: rowId } } : s;
}

/** `t`: past events shown or hidden, the cursor back to the top, and the TUI's status line. */
export function toggleCalendarPast(s: AppState): AppState {
  const view = s.calendarView;
  if (!view) return s;
  const showPast = !view.showPast;
  return withNotice({ ...s, calendarView: { ...view, showPast, cursor: null } }, scopeNotice(showPast));
}

/**
 * Enter and `e`: the cursor row's `invite.ics` in the external editor, the
 * TUI's `Action::OpenEventSource`. A row with none says so in the TUI's words.
 */
export async function openEventSource(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const view = s.calendarView;
  const event = view ? cursorEvent(s) : null;
  if (!view || !event) {
    dispatch({ type: "notice", text: "The agenda has no event to open" });
    return;
  }
  try {
    const launch = await cmd.inviteSourceOpen(view.account, event.row_id);
    dispatch({ type: "notice", text: fixtureNotice(launch) ?? `Opened the invite.ics of ${eventTitle(event)} in ${launch.editor}` });
  } catch (e: unknown) {
    const error = asGuiError(e);
    dispatch({ type: "notice", text: error.kind === "not_found" ? error.message : `Open failed: ${error.message}` });
  }
}

/** `r`: read the agenda again, and say how many events it holds once it lands. */
export function refreshCalendar(s: AppState): AppState {
  const view = s.calendarView;
  if (!view) return s;
  return {
    ...s,
    calendar: { ...s.calendar, [view.account]: markStale(loadableOf(s, view.account)) },
    calendarView: { ...view, refreshing: true },
  };
}
