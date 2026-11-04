// The event form's availability grid (CAL-14, PERSO-255; the TUI's
// availability rows): one row per person and room from 07:00 to 20:00, each
// 30-minute slot coloured by its status, the chosen time outlined, and "no
// free/busy" for an address the server cannot read. It asks
// `calendar.freebusy` once the form's day and addresses have stood still for
// AVAILABILITY_DEBOUNCE_MS; an answer for a query the form no longer names
// is dropped, and a calendar whose backend serves no free/busy shows nothing.

import { useEffect, useState } from "react";
import type { CalendarFreeBusyResult } from "@/protocol/types";
import type { EventCalendar, EventFields } from "@/app/state";
import {
  AVAILABILITY_DEBOUNCE_MS,
  FIRST_HOUR,
  LAST_HOUR,
  STATUS_WORD,
  availabilityRows,
  chosenSlots,
  freeBusyParams,
  type SlotStatus,
} from "@/app/availability";
import { localZone } from "@/app/eventForm";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";

const CELL: Record<SlotStatus, string> = {
  free: "bg-muted/50",
  tentative: "bg-primary/40",
  busy: "bg-primary",
  oof: "bg-destructive/70",
  "working-elsewhere": "bg-link/50",
  unknown: "bg-muted-foreground/25",
};

type Answer = { key: string; result: CalendarFreeBusyResult | null; error: string | null };

export type AvailabilityGridProps = { fields: EventFields | null; calendar: EventCalendar | null };

export function AvailabilityGrid({ fields, calendar }: AvailabilityGridProps) {
  const params = fields && calendar?.freeBusy ? freeBusyParams(fields, calendar.account, localZone()) : null;
  const key = params ? JSON.stringify(params) : null;
  const [answer, setAnswer] = useState<Answer | null>(null);

  useEffect(() => {
    if (!key) return;
    let live = true;
    const timer = setTimeout(() => {
      cmd.calendarFreeBusy(JSON.parse(key)).then(
        (result) => live && setAnswer({ key, result, error: null }),
        (e: unknown) => live && setAnswer({ key, result: null, error: asGuiError(e).message }),
      );
    }, AVAILABILITY_DEBOUNCE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [key]);

  if (!fields || !params || !key) return null;
  const current = answer?.key === key ? answer : null;
  const hours = Array.from({ length: LAST_HOUR - FIRST_HOUR }, (_, n) => FIRST_HOUR + n);
  const header = (
    <span className="text-sm text-muted-foreground">
      Availability
      {current ? null : <span className="ml-1 text-xs">(checking…)</span>}
    </span>
  );
  if (current?.error) {
    return (
      <section aria-label="Availability" className="grid grid-cols-[6rem_1fr] items-start gap-2">
        {header}
        <p data-slot="availability-error" className="text-xs text-warning">
          {`No availability: ${current.error}`}
        </p>
      </section>
    );
  }
  const result = current?.result ?? null;
  const rows = result ? availabilityRows(result, params.start) : [];
  const picked = result ? chosenSlots(fields, result, params.start) : null;
  return (
    <section aria-label="Availability" className="grid grid-cols-[6rem_1fr] items-start gap-2">
      {header}
      <div className="flex flex-col gap-1 overflow-hidden">
        <div className="grid grid-cols-[repeat(13,1fr)] text-[10px] text-muted-foreground" aria-hidden="true">
          {hours.map((h) => (
            <span key={h}>{String(h).padStart(2, "0")}</span>
          ))}
        </div>
        {rows.map((row) => (
          <div key={row.address} data-slot="availability-row" data-address={row.address} className="flex flex-col">
            <span className="truncate text-xs text-muted-foreground" title={row.address}>
              {row.address}
            </span>
            {row.error ? (
              <span data-slot="availability-none" className="text-xs text-muted-foreground" title={row.error}>
                no free/busy
              </span>
            ) : (
              <div className="flex h-3 gap-px" role="img" aria-label={`${row.address}: ${summary(row.slots)}`}>
                {row.slots.map((status, n) => {
                  const chosen = picked !== null && n >= picked[0] && n <= picked[1];
                  return (
                    <span
                      key={n}
                      data-status={status}
                      data-chosen={chosen ? "" : undefined}
                      title={`${slotTime(n)} ${STATUS_WORD[status]}`}
                      className={`flex-1 rounded-[1px] ${CELL[status]} ${chosen ? "outline outline-1 outline-foreground" : ""}`}
                    />
                  );
                })}
              </div>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}

/** The local time a row's slot `n` starts at, `HH:MM`. */
function slotTime(n: number): string {
  const minutes = FIRST_HOUR * 60 + n * 30;
  return `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
}

/** A row in words for a screen reader: the slots that are not free. */
function summary(slots: SlotStatus[]): string {
  const busy = slots.map((s, n) => (s === "free" ? null : `${slotTime(n)} ${STATUS_WORD[s]}`)).filter(Boolean);
  return busy.length === 0 ? "free all day" : busy.join(", ");
}
