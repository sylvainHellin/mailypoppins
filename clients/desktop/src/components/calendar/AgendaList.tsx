import { useEffect, useRef } from "react";
import { AgendaRow } from "@/components/calendar/AgendaRow";
import { calendarOf, calendarTag, dayLabel, type CalendarEntry, type Day } from "@/app/calendar";
import type { CalendarInfo } from "@/protocol/types";

export type AgendaListProps = {
  entries: CalendarEntry[];
  calendars: readonly CalendarInfo[] | null;
  cursor: string | null;
  today: Day;
  /** Moves whenever the keyboard moved the cursor, so the row is scrolled into view. */
  focusSeq: number;
  onSelect: (key: string) => void;
  onOpen: (key: string) => void;
};

/**
 * The agenda layout: the entries of the shown days, a single-select listbox
 * named "Agenda" grouped by day, the all-day ones first on their day.
 */
export function AgendaList({ entries, calendars, cursor, today, focusSeq, onSelect, onOpen }: AgendaListProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [cursor, focusSeq]);
  const days: { day: Day; entries: CalendarEntry[] }[] = [];
  for (const entry of entries) {
    const last = days[days.length - 1];
    if (last?.day === entry.day) last.entries.push(entry);
    else days.push({ day: entry.day, entries: [entry] });
  }
  let position = 0;
  return (
    <div ref={ref} role="listbox" aria-label="Agenda">
      {days.map(({ day, entries: onDay }) => (
        <div key={day} role="group" aria-label={dayLabel(day)} data-day={day}>
          <div
            role="presentation"
            data-today={day === today || undefined}
            className="sticky top-0 z-10 border-b border-border bg-background px-3 py-1 text-xs font-semibold text-muted-foreground data-[today]:text-foreground"
          >
            {dayLabel(day)}
            {day === today ? " (today)" : ""}
          </div>
          {onDay.map((entry) => {
            position += 1;
            return (
              <AgendaRow
                key={entry.key}
                entry={entry}
                tag={calendarTag(calendars, entry.event)}
                color={calendarOf(calendars, entry.event)?.color ?? null}
                cursor={entry.key === cursor}
                position={position}
                setSize={entries.length}
                onSelect={onSelect}
                onOpen={onOpen}
              />
            );
          })}
        </div>
      ))}
    </div>
  );
}
