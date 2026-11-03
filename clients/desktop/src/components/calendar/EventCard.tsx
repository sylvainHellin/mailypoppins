import { useId, type ReactNode } from "react";
import { ExternalLink, Mail } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { CalendarEvent, CalendarInfo, EventFrontmatter } from "@/protocol/types";
import { addDays, calendarTag, dayLabel, dayOf, eventTitle, isOrganizer, viewerZone } from "@/app/calendar";

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

const hhmm = (at: Date) => `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;

/**
 * A neutral event's time in the viewer's zone: `Wed 14 Oct 2099, 09:00 to
 * 10:30`, the end's date only when it is another day, the event's own zone
 * named when it is not the viewer's; an all-day event's dates, the
 * exclusive end made inclusive.
 */
export function neutralWhen(e: CalendarEvent, zone: string | null = viewerZone()): string {
  const t = e.time;
  if (t.kind === "all-day") {
    const last = t.end > t.start ? addDays(t.end, -1) : t.start;
    return last === t.start ? `${dayLabel(t.start)}, all day` : `${dayLabel(t.start)} to ${dayLabel(last)}, all day`;
  }
  const start = new Date(t.start);
  const end = new Date(t.end);
  const from = `${dayLabel(dayOf(start))}, ${hhmm(start)}`;
  const to = dayOf(end) === dayOf(start) ? hhmm(end) : `${dayLabel(dayOf(end))}, ${hhmm(end)}`;
  const own = t.zone && zone && t.zone !== zone ? ` (the event's zone: ${t.zone})` : "";
  return `${from} to ${to}${own}`;
}

const person = (p: { name?: string; email: string }) => (p.name ? `${p.name} <${p.email}>` : p.email);

export type CalendarEventCardProps = {
  event: CalendarEvent;
  /** The listed calendars, which name the event's. */
  calendars: readonly CalendarInfo[] | null;
  /** The element id Enter focuses. */
  id?: string;
  /** The invitation mail behind an `imip:` event, in the Mail view. */
  onOpenMail?: () => void;
  /** The online meeting in the browser. */
  onJoin?: (url: string) => void;
  /** What follows the details, such as the respond button. */
  children?: ReactNode;
};

/**
 * One event of the server calendar or of an invitation mail (PERSO-200),
 * the TUI's event card: the summary, a cancellation first, when with its
 * zone, the series, where, the calendar, the organizer, the user's own
 * answer, the join link, the description and every attendee's status; an
 * event that came from an invitation mail offers that mail.
 */
export function CalendarEventCard({ event, calendars, id, onOpenMail, onJoin, children }: CalendarEventCardProps) {
  const attendeesId = useId();
  const rows: [string, ReactNode][] = [["When", neutralWhen(event)]];
  if (event.recurrence) rows.push(["Repeats", event.recurrence]);
  else if (event.recurrence_id) rows.push(["Repeats", "An occurrence of a series"]);
  if (event.location) rows.push(["Where", event.location]);
  rows.push(["Calendar", calendarTag(calendars, event)]);
  if (event.organizer) rows.push(["Organizer", person(event.organizer)]);
  if (!isOrganizer(event) && event.own_response !== "none") {
    rows.push([
      "Your response",
      <span key="response" data-slot="event-rsvp" className={STATUS_TONE[event.own_response] ?? "text-muted-foreground"}>
        {humanStatus(event.own_response)}
      </span>,
    ]);
  }
  const url = event.online_meeting_url;
  return (
    <section id={id} tabIndex={-1} aria-label="Event" data-slot="event-card" className="flex flex-col gap-2 text-sm outline-none">
      <h3 className={`text-base font-semibold ${event.cancelled ? "text-muted-foreground line-through" : ""}`}>{eventTitle(event)}</h3>
      {event.cancelled ? (
        <p data-slot="event-state" className="font-medium text-destructive">
          Cancelled by the organizer.
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
      {url || event.source ? (
        <div className="flex flex-wrap gap-1.5">
          {url ? (
            <Button size="sm" variant="outline" onClick={() => onJoin?.(url)} title={url}>
              <ExternalLink aria-hidden="true" />
              Join the meeting
            </Button>
          ) : null}
          {event.source ? (
            <Button size="sm" variant="outline" onClick={onOpenMail} title="Open the invitation mail (m)">
              <Mail aria-hidden="true" />
              Open mail
            </Button>
          ) : null}
        </div>
      ) : null}
      {event.body?.trim() ? (
        <p data-slot="event-body" className="whitespace-pre-wrap break-words text-muted-foreground">
          {event.body.trim()}
        </p>
      ) : null}
      {event.attendees.length > 0 ? (
        <div className="flex flex-col gap-1">
          <h4 id={attendeesId} className="text-muted-foreground">
            Attendees
          </h4>
          <ul aria-labelledby={attendeesId} className="flex flex-col gap-0.5">
            {event.attendees.map((a) => (
              <li key={a.email} data-slot="event-attendee" className="flex min-w-0 justify-between gap-3">
                <span className="truncate">
                  {person(a)}
                  {a.role === "optional" ? <span className="text-muted-foreground"> (optional)</span> : null}
                </span>
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
