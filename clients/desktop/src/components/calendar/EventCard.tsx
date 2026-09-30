import { useId, type ReactNode } from "react";
import type { EventFrontmatter } from "@/protocol/types";

/** A status word as a person reads it, the TUI's `humanize_status`. */
export function humanStatus(status: string): string {
  switch (status) {
    case "accepted":
      return "Accepted";
    case "declined":
      return "Declined";
    case "tentative":
      return "Tentative";
    case "needs-action":
    case "":
      return "No response yet";
    default:
      return status;
  }
}

export const STATUS_TONE: Record<string, string> = {
  accepted: "text-link",
  declined: "text-destructive",
  tentative: "text-warning",
};

/** An RFC3339 instant to the minute, its offset kept: `2099-10-14 10:00 +02:00`; a bare date stays. */
export function shortInstant(value: string): string {
  const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})(?::\d{2}(?:\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?$/.exec(value);
  if (!m) return value;
  return [m[1], m[2], m[3] === "Z" ? "UTC" : m[3]].filter(Boolean).join(" ");
}

/** The event's time span; the end drops the date when it is the start's. */
export function eventWhen(start: string | null | undefined, end: string | null | undefined): string | null {
  if (!start) return null;
  const from = shortInstant(start);
  if (!end) return from;
  const to = shortInstant(end);
  const day = from.slice(0, 10);
  return `${from} to ${to.startsWith(`${day} `) ? to.slice(11) : to}`;
}

/** "2 occurrences cancelled: a, b, c, +1 more", the TUI's `cancelled_instances_line`. */
export function cancelledInstancesLine(instances: readonly string[]): string {
  const shown = instances.slice(0, 3).map(shortInstant).join(", ");
  const rest = instances.length - 3;
  const n = instances.length;
  return `${n} ${n === 1 ? "occurrence" : "occurrences"} cancelled: ${shown}${rest > 0 ? `, +${rest} more` : ""}`;
}

export type EventCardProps = {
  event: EventFrontmatter;
  /** The user organizes it (an agenda row's `is_organizer`, a Sent copy): no own reply is shown. */
  organizer?: boolean;
  /** The region's name; the agenda's card is "Event", the reader's invitation card names itself. */
  label?: string;
  /** What follows the details, such as the reader's reply buttons. */
  children?: ReactNode;
};

/**
 * One calendar event, the TUI's shared event card (`event_card_lines`): the
 * summary, a cancellation or supersession first since it changes what every
 * line below means, the time, recurrence, place, organizer, the user's own
 * reply and every attendee's. The agenda shows it for its cursor row, and
 * the reader's invitation card reuses it.
 */
export function EventCard({ event, organizer = false, label = "Event", children }: EventCardProps) {
  const summary = event.summary?.trim() || "(no summary)";
  const when = eventWhen(event.start, event.end);
  const cancelledInstances = event.cancelled_instances ?? [];
  const attendees = event.attendees ?? [];
  const attendeesId = useId();
  const rows: [string, ReactNode][] = [];
  if (when) rows.push(["When", when]);
  if (event.recurrence) rows.push(["Repeats", event.recurrence]);
  if (event.location) rows.push(["Where", event.location]);
  if (event.organizer) rows.push(["Organizer", event.organizer]);
  if (!organizer) {
    rows.push([
      "Your RSVP",
      <span key="rsvp" data-slot="event-rsvp" className={STATUS_TONE[event.rsvp] ?? "text-muted-foreground"}>
        {humanStatus(event.rsvp)}
      </span>,
    ]);
  }
  return (
    <section aria-label={label} data-slot="event-card" className="flex flex-col gap-2 text-sm">
      <h3 className={`text-base font-semibold ${event.cancelled ? "text-muted-foreground line-through" : ""}`}>{summary}</h3>
      {event.cancelled ? (
        <p data-slot="event-state" className="font-medium text-destructive">
          Cancelled by the organizer.
        </p>
      ) : event.superseded ? (
        <p data-slot="event-state" className="font-medium text-warning">
          Superseded: a newer version of this invitation has arrived.
        </p>
      ) : null}
      {!event.cancelled && cancelledInstances.length > 0 ? (
        <p data-slot="event-cancelled-instances" className="text-warning">
          {cancelledInstancesLine(cancelledInstances)}
        </p>
      ) : null}
      <dl className="grid grid-cols-[max-content_minmax(0,1fr)] gap-x-3 gap-y-1">
        {rows.map(([term, value]) => (
          <div key={term} className="contents">
            <dt className="text-muted-foreground">{term}</dt>
            <dd className="min-w-0 break-words">{value}</dd>
          </div>
        ))}
      </dl>
      {attendees.length > 0 ? (
        <div className="flex flex-col gap-1">
          <h4 id={attendeesId} className="text-muted-foreground">
            Attendees
          </h4>
          <ul aria-labelledby={attendeesId} className="flex flex-col gap-0.5">
            {attendees.map((a) => (
              <li key={a.address} data-slot="event-attendee" className="flex min-w-0 justify-between gap-3">
                <span className="truncate">{a.address}</span>
                <span className={`shrink-0 ${STATUS_TONE[a.status] ?? "text-muted-foreground"}`}>{humanStatus(a.status)}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {children}
    </section>
  );
}
