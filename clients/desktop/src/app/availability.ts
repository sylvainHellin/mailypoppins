// The event form's availability grid (CAL-14, PERSO-255; the TUI's
// availability rows, the CLI's `mp calendar freebusy`): the free/busy of the
// user, the attendees and the rooms over the day of the form's start, which
// `calendar.freebusy` answers. Pure functions over the form's fields and the
// settled answer; the grid component asks once the query has stood still for
// AVAILABILITY_DEBOUNCE_MS, so typing an address asks once.

import type { CalendarFreeBusyParams, CalendarFreeBusyResult } from "@/protocol/types";
import type { EventFields } from "@/app/state";
import { addresses } from "@/app/eventForm";

/** How long the form's query stands still before it is asked. */
export const AVAILABILITY_DEBOUNCE_MS = 500;
/** The slot length the form asks for. */
export const AVAILABILITY_INTERVAL = 30;
/** The hours a row shows, from the first to the last, excluded. */
export const FIRST_HOUR = 7;
export const LAST_HOUR = 20;

/** What one slot of a schedule's view is. */
export type SlotStatus = "free" | "tentative" | "busy" | "oof" | "working-elsewhere" | "unknown";

const STATUS: Record<string, SlotStatus> = {
  "0": "free",
  "1": "tentative",
  "2": "busy",
  "3": "oof",
  "4": "working-elsewhere",
};

/** The status a view's digit stands for; anything else, a slot past the view included, is unknown. */
export function slotStatus(digit: string | undefined): SlotStatus {
  return (digit !== undefined && STATUS[digit]) || "unknown";
}

/** How a status reads, for a cell's title and the legend. */
export const STATUS_WORD: Record<SlotStatus, string> = {
  free: "free",
  tentative: "tentative",
  busy: "busy",
  oof: "away",
  "working-elsewhere": "working elsewhere",
  unknown: "unknown",
};

const DAY = /^\d{4}-\d{2}-\d{2}/;
const WHOLE = /^[^@\s]+@[^@\s.]+(\.[^@\s.]+)+$/;

/**
 * `calendar.freebusy`'s params for the form: the day of its start in `zone`,
 * every whole address of its three lists once (a half-typed one waits); null
 * while the start names no day.
 */
export function freeBusyParams(fields: EventFields, account: string, zone: string): CalendarFreeBusyParams | null {
  const day = DAY.exec(fields.start)?.[0];
  if (!day) return null;
  const asked: string[] = [];
  for (const list of [fields.to, fields.optional, fields.rooms]) {
    for (const address of addresses(list)) {
      if (WHOLE.test(address) && !asked.some((a) => a.toLowerCase() === address.toLowerCase())) asked.push(address);
    }
  }
  return { account, addresses: asked, start: day, end: day, zone, interval_minutes: AVAILABILITY_INTERVAL };
}

/** One row of the grid: an address and its slots from FIRST_HOUR to LAST_HOUR, or why it has none. */
export type AvailabilityRow = { address: string; error: string | null; slots: SlotStatus[] };

/** The slot of `result` that the local time `at` (`YYYY-MM-DDTHH:MM`) falls in. */
function slotAt(result: CalendarFreeBusyResult, at: string): number {
  const interval = Math.max(result.interval_minutes, 1) * 60000;
  return Math.floor((new Date(at).getTime() - Date.parse(result.start)) / interval);
}

/** The grid's rows over `day` (`YYYY-MM-DD`, in this window's zone), one per schedule in the answer's order. */
export function availabilityRows(result: CalendarFreeBusyResult, day: string): AvailabilityRow[] {
  const first = slotAt(result, `${day}T${String(FIRST_HOUR).padStart(2, "0")}:00`);
  const count = ((LAST_HOUR - FIRST_HOUR) * 60) / Math.max(result.interval_minutes, 1);
  return result.schedules.map((s) => ({
    address: s.address,
    error: s.error ?? null,
    slots: s.error ? [] : Array.from({ length: count }, (_, n) => slotStatus(s.view[first + n])),
  }));
}

/**
 * The slots of the form's chosen time as indexes into a row, both included;
 * null for an all-day event, a time off the grid's day, or an end before the start.
 */
export function chosenSlots(fields: EventFields, result: CalendarFreeBusyResult, day: string): [number, number] | null {
  if (fields.allDay || fields.end <= fields.start) return null;
  const first = slotAt(result, `${day}T${String(FIRST_HOUR).padStart(2, "0")}:00`);
  const from = slotAt(result, fields.start) - first;
  const to = Math.ceil((new Date(fields.end).getTime() - Date.parse(result.start)) / (Math.max(result.interval_minutes, 1) * 60000)) - 1 - first;
  return Number.isFinite(from) && Number.isFinite(to) ? [from, to] : null;
}

/**
 * The slots an edited event holds now (its fields as the form opened), so
 * the organizer's busy block there reads as the event itself; null for an
 * all-day event or one that starts on another day.
 */
export function currentSlots(original: EventFields | null | undefined, result: CalendarFreeBusyResult, day: string): [number, number] | null {
  if (!original || !original.start.startsWith(day)) return null;
  return chosenSlots(original, result, day);
}
