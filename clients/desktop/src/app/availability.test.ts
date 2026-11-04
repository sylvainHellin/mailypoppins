import { describe, expect, it } from "vitest";
import { availabilityRows, chosenSlots, freeBusyParams, slotStatus } from "@/app/availability";
import type { CalendarFreeBusyResult } from "@/protocol/types";
import type { EventFields } from "@/app/state";

const fields = (over: Partial<EventFields> = {}): EventFields => ({
  calendar: "home/AAMk-cal-home",
  title: "Kick-off",
  allDay: false,
  start: "2099-10-14T14:00",
  end: "2099-10-14T15:30",
  location: "",
  to: "robin@example.com, kim@exa",
  optional: "Robin@Example.com; kim@example.com",
  rooms: "room-1@example.com",
  teams: false,
  reminder: "",
  noReminder: false,
  showAs: "busy",
  body: "",
  ...over,
});

/** A Berlin day (the tests run in Europe/Berlin) at 30-minute slots, free but for `marks`. */
function result(marks: Record<number, string>, error?: string): CalendarFreeBusyResult {
  const view = Array.from({ length: 48 }, (_, n) => marks[n] ?? "0").join("");
  return {
    account: "home",
    start: "2099-10-13T22:00:00Z",
    end: "2099-10-14T22:00:00Z",
    interval_minutes: 30,
    schedules: [
      { address: "me@example.com", view, items: [] },
      { address: "someone@gmail.com", view: "", items: [], error: error ?? "Proxy web request failed (5016)" },
    ],
  };
}

describe("the availability grid's model", () => {
  it("asks about the day of the start and every whole address once, a half-typed one waiting", () => {
    expect(freeBusyParams(fields(), "home", "Europe/Berlin")).toEqual({
      account: "home",
      addresses: ["robin@example.com", "kim@example.com", "room-1@example.com"],
      start: "2099-10-14",
      end: "2099-10-14",
      zone: "Europe/Berlin",
      interval_minutes: 30,
    });
    expect(freeBusyParams(fields({ allDay: true, start: "2099-10-15", end: "" }), "home", "UTC")?.start).toBe("2099-10-15");
    expect(freeBusyParams(fields({ start: "" }), "home", "UTC")).toBeNull();
  });

  it("reads each row's slots from 07:00 to 20:00 and an outside address as none", () => {
    // 09:00 local is slot 18 of the day, the grid's slot 4.
    const rows = availabilityRows(result({ 18: "2", 19: "2", 28: "1", 30: "3", 31: "4" }), "2099-10-14");
    expect(rows).toHaveLength(2);
    expect(rows[0].slots).toHaveLength(26);
    expect(rows[0].slots.slice(3, 6)).toEqual(["free", "busy", "busy"]);
    expect(rows[0].slots.slice(14, 18)).toEqual(["tentative", "free", "oof", "working-elsewhere"]);
    expect(rows[1]).toEqual({ address: "someone@gmail.com", error: "Proxy web request failed (5016)", slots: [] });
    expect(slotStatus("?")).toBe("unknown");
    expect(slotStatus(undefined)).toBe("unknown");
  });

  it("marks the chosen time's slots, none for an all-day event", () => {
    const r = result({});
    expect(chosenSlots(fields(), r, "2099-10-14")).toEqual([14, 16]);
    expect(chosenSlots(fields({ end: "2099-10-14T15:00" }), r, "2099-10-14")).toEqual([14, 15]);
    expect(chosenSlots(fields({ allDay: true }), r, "2099-10-14")).toBeNull();
    expect(chosenSlots(fields({ end: "2099-10-14T13:00" }), r, "2099-10-14")).toBeNull();
  });
});
