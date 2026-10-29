import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import {
  calendarEntries,
  calendarRows,
  cursorEntry,
  dayPart,
  gridBlocks,
  layoutDays,
  localInstant,
  midnight,
  parseDay,
  partLabel,
  statusBadge,
  viewRange,
  type CalendarLoad,
} from "@/app/calendar";
import { initialState, isStale, type AppState } from "@/app/state";
import { hiddenNotice, OPEN_CALENDAR_FIRST } from "@/app/views";
import { fixtures } from "@/test/tauri-mock";
import type { CalendarEvent } from "@/protocol/types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);
const events = fixtures.calendarServer.events;
const byId = (id: string) => events.find((e) => e.id === id)!;

function timed(id: string, start: string, end: string, patch: Partial<CalendarEvent> = {}): CalendarEvent {
  return { ...byId("AAMkAD-design"), id, summary: id, time: { kind: "timed", start, end, zone: "Europe/Berlin" }, ...patch };
}

function booted(): AppState {
  return run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

const load = (days: string): CalendarLoad => ({
  days,
  result: { range: { start: "", end: "" }, zone: "Europe/Berlin", window: { start: "", end: "" }, events },
});

/** The Calendar view on the agenda from 2099-10-14, the fixture's events read. */
function inCalendar(): AppState {
  let s = run(booted(), { type: "switch_view", view: "calendar" }, { type: "calendar_go_to", day: "2099-10-14" });
  s = run(s, { type: "calendar_loaded", gen: s.calendar.gen, load: load(viewRange(s.calendarView!).days) });
  return s;
}

const envelope = (kind: string, payload: unknown): Action => ({
  type: "gui_event",
  event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision: 99, kind, payload } },
});

describe("days and layouts", () => {
  it("covers the TUI's days: the agenda fifteen from its anchor, the day its own, the week Monday to Sunday", () => {
    expect(layoutDays("agenda", "2099-10-14")).toEqual({ first: "2099-10-14", last: "2099-10-28" });
    expect(layoutDays("day", "2099-10-14")).toEqual({ first: "2099-10-14", last: "2099-10-14" });
    expect(layoutDays("week", "2099-10-14")).toEqual({ first: "2099-10-12", last: "2099-10-18" });
    expect(layoutDays("week", "2099-10-18")).toEqual({ first: "2099-10-12", last: "2099-10-18" });
  });

  it("asks calendar.view from local midnight to local midnight, across a change of offset", () => {
    expect(localInstant(midnight("2099-10-14"))).toBe("2099-10-14T00:00:00+02:00");
    expect(viewRange({ layout: "week", anchor: "2099-10-28" })).toEqual({
      start: "2099-10-26T00:00:00+01:00",
      end: "2099-11-02T00:00:00+01:00",
      days: "2099-10-26..2099-11-01",
    });
  });

  it("reads a typed date only when it is a real one", () => {
    expect(parseDay(" 2099-10-14 ")).toBe("2099-10-14");
    expect(parseDay("2099-02-30")).toBeNull();
    expect(parseDay("14.10.2099")).toBeNull();
  });
});

describe("events on days, the TUI's day_part", () => {
  it("clamps a timed event to the day in the local zone and marks what runs over midnight", () => {
    const late = timed("late", "2099-10-14T20:00:00Z", "2099-10-14T23:30:00Z");
    expect(dayPart(late, "2099-10-14")).toEqual({ kind: "timed", from: 22 * 60, to: 24 * 60, fromBefore: false, pastMidnight: true });
    expect(partLabel(dayPart(late, "2099-10-14")!)).toBe("22:00-24:00");
    expect(dayPart(late, "2099-10-15")).toEqual({ kind: "timed", from: 0, to: 90, fromBefore: true, pastMidnight: false });
    expect(dayPart(late, "2099-10-16")).toBeNull();
    const instant = timed("instant", "2099-10-14T08:00:00Z", "2099-10-14T08:00:00Z");
    expect(partLabel(dayPart(instant, "2099-10-14")!)).toBe("10:00");
  });

  it("covers an all-day event's dates up to its exclusive end", () => {
    const holiday = byId("AAMkAD-holiday");
    expect(dayPart(holiday, "2099-10-16")).toEqual({ kind: "all-day" });
    expect(dayPart(holiday, "2099-10-17")).toBeNull();
    expect(partLabel({ kind: "all-day" })).toBe("all day");
  });

  it("lists each event under every shown day it covers, all-day first, the daemon's order kept", () => {
    const entries = calendarEntries("week", "2099-10-14", events);
    expect(entries.map((e) => `${e.day} ${e.event.id}`)).toEqual([
      "2099-10-13 AAMkAD-weekly-20991013",
      "2099-10-14 AAMkAD-design",
      "2099-10-14 imip:1008",
      "2099-10-14 AAMkAD-robin",
      "2099-10-16 AAMkAD-holiday",
    ]);
    const two = timed("two", "2099-10-15T20:00:00Z", "2099-10-16T08:00:00Z");
    const withTwo = calendarEntries("day", "2099-10-16", [byId("AAMkAD-holiday"), two]).map((e) => e.event.id);
    expect(withTwo).toEqual(["AAMkAD-holiday", "two"]);
  });

  it("puts overlapping events side by side and a free one alone", () => {
    const entries = calendarEntries("day", "2099-10-14", [
      ...events,
      timed("evening", "2099-10-14T16:00:00Z", "2099-10-14T17:00:00Z"),
      timed("after", "2099-10-14T09:00:00Z", "2099-10-14T09:30:00Z"),
    ]);
    const blocks = gridBlocks(entries).map((b) => `${b.entry.event.id} ${b.column + 1}/${b.columns}`);
    expect(blocks).toEqual(["AAMkAD-design 1/3", "imip:1008 2/3", "AAMkAD-robin 3/3", "after 1/1", "evening 1/1"]);
  });

  it("badges cancelled first, then organizer, then the user's answer, and an event that invites nobody none", () => {
    expect(statusBadge({ ...byId("AAMkAD-robin"), cancelled: true })).toBe("cancelled");
    expect(statusBadge(byId("AAMkAD-robin"))).toBe("organizer");
    expect(statusBadge(byId("AAMkAD-weekly-20991020"))).toBe("accepted");
    expect(statusBadge(byId("AAMkAD-design"))).toBe("no reply");
    expect(statusBadge(byId("AAMkAD-holiday"))).toBeNull();
  });
});

describe("the Calendar view's model", () => {
  it("opens on today's agenda once, and keeps its layout and days while another view shows", () => {
    let s = run(booted(), { type: "switch_view", view: "calendar" });
    expect(s.calendarView).toMatchObject({ layout: "agenda", cursor: null });
    s = run(s, { type: "calendar_go_to", day: "2099-10-14" }, { type: "calendar_layout", layout: "week" });
    s = run(s, { type: "switch_view", view: "mail" }, { type: "switch_view", view: "calendar" });
    expect(s.calendarView).toMatchObject({ layout: "week", anchor: "2099-10-14" });
  });

  it("puts the cursor on the first entry and moves it with move_selection, the mail selection left alone", () => {
    let s = inCalendar();
    const selection = s.selection;
    expect(calendarRows(s)).toHaveLength(5);
    expect(cursorEntry(s)?.event.id).toBe("AAMkAD-design");
    s = run(s, { type: "move_selection", to: 2, relative: true });
    expect(cursorEntry(s)?.event.id).toBe("AAMkAD-robin");
    s = run(s, { type: "move_selection", to: 99, relative: true });
    expect(cursorEntry(s)?.event.id).toBe("AAMkAD-weekly-20991020");
    s = run(s, { type: "move_selection", to: "first", relative: false });
    expect(cursorEntry(s)?.event.id).toBe("AAMkAD-design");
    expect(s.selection).toBe(selection);
  });

  it("a move to other days reads them, a layout over the same days does not", () => {
    let s = inCalendar();
    expect(isStale(s.calendar)).toBe(false);
    s = run(s, { type: "calendar_shift", forward: true });
    expect(s.calendarView?.anchor).toBe("2099-10-21");
    expect(isStale(s.calendar)).toBe(true);
    s = run(s, { type: "calendar_loaded", gen: s.calendar.gen, load: load(viewRange(s.calendarView!).days) });
    s = run(s, { type: "calendar_layout", layout: "day" }, { type: "calendar_shift", forward: false });
    expect(s.calendarView).toMatchObject({ layout: "day", anchor: "2099-10-20" });
    expect(s.notice).toBe("Calendar: day layout");
  });

  it("the layout follows the day under the cursor", () => {
    let s = inCalendar();
    s = run(s, { type: "move_selection", to: "last", relative: false }, { type: "calendar_layout", layout: "day" });
    expect(s.calendarView).toMatchObject({ layout: "day", anchor: "2099-10-20" });
    expect(cursorEntry(s)?.event.id).toBe("AAMkAD-weekly-20991020");
  });

  it("goes stale on calendar.changed, on any account's mail, and on a bootstrap", () => {
    let s = inCalendar();
    s = run(s, envelope("calendar.changed", { account: "home" }));
    expect(isStale(s.calendar)).toBe(true);
    s = run(s, { type: "calendar_loaded", gen: s.calendar.gen, load: load("x") });
    s = run(s, envelope("state.invalidate", { resource: "mailbox:work/inbox", scope: { query: "counts" } }));
    expect(isStale(s.calendar)).toBe(true);
    s = run(s, { type: "calendar_loaded", gen: s.calendar.gen, load: load("x") });
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "reconnected", bootstrap: fixtures.bootstrap } });
    expect(isStale(s.calendar)).toBe(true);
  });

  it("r reads again and says how many events came, and a failed read keeps the events and says why", () => {
    let s = inCalendar();
    s = run(s, { type: "calendar_refresh" });
    expect(isStale(s.calendar)).toBe(true);
    expect(isStale(s.calendars)).toBe(true);
    s = run(s, { type: "calendar_loaded", gen: s.calendar.gen, load: load("x") });
    expect(s.notice).toBe(`Calendar refreshed (${events.length} events)`);
    s = run(s, { type: "calendar_refresh" });
    s = run(s, { type: "calendar_failed", gen: s.calendar.gen, error: { kind: "protocol", message: "boom", code: null } });
    expect(s.notice).toBe("Calendar refresh failed: boom");
    expect(s.calendar.data?.result.events).toHaveLength(events.length);
  });

  it("c opens the calendars panel and reads them again", () => {
    let s = inCalendar();
    s = run(s, { type: "calendars_loaded", gen: s.calendars.gen, calendars: fixtures.calendarServer.calendars });
    s = run(s, { type: "calendar_toggle_calendars" });
    expect(s.calendarView?.calendarsOpen).toBe(true);
    expect(isStale(s.calendars)).toBe(true);
    s = run(s, { type: "calendar_toggle_calendars" });
    expect(s.calendarView?.calendarsOpen).toBe(false);
  });

  it("answers its own actions outside the Calendar view with a notice", () => {
    const s = booted();
    for (const id of ["calendar_today", "calendar_layout_week", "calendar_calendars", "calendar_rsvp"] as const) {
      expect(hiddenNotice(s, id)).toBe(OPEN_CALENDAR_FIRST);
      expect(hiddenNotice(inCalendar(), id)).toBeNull();
    }
  });
});
