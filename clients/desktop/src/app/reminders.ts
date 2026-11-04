// The calendar reminders (CAL-15, INT-07, PERSO-255).
//
// The daemon fires a reminder at its due minute as `calendar.reminder`, and
// says with `calendar.reminders_changed` that one was dismissed, snoozed or
// cleared in any client. The window keeps the waiting list
// (`calendar_reminders`), read at every bootstrap and again at every
// `calendar.reminders_changed`, and shows it as the reminder banner with
// Snooze and Dismiss, since macOS shows no notification banner for the
// frontmost app. The system notification is the Rust layer's
// (src-tauri/src/reminders.rs); a click on it brings the window forward and
// opens the event here (`reminder:open`).

import type { CalendarReminder } from "@/protocol/types";
import { markStale, type AppState } from "@/app/state";
import type { GuiError } from "@/lib/gui-types";
import { calendarGoTo, dayOf, openCalendar, selectCalendarEntry } from "@/app/calendar";

/** How long Snooze puts a reminder off, the TUI's `s` too. */
export const SNOOZE_MINUTES = 5;

/** Whether `r` is the reminder of `account`'s `event`. */
function same(r: Pick<CalendarReminder, "account" | "event">, account: string, event: string): boolean {
  return r.account === account && r.event === event;
}

/** Read the waiting reminders again. */
export function staleReminders(s: AppState): AppState {
  return { ...s, reminders: markStale(s.reminders) };
}

export function remindersLoaded(s: AppState, gen: number, reminders: CalendarReminder[]): AppState {
  return { ...s, reminders: { ...s.reminders, data: reminders, loadedGen: gen, error: null } };
}

export function remindersFailed(s: AppState, gen: number, error: GuiError): AppState {
  return { ...s, reminders: { ...s.reminders, loadedGen: gen, error } };
}

/**
 * `calendar.reminder`: a reminder fell due. It joins the list in start
 * order, replacing an earlier entry of the same event (a snooze that ended).
 */
export function reminderFired(s: AppState, payload: unknown): AppState {
  if (!isReminder(payload)) return s;
  const kept = (s.reminders.data ?? []).filter((r) => !same(r, payload.account, payload.event));
  const data = [...kept, payload].sort((a, b) => a.start.localeCompare(b.start));
  return { ...s, reminders: { ...s.reminders, data } };
}

function isReminder(p: unknown): p is CalendarReminder {
  if (typeof p !== "object" || p === null) return false;
  const r = p as Record<string, unknown>;
  return typeof r.account === "string" && typeof r.event === "string" && typeof r.summary === "string" && typeof r.start === "string";
}

/** Take a reminder off the list at once, ahead of the `calendar.reminders_changed` that confirms it. */
export function reminderDecided(s: AppState, account: string, event: string): AppState {
  const data = s.reminders.data?.filter((r) => !same(r, account, event)) ?? null;
  return { ...s, reminders: { ...s.reminders, data } };
}

/**
 * Open a reminder's event: the Calendar view on the day it starts, the
 * cursor on it, which shows its card.
 */
export function openReminderEvent(s: AppState, account: string, event: string, start: string): AppState {
  const at = new Date(start);
  if (Number.isNaN(at.getTime())) return s;
  const day = dayOf(at);
  const shown = calendarGoTo(openCalendar(s), day);
  return selectCalendarEntry(shown, `${account}\u0000${event}\u0000${day}`);
}

/** The minutes from `now` to `start`, rounded toward zero as the CLI and the TUI do. */
function minutesUntil(start: Date, now: Date): number {
  return Math.trunc((start.getTime() - now.getTime()) / 60_000);
}

function span(minutes: number): string {
  const h = Math.trunc(minutes / 60);
  const m = minutes % 60;
  if (h === 0) return `${m} min`;
  return m === 0 ? `${h} h` : `${h} h ${m} min`;
}

const clock = (d: Date): string => `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;

/**
 * When a reminder's event starts, from `now`, in the local zone, in the
 * words of `mp_client::format::reminder_when`: `in 15 min (09:00)`, `now
 * (09:00)`, `started 5 min ago (09:00)`, `all day today`, `all day
 * tomorrow`, `all day Mon 9 Nov`.
 */
export function reminderWhen(r: Pick<CalendarReminder, "start" | "all_day">, now: Date = new Date()): string {
  const start = new Date(r.start);
  if (Number.isNaN(start.getTime())) return r.start;
  if (r.all_day) {
    const days = Math.round((Date.parse(`${dayOf(start)}T00:00:00Z`) - Date.parse(`${dayOf(now)}T00:00:00Z`)) / 86_400_000);
    if (days === 0) return "all day today";
    if (days === 1) return "all day tomorrow";
    const weekday = start.toLocaleDateString("en-GB", { weekday: "short" });
    const month = start.toLocaleDateString("en-GB", { month: "short" });
    return `all day ${weekday} ${start.getDate()} ${month}`;
  }
  const minutes = minutesUntil(start, now);
  if (minutes === 0) return `now (${clock(start)})`;
  return minutes > 0 ? `in ${span(minutes)} (${clock(start)})` : `started ${span(-minutes)} ago (${clock(start)})`;
}

/** The reminder's line in the banner: when and, when it has one, where. */
export function reminderDetail(r: CalendarReminder, now: Date = new Date()): string {
  const where = r.location?.trim();
  return where ? `${reminderWhen(r, now)}, ${where}` : reminderWhen(r, now);
}
