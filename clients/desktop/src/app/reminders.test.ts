import { describe, expect, it } from "vitest";
import { initialState } from "@/app/state";
import { reducer } from "@/app/reducer";
import { reminderDetail, reminderWhen } from "@/app/reminders";
import type { CalendarReminder } from "@/protocol/types";

const reminder = (event: string, start: string, all_day = false): CalendarReminder => ({
  account: "home",
  event,
  summary: `Meeting ${event}`,
  start,
  all_day,
  location: "Room 2.14",
  minutes_before: 15,
  presenter: "gui",
});

const envelope = (kind: string, payload: unknown) => ({
  type: "gui_event" as const,
  event: { type: "event" as const, event: { instance_id: "i", revision: 2, kind, payload } },
});

describe("the reminder line", () => {
  // The tests run in Europe/Berlin (src/test/setup.ts): 08:45 UTC is 09:45 there in November.
  const now = new Date("2026-11-04T08:45:00Z");

  it("says when the event starts, as the CLI and the TUI do", () => {
    expect(reminderWhen(reminder("a", "2026-11-04T09:00:00Z"), now)).toBe("in 15 min (10:00)");
    expect(reminderWhen(reminder("a", "2026-11-04T10:30:00Z"), now)).toBe("in 1 h 45 min (11:30)");
    expect(reminderWhen(reminder("a", "2026-11-04T09:45:00Z"), now)).toBe("in 1 h (10:45)");
    expect(reminderWhen(reminder("a", "2026-11-04T08:45:30Z"), now)).toBe("now (09:45)");
    expect(reminderWhen(reminder("a", "2026-11-04T08:40:00Z"), now)).toBe("started 5 min ago (09:40)");
  });

  it("names an all-day event by its day, and adds the place", () => {
    expect(reminderWhen(reminder("a", "2026-11-03T23:00:00Z", true), now)).toBe("all day today");
    expect(reminderWhen(reminder("a", "2026-11-04T23:00:00Z", true), now)).toBe("all day tomorrow");
    expect(reminderWhen(reminder("a", "2026-11-08T23:00:00Z", true), now)).toBe("all day Mon 9 Nov");
    expect(reminderDetail(reminder("a", "2026-11-04T09:00:00Z"), now)).toBe("in 15 min (10:00), Room 2.14");
  });
});

describe("the reminder state", () => {
  it("adds a fired reminder in start order, replacing an earlier entry of its event", () => {
    let s = reducer(initialState(), { type: "reminders_loaded", gen: 1, reminders: [reminder("b", "2026-11-04T10:00:00Z")] });
    s = reducer(s, envelope("calendar.reminder", reminder("a", "2026-11-04T09:00:00Z")));
    expect(s.reminders.data?.map((r) => r.event)).toEqual(["a", "b"]);
    s = reducer(s, envelope("calendar.reminder", { ...reminder("b", "2026-11-04T10:00:00Z"), summary: "Moved" }));
    expect(s.reminders.data?.map((r) => r.summary)).toEqual(["Meeting a", "Moved"]);
    s = reducer(s, envelope("calendar.reminder", { nonsense: true }));
    expect(s.reminders.data).toHaveLength(2);
  });

  it("reads the list again on calendar.reminders_changed, and takes a decided one off at once", () => {
    let s = reducer(initialState(), { type: "reminders_loaded", gen: 1, reminders: [reminder("a", "2026-11-04T09:00:00Z")] });
    const gen = s.reminders.gen;
    s = reducer(s, envelope("calendar.reminders_changed", { account: "home" }));
    expect(s.reminders.gen).toBe(gen + 1);
    s = reducer(s, { type: "reminder_decided", account: "home", event: "a" });
    expect(s.reminders.data).toEqual([]);
  });

  it("opens the event on its day in the Calendar view, the cursor on it", () => {
    const s = reducer(initialState(), { type: "reminder_open", account: "home", event: "AAMkAD-x", start: "2026-11-04T08:00:00Z" });
    expect(s.view).toBe("calendar");
    expect(s.calendarView?.anchor).toBe("2026-11-04");
    expect(s.calendarView?.cursor).toBe("home\u0000AAMkAD-x\u00002026-11-04");
  });
});
