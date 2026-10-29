import { memo } from "react";
import { Badge } from "@/components/ui/badge";
import { eventTitle, partLabel, statusBadge, type CalendarEntry, type StatusBadge } from "@/app/calendar";

export const BADGE_TONE: Record<StatusBadge, string> = {
  cancelled: "bg-destructive/10 text-destructive",
  organizer: "bg-secondary text-secondary-foreground",
  accepted: "bg-link/10 text-link",
  declined: "bg-destructive/10 text-destructive",
  tentative: "bg-warning/15 text-warning",
  "no reply": "bg-secondary text-muted-foreground",
};

export type AgendaRowProps = {
  entry: CalendarEntry;
  /** The account and calendar, `work/Invitations`. */
  tag: string;
  /** The calendar's colour, if the server names one. */
  color: string | null;
  cursor: boolean;
  position: number;
  setSize: number;
  onSelect: (key: string) => void;
  onOpen: (key: string) => void;
};

/**
 * One agenda entry, the TUI's agenda row: the part of the day the event
 * covers (`all day`, `09:00-10:30`), its title, the account and calendar it
 * sits in, and one badge: cancelled, organizer, or the user's own answer.
 */
export const AgendaRow = memo(function AgendaRow({ entry, tag, color, cursor, position, setSize, onSelect, onOpen }: AgendaRowProps) {
  const { event } = entry;
  const when = partLabel(entry.part);
  const title = eventTitle(event);
  const badge = statusBadge(event);
  return (
    <div
      role="option"
      aria-selected={cursor}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={[when, title, tag, badge].filter(Boolean).join(", ")}
      tabIndex={cursor ? 0 : -1}
      data-roving={cursor ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-key={entry.key}
      data-day={entry.day}
      data-event-id={event.id}
      data-cancelled={event.cancelled || undefined}
      onClick={() => onSelect(entry.key)}
      onDoubleClick={() => onOpen(entry.key)}
      className={`flex cursor-default items-center gap-3 border-b border-border px-3 py-2 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : "hover:bg-accent/60"
      }`}
    >
      <span aria-hidden="true" className="h-4 w-1 shrink-0 rounded-full bg-muted-foreground/40" style={color ? { backgroundColor: color } : undefined} />
      <time className="w-24 shrink-0 text-xs tabular-nums text-muted-foreground">{when}</time>
      <span className={`min-w-0 flex-1 truncate ${event.cancelled ? "line-through opacity-70" : ""}`}>{title}</span>
      <span data-slot="agenda-tag" className="hidden shrink-0 truncate text-xs text-muted-foreground sm:inline">
        {tag}
      </span>
      {badge ? (
        <Badge data-slot="agenda-badge" className={BADGE_TONE[badge]}>
          {badge}
        </Badge>
      ) : null}
    </div>
  );
});
