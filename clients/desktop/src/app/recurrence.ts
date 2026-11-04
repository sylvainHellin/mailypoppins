// A recurring event's rule in the event form (CAL-11, CAL-12, PERSO-255;
// the TUI's Repeats rows, the CLI's `--repeat`): the patterns Outlook's
// repeat menu offers, each read against the form's first day, the rule
// `event.create` and `event.update` take (`CalendarRecurrence`, whatever
// the pattern leaves out the daemon fills from that day), and the rule in
// words as the cards print it, mirroring `mp_core`'s `Recurrence::describe`.
// Pure functions.

import type { CalendarRecurrence } from "@/protocol/types";

/** The patterns the Repeat control offers. */
export type RepeatPattern = "daily" | "weekly" | "monthly-date" | "monthly-weekday" | "yearly";

export const REPEAT_PATTERNS: readonly RepeatPattern[] = ["daily", "weekly", "monthly-date", "monthly-weekday", "yearly"];

/**
 * The Repeat section of the event form: the pattern (null for an event that
 * does not repeat, and for a series whose rule no pattern names, which
 * `kept` holds until a pattern replaces it), its interval, a weekly series'
 * days (none for the first day's own), and its end. `series` is the rule in
 * words of the series an occurrence being changed on its own belongs to.
 */
export type RepeatFields = {
  pattern: RepeatPattern | null;
  every: string;
  on: string[];
  ends: "never" | "until" | "count";
  until: string;
  count: string;
  kept: CalendarRecurrence | null;
  series: string | null;
};

export const NO_REPEAT: RepeatFields = { pattern: null, every: "1", on: [], ends: "never", until: "", count: "", kept: null, series: null };

/** The weekdays, Monday first, as the wire spells them. */
export const WEEKDAYS = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"] as const;
const INDEXES = ["first", "second", "third", "fourth", "last"] as const;
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

const capital = (word: string) => word.charAt(0).toUpperCase() + word.slice(1);

/** The parts of a `YYYY-MM-DD` day, or null for another text. */
function partsOf(day: string): { year: number; month: number; date: number; weekday: string } | null {
  if (!/^\d{4}-\d{2}-\d{2}/.test(day)) return null;
  const at = new Date(`${day.slice(0, 10)}T12:00:00Z`);
  if (Number.isNaN(at.getTime())) return null;
  return { year: at.getUTCFullYear(), month: at.getUTCMonth() + 1, date: at.getUTCDate(), weekday: WEEKDAYS[(at.getUTCDay() + 6) % 7] };
}

/** The week of its month `day` falls in, as a rule on its weekday names it: the first to fourth for the 1st to the 28th, the last after. */
export function weekIndexOf(day: string): string {
  const parts = partsOf(day);
  return INDEXES[Math.min(4, Math.floor(((parts?.date ?? 1) - 1) / 7))];
}

/** `rule` with what its pattern leaves out filled from `day`, as the daemon fills it. */
export function filled(rule: CalendarRecurrence, day: string): CalendarRecurrence {
  const parts = partsOf(day);
  if (!parts) return rule;
  const own = rule.weekdays.length > 0 ? rule.weekdays : [parts.weekday];
  switch (rule.frequency) {
    case "weekly":
      return { ...rule, weekdays: own };
    case "monthly":
      return rule.week_index ? { ...rule, weekdays: own } : { ...rule, day_of_month: rule.day_of_month ?? parts.date };
    case "yearly":
      return rule.week_index
        ? { ...rule, weekdays: own, month: rule.month ?? parts.month }
        : { ...rule, day_of_month: rule.day_of_month ?? parts.date, month: rule.month ?? parts.month };
    default:
      return rule;
  }
}

/** A rule in words, as the cards print it: `Weekly on Wednesday`, `Every 2 months on the last Thursday (6 times)`. */
export function describeRecurrence(rule: CalendarRecurrence): string {
  const units: Record<string, [string, string]> = {
    daily: ["Daily", "days"],
    weekly: ["Weekly", "weeks"],
    monthly: ["Monthly", "months"],
    yearly: ["Yearly", "years"],
  };
  const [unit, plural] = units[rule.frequency] ?? [capital(rule.frequency), rule.frequency];
  let text = rule.interval > 1 ? `Every ${rule.interval} ${plural}` : unit;
  const days = rule.weekdays.map(capital).join(", ");
  const month = rule.month ? MONTHS[rule.month - 1] : undefined;
  if (rule.frequency === "weekly") {
    if (days) text += ` on ${days}`;
  } else if ((rule.frequency === "monthly" || rule.frequency === "yearly") && rule.week_index && days) {
    text += ` on the ${rule.week_index} ${days}`;
    if (rule.frequency === "yearly" && month) text += ` of ${month}`;
  } else if (rule.frequency === "monthly") {
    if (rule.day_of_month) text += ` on day ${rule.day_of_month}`;
  } else if (rule.frequency === "yearly") {
    if (rule.day_of_month && month) text += ` on ${rule.day_of_month} ${month}`;
    else if (month) text += ` in ${month}`;
  }
  if (rule.until) text += ` until ${rule.until}`;
  else if (rule.count) text += ` (${rule.count} times)`;
  return text;
}

/** The pattern's words on `day`, as the Repeat control lists it. */
export function patternLabel(pattern: RepeatPattern, day: string): string {
  const base: Record<RepeatPattern, CalendarRecurrence> = {
    daily: { frequency: "daily", interval: 1, weekdays: [] },
    weekly: { frequency: "weekly", interval: 1, weekdays: [] },
    "monthly-date": { frequency: "monthly", interval: 1, weekdays: [] },
    "monthly-weekday": { frequency: "monthly", interval: 1, weekdays: [], week_index: weekIndexOf(day) },
    yearly: { frequency: "yearly", interval: 1, weekdays: [] },
  };
  return describeRecurrence(filled(base[pattern], day));
}

/**
 * The rule the Repeat section names on `day`: null for an event that does
 * not repeat (or a kept rule as it was); a problem with what was typed
 * otherwise. The daemon checks it again.
 */
export function ruleOf(repeat: RepeatFields, day: string): { rule: CalendarRecurrence | null } | { problem: string } {
  if (repeat.series !== null) return { rule: null };
  if (repeat.pattern === null) return { rule: repeat.kept };
  const every = Number(repeat.every.trim());
  if (!/^\d+$/.test(repeat.every.trim()) || every < 1 || every > 99) return { problem: "Every is a number from 1 to 99" };
  const rule: CalendarRecurrence = {
    frequency: repeat.pattern.startsWith("monthly") ? "monthly" : repeat.pattern,
    interval: every,
    weekdays: repeat.pattern === "weekly" ? WEEKDAYS.filter((d) => repeat.on.includes(d)) : [],
  };
  if (repeat.pattern === "monthly-weekday") rule.week_index = weekIndexOf(day);
  if (repeat.ends === "until") {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(repeat.until)) return { problem: "The series' last day is YYYY-MM-DD" };
    if (repeat.until < day.slice(0, 10)) return { problem: `The series ends before its first day, ${day.slice(0, 10)}` };
    rule.until = repeat.until;
  } else if (repeat.ends === "count") {
    const count = Number(repeat.count.trim());
    if (!/^\d+$/.test(repeat.count.trim()) || count < 1 || count > 999) return { problem: "A series has 1 to 999 occurrences" };
    rule.count = count;
  }
  return { rule };
}

/** The Repeat section filled from a series' `rule` on its first `day`: a pattern the form names, else the rule kept as it is. */
export function repeatFromRule(rule: CalendarRecurrence, day: string): RepeatFields {
  const parts = partsOf(day);
  const ownDay = rule.weekdays.length === 1 && rule.weekdays[0] === parts?.weekday;
  const onDate = !rule.day_of_month || rule.day_of_month === parts?.date;
  let pattern: RepeatPattern | null = null;
  if (rule.frequency === "daily") pattern = "daily";
  else if (rule.frequency === "weekly") pattern = "weekly";
  else if (rule.frequency === "monthly" && !rule.week_index && onDate) pattern = "monthly-date";
  else if (rule.frequency === "monthly" && rule.week_index === weekIndexOf(day) && ownDay) pattern = "monthly-weekday";
  else if (rule.frequency === "yearly" && !rule.week_index && onDate && (!rule.month || rule.month === parts?.month)) pattern = "yearly";
  return {
    pattern,
    every: String(Math.max(1, rule.interval)),
    on: pattern === "weekly" ? [...rule.weekdays] : [],
    ends: rule.until ? "until" : rule.count ? "count" : "never",
    until: rule.until ?? "",
    count: rule.count ? String(rule.count) : "",
    kept: pattern === null ? rule : null,
    series: null,
  };
}

/** The Repeat section's words on `day`: the rule, the series an occurrence stays part of, or that the event does not repeat. */
export function repeatLabel(repeat: RepeatFields, day: string): string {
  if (repeat.series !== null) return `${repeat.series} (this occurrence only)`;
  const made = ruleOf(repeat, day);
  if ("problem" in made) return repeat.pattern ? patternLabel(repeat.pattern, day) : "Does not repeat";
  return made.rule ? describeRecurrence(filled(made.rule, day)) : "Does not repeat";
}

/** Whether two rules are the same, as an edit compares the one it opened with. */
export function sameRule(a: CalendarRecurrence | null, b: CalendarRecurrence | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
