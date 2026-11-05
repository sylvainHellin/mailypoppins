// The event form's availability grid (CAL-14, PERSO-255; the TUI's
// availability rows): a small timeline from 07:00 to 20:00, one row per
// person, the user first, then the attendees and the rooms, each 30-minute
// slot filled by its status, hour gridlines under the hour labels, the chosen
// time a band across every row, a tooltip naming the person and the slot, and
// "no free/busy" for an address the server cannot read. It asks
// `calendar.freebusy` once the form's day and addresses have stood still for
// AVAILABILITY_DEBOUNCE_MS; until the answer for the query the form names
// comes, the last answer's rows stay, dimmed and marked as checking, and an
// address the answer lacks waits as a placeholder row, so the grid neither
// collapses nor jumps under the field being typed in. A calendar whose
// backend serves no free/busy shows nothing. On the edit form the slots the
// event holds now are outlined, with a legend, so the organizer's busy block
// there reads as the event itself rather than a clash.

import { useEffect, useState, type CSSProperties } from "react";
import { DoorOpen } from "lucide-react";
import type { CalendarFreeBusyParams, CalendarFreeBusyResult } from "@/protocol/types";
import type { EventCalendar, EventFields } from "@/app/state";
import {
  AVAILABILITY_DEBOUNCE_MS,
  AVAILABILITY_INTERVAL,
  FIRST_HOUR,
  LAST_HOUR,
  STATUS_WORD,
  availabilityRows,
  chosenSlots,
  currentSlots,
  freeBusyParams,
  type AvailabilityRow,
  type SlotStatus,
} from "@/app/availability";
import { addresses, localZone } from "@/app/eventForm";
import { Skeleton } from "@/components/ui/skeleton";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";

const FILL: Record<SlotStatus, string> = {
  free: "",
  tentative: "bg-primary/40",
  busy: "bg-primary",
  oof: "bg-destructive/70",
  "working-elsewhere": "bg-link/50",
  unknown: "bg-muted-foreground/25",
};

/** The statuses the legend names, free being the empty track. */
const LEGEND: SlotStatus[] = ["busy", "tentative", "oof", "working-elsewhere"];

/** The form's label column, so the timeline lines up with the fields above it. */
const COLUMNS = "grid grid-cols-[6rem_minmax(0,1fr)] gap-x-2";

const HOURS = Array.from({ length: LAST_HOUR - FIRST_HOUR }, (_, n) => FIRST_HOUR + n);

/** An answer and the query it answers: its key and the day its rows read. */
type Answer = { key: string; day: string; result: CalendarFreeBusyResult | null; error: string | null };

/** Who a row is: the user, an attendee, an optional one or a room. */
type Kind = "you" | "required" | "optional" | "room";

/** One line of the grid: an answered row, or an address still on its way (`row` null). */
type Line = { address: string | null; kind: Kind; row: AvailabilityRow | null };

/** `current` is an edited event's fields as the form opened, whose time the grid outlines. */
export type AvailabilityGridProps = { fields: EventFields | null; calendar: EventCalendar | null; current?: EventFields | null };

export function AvailabilityGrid({ fields, calendar, current = null }: AvailabilityGridProps) {
  const params = fields && calendar?.freeBusy ? freeBusyParams(fields, calendar.account, localZone()) : null;
  const key = params ? JSON.stringify(params) : null;
  const [answer, setAnswer] = useState<Answer | null>(null);
  const [tip, setTip] = useState<{ address: string; slot: number } | null>(null);

  useEffect(() => {
    if (!key) return;
    let live = true;
    const timer = setTimeout(() => {
      const asked = JSON.parse(key) as CalendarFreeBusyParams;
      const day = asked.start;
      cmd.calendarFreeBusy(asked).then(
        (result) => live && setAnswer({ key, day, result, error: null }),
        (e: unknown) => live && setAnswer({ key, day, result: null, error: asGuiError(e).message }),
      );
    }, AVAILABILITY_DEBOUNCE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [key]);

  if (!fields || !params || !key) return null;
  // The last answer, the form's own or, while that is on its way, the one
  // before it, shown stale.
  const stale = answer !== null && answer.key !== key;
  const checking = answer === null || stale;
  const result = answer?.result ?? null;
  const day = answer?.day ?? params.start;
  const rows = result ? availabilityRows(result, day) : [];
  const picked = result ? chosenSlots(fields, result, day) : null;
  const now = result ? currentSlots(current, result, day) : null;
  const count = rows.find((r) => !r.error)?.slots.length ?? ((LAST_HOUR - FIRST_HOUR) * 60) / AVAILABILITY_INTERVAL;

  const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
  const askedBefore = answer ? (JSON.parse(answer.key) as CalendarFreeBusyParams).addresses : [];
  const kindOf = (address: string): Kind =>
    addresses(fields.rooms).some((a) => same(a, address)) ? "room" : addresses(fields.optional).some((a) => same(a, address)) ? "optional" : "required";
  // The answer's rows (the user's is the one nobody asked for), then the
  // query's addresses the answer has not reached yet; before any answer the
  // user's row waits too. An answer that failed keeps the query's lines, empty.
  const lines: Line[] = result
    ? rows.map((row) => ({ address: row.address, kind: askedBefore.some((a) => same(a, row.address)) ? kindOf(row.address) : "you", row }))
    : [{ address: null, kind: "you", row: null }];
  for (const address of params.addresses) {
    if (!lines.some((l) => l.address !== null && same(l.address, address))) lines.push({ address, kind: kindOf(address), row: null });
  }

  const band = (span: [number, number] | null): CSSProperties | null => {
    if (!span) return null;
    const from = Math.max(span[0], 0);
    const to = Math.min(span[1], count - 1);
    if (to < from) return null;
    return { left: `${(from / count) * 100}%`, width: `${((to - from + 1) / count) * 100}%`, gridColumn: "2", gridRow: "2 / -1" };
  };
  const chosenBand = band(picked);
  // Drawn a little outside the chosen band, so the two read apart where they coincide.
  const nowBand = band(now);
  const currentBand = nowBand ? { ...nowBand, left: `calc(${nowBand.left} - 3px)`, width: `calc(${nowBand.width} + 6px)` } : null;
  const error = answer?.error ?? null;

  return (
    <section aria-label="Availability" aria-busy={stale} className="flex flex-col gap-1.5">
      <div className={`${COLUMNS} items-baseline`}>
        <span className="text-sm text-muted-foreground">Availability</span>
        <span className="flex min-w-0 flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
          {error ? (
            <span
              data-slot="availability-error"
              data-stale={stale ? "" : undefined}
              title={error}
              className={`min-w-0 truncate text-xs text-warning ${stale ? "opacity-50" : ""}`}
            >
              {`No availability: ${error}`}
            </span>
          ) : (
            <span data-slot="availability-legend" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
              {LEGEND.map((status) => (
                <span key={status} className="flex items-center gap-1">
                  <span aria-hidden="true" className={`size-2.5 rounded-[2px] ${FILL[status]}`} />
                  {STATUS_WORD[status]}
                </span>
              ))}
              {now ? (
                <span className="flex items-center gap-1">
                  <span aria-hidden="true" className="size-2.5 rounded-[2px] border border-dashed border-foreground" />
                  this event
                </span>
              ) : null}
            </span>
          )}
          {checking ? <span className="text-xs text-muted-foreground">(checking…)</span> : null}
        </span>
      </div>
      <div
        data-slot="availability-rows"
        data-stale={stale ? "" : undefined}
        className={`relative ${COLUMNS} gap-y-1 transition-opacity ${stale ? "opacity-50" : ""}`}
        style={{ gridTemplateRows: `auto repeat(${lines.length}, 1.25rem)` }}
      >
        <span aria-hidden="true" />
        <div aria-hidden="true" className="relative h-4 text-[10px] leading-4 text-muted-foreground tabular-nums">
          {HOURS.map((h) => (
            <span key={h} className="absolute top-0 pl-0.5" style={{ left: `${((h - FIRST_HOUR) / HOURS.length) * 100}%` }}>
              {String(h).padStart(2, "0")}
            </span>
          ))}
        </div>
        {lines.map((line, i) => {
          const who = line.kind === "you" ? "You" : (line.address ?? "");
          const name = line.kind === "you" ? "You" : who.split("@")[0];
          const label = (
            <span
              className={`flex min-w-0 items-center gap-1 text-xs ${line.kind === "you" ? "font-medium" : line.kind === "optional" ? "text-muted-foreground" : ""}`}
              title={line.address ? `${line.address}${line.kind === "optional" ? " (optional)" : line.kind === "room" ? " (room)" : ""}` : undefined}
            >
              {line.kind === "room" ? <DoorOpen aria-hidden="true" className="size-3 shrink-0 text-muted-foreground" /> : null}
              <span className="truncate">{name}</span>
            </span>
          );
          if (!line.row) {
            return (
              <div key={line.address ?? `you-${i}`} data-slot="availability-pending" data-address={line.address ?? undefined} className="contents">
                {label}
                {error ? <span className="rounded-[3px] bg-muted" /> : <Skeleton className="rounded-[3px]" />}
              </div>
            );
          }
          const row = line.row;
          return (
            <div key={row.address} data-slot="availability-row" data-address={row.address} className="contents">
              {label}
              {row.error ? (
                <span
                  data-slot="availability-none"
                  title={row.error}
                  className="flex items-center justify-end rounded-[3px] bg-muted/60 px-2 text-xs text-muted-foreground"
                >
                  no free/busy
                </span>
              ) : (
                <div className="relative min-w-0" onMouseLeave={() => setTip(null)}>
                  <div
                    role="img"
                    aria-label={`${row.address}: ${summary(row.slots, now)}`}
                    className="absolute inset-0 flex overflow-hidden rounded-[3px] bg-muted"
                  >
                    {row.slots.map((status, n) => {
                      const chosen = picked !== null && n >= picked[0] && n <= picked[1];
                      const held = now !== null && n >= now[0] && n <= now[1];
                      return (
                        <span
                          key={n}
                          data-status={status}
                          data-chosen={chosen ? "" : undefined}
                          data-current={held ? "" : undefined}
                          data-tip={`${slotTime(n)} ${STATUS_WORD[status]}${held ? ", this event" : ""}`}
                          onMouseEnter={() => setTip({ address: row.address, slot: n })}
                          className={`h-full flex-1 ${FILL[status]}`}
                        />
                      );
                    })}
                  </div>
                  {tip && tip.address === row.address ? (
                    <SlotTip who={line.kind === "you" ? `You (${row.address})` : row.address} kind={line.kind} slot={tip.slot} count={count}>
                      {`${slotTime(tip.slot)} to ${slotTime(tip.slot + 1)}: ${STATUS_WORD[row.slots[tip.slot]]}${
                        now !== null && tip.slot >= now[0] && tip.slot <= now[1] ? ", this event" : ""
                      }`}
                    </SlotTip>
                  ) : null}
                </div>
              )}
            </div>
          );
        })}
        <div aria-hidden="true" className="pointer-events-none absolute inset-0" style={{ gridColumn: "2", gridRow: "1 / -1" }}>
          {HOURS.slice(1).map((h) => (
            <span key={h} className="absolute inset-y-0 w-px bg-foreground/10" style={{ left: `${((h - FIRST_HOUR) / HOURS.length) * 100}%` }} />
          ))}
        </div>
        {currentBand ? (
          <div
            aria-hidden="true"
            data-slot="availability-current"
            className="pointer-events-none absolute -inset-y-1 rounded-[5px] border-2 border-dashed border-foreground/70"
            style={currentBand}
          />
        ) : null}
        {chosenBand ? (
          <div
            aria-hidden="true"
            data-slot="availability-chosen"
            className="pointer-events-none absolute -inset-y-0.5 rounded-[4px] border-2 border-framing bg-framing/15"
            style={chosenBand}
          />
        ) : null}
      </div>
    </section>
  );
}

/** The hover tooltip over a row's slot, kept inside the timeline at either end. */
function SlotTip({ who, kind, slot, count, children }: { who: string; kind: Kind; slot: number; count: number; children: string }) {
  const place: CSSProperties =
    slot < 4
      ? { left: `${(slot / count) * 100}%` }
      : slot >= count - 4
        ? { right: `${((count - slot - 1) / count) * 100}%` }
        : { left: `${((slot + 0.5) / count) * 100}%`, transform: "translateX(-50%)" };
  return (
    <span
      role="tooltip"
      data-slot="availability-tip"
      className="pointer-events-none absolute bottom-full z-50 mb-1.5 flex flex-col rounded-md bg-foreground px-3 py-1.5 text-xs whitespace-nowrap text-background"
      style={place}
    >
      <span className="font-medium">
        {who}
        {kind === "optional" ? ", optional" : kind === "room" ? ", room" : ""}
      </span>
      <span>{children}</span>
    </span>
  );
}

/** The local time a row's slot `n` starts at, `HH:MM`. */
function slotTime(n: number): string {
  const minutes = FIRST_HOUR * 60 + n * AVAILABILITY_INTERVAL;
  return `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
}

/** A row in words for a screen reader: the slots that are not free, and when the edited event is now. */
function summary(slots: SlotStatus[], now: [number, number] | null): string {
  const busy = slots.map((s, n) => (s === "free" ? null : `${slotTime(n)} ${STATUS_WORD[s]}`)).filter(Boolean);
  const words = busy.length === 0 ? "free all day" : busy.join(", ");
  return now ? `${words}; this event at ${slotTime(now[0])}` : words;
}
