import { useEffect, useRef, type CSSProperties } from "react";
import {
  calendarOf,
  calendarTag,
  dayLabel,
  eventTitle,
  gridBlocks,
  partLabel,
  statusBadge,
  WEEKDAYS,
  weekday,
  type CalendarEntry,
  type Day,
} from "@/app/calendar";
import type { CalendarInfo } from "@/protocol/types";

/** The height of one hour of the grid. */
const HOUR_PX = 44;
const HOURS = Array.from({ length: 24 }, (_, h) => h);
/** Where a grid opens, scrolled: the working morning. */
const FIRST_SHOWN_HOUR = 7;

export type CalendarGridProps = {
  /** "Day" or "Week", the listbox's name. */
  label: string;
  days: Day[];
  entries: CalendarEntry[];
  calendars: readonly CalendarInfo[] | null;
  cursor: string | null;
  today: Day;
  focusSeq: number;
  onSelect: (key: string) => void;
  onOpen: (key: string) => void;
};

type BlockProps = {
  entry: CalendarEntry;
  calendars: readonly CalendarInfo[] | null;
  cursor: boolean;
  /** The block's column among the events it overlaps, and how many there are. */
  column?: { at: number; of: number };
  style?: CSSProperties;
  className: string;
  onSelect: (key: string) => void;
  onOpen: (key: string) => void;
};

/** One event on a grid: a chip in the all-day band, or a block over its hours. */
function Block({ entry, calendars, cursor, column, style, className, onSelect, onOpen }: BlockProps) {
  const { event } = entry;
  const title = eventTitle(event);
  const when = partLabel(entry.part);
  const tag = calendarTag(calendars, event);
  const badge = statusBadge(event);
  const color = calendarOf(calendars, event)?.color ?? null;
  return (
    <div
      role="option"
      aria-selected={cursor}
      aria-label={[when, title, tag, badge].filter(Boolean).join(", ")}
      tabIndex={cursor ? 0 : -1}
      data-roving={cursor ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-key={entry.key}
      data-day={entry.day}
      data-event-id={event.id}
      data-cancelled={event.cancelled || undefined}
      data-column={column ? `${column.at + 1}/${column.of}` : undefined}
      title={`${when} ${title} (${tag})`}
      onClick={() => onSelect(entry.key)}
      onDoubleClick={() => onOpen(entry.key)}
      style={{ ...style, ...(color ? { borderLeftColor: color } : {}) }}
      className={`cursor-default overflow-hidden rounded-sm border border-l-4 border-border px-1 text-xs leading-tight outline-none select-none focus-visible:outline-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : "bg-card hover:bg-accent"
      } ${event.cancelled ? "line-through opacity-70" : ""} ${className}`}
    >
      {entry.part.kind === "timed" ? <span className="tabular-nums text-muted-foreground">{when} </span> : null}
      <span className="font-medium">{title}</span>
    </div>
  );
}

/**
 * The day and week layouts (D10, calcure's model): one column per day,
 * Monday to Sunday on the week, today marked; the all-day events in a band
 * on top, and the timed ones over an hour grid from 0 to 24, those that
 * overlap side by side. Every event is an option of one listbox, so the
 * cursor and a click work as on the agenda.
 */
export function CalendarGrid({ label, days, entries, calendars, cursor, today, focusSeq, onSelect, onOpen }: CalendarGridProps) {
  const scroller = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (scroller.current) scroller.current.scrollTop = FIRST_SHOWN_HOUR * HOUR_PX;
  }, []);
  useEffect(() => {
    scroller.current?.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [cursor, focusSeq]);
  const byDay = new Map<Day, CalendarEntry[]>(days.map((d) => [d, []]));
  for (const entry of entries) byDay.get(entry.day)?.push(entry);
  const bandRows = Math.max(1, ...days.map((d) => (byDay.get(d) ?? []).filter((e) => e.part.kind === "all-day").length));
  const columns = `3rem repeat(${days.length}, minmax(0, 1fr))`;
  return (
    <div ref={scroller} role="listbox" aria-label={label} data-layout={label.toLowerCase()} className="relative h-full min-h-0 overflow-y-auto">
      <div className="sticky top-0 z-20 grid border-b border-border bg-background" style={{ gridTemplateColumns: columns }}>
        <div role="presentation" />
        {days.map((day) => (
          <div
            key={day}
            role="presentation"
            data-today={day === today || undefined}
            aria-current={day === today ? "date" : undefined}
            className="truncate border-l border-border px-1 py-1 text-center text-xs text-muted-foreground data-[today]:bg-accent data-[today]:font-semibold data-[today]:text-foreground"
          >
            {days.length === 1 ? dayLabel(day) : `${WEEKDAYS[weekday(day)]} ${Number(day.slice(8))}`}
          </div>
        ))}
        <div role="presentation" className="px-1 py-1 text-right text-[10px] text-muted-foreground">
          all day
        </div>
        {days.map((day) => (
          <div
            key={day}
            role="group"
            aria-label={`${dayLabel(day)}, all day`}
            data-day={day}
            data-band="all-day"
            data-today={day === today || undefined}
            className="flex flex-col gap-0.5 border-l border-border p-0.5 data-[today]:bg-accent/40"
            style={{ minHeight: `${bandRows * 1.4 + 0.4}rem` }}
          >
            {(byDay.get(day) ?? [])
              .filter((e) => e.part.kind === "all-day")
              .map((entry) => (
                <Block key={entry.key} entry={entry} calendars={calendars} cursor={entry.key === cursor} className="truncate py-0.5" onSelect={onSelect} onOpen={onOpen} />
              ))}
          </div>
        ))}
      </div>
      <div className="grid" style={{ gridTemplateColumns: columns, height: `${24 * HOUR_PX}px` }}>
        <div role="presentation" className="relative">
          {HOURS.map((h) => (
            <div key={h} className="absolute right-1 -translate-y-1/2 text-[10px] tabular-nums text-muted-foreground" style={{ top: `${h * HOUR_PX}px` }}>
              {h === 0 ? "" : `${String(h).padStart(2, "0")}:00`}
            </div>
          ))}
        </div>
        {days.map((day) => (
          <div
            key={day}
            role="group"
            aria-label={dayLabel(day)}
            data-day={day}
            data-today={day === today || undefined}
            className="relative border-l border-border data-[today]:bg-accent/30"
          >
            {HOURS.map((h) => (
              <div key={h} role="presentation" className="absolute inset-x-0 border-t border-border/60" style={{ top: `${h * HOUR_PX}px` }} />
            ))}
            {gridBlocks(byDay.get(day) ?? []).map((b) => (
              <Block
                key={b.entry.key}
                entry={b.entry}
                calendars={calendars}
                cursor={b.entry.key === cursor}
                column={{ at: b.column, of: b.columns }}
                className="absolute"
                style={{
                  top: `${(b.from / 60) * HOUR_PX}px`,
                  height: `${Math.max(((b.to - b.from) / 60) * HOUR_PX - 1, 14)}px`,
                  left: `calc(${(b.column / b.columns) * 100}% + 1px)`,
                  width: `calc(${100 / b.columns}% - 2px)`,
                }}
                onSelect={onSelect}
                onOpen={onOpen}
              />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
