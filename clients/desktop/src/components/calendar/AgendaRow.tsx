import { memo } from "react";
import { Badge } from "@/components/ui/badge";
import { eventTitle, statusBadge } from "@/app/calendar";
import type { AgendaEvent } from "@/protocol/types";

const BADGE_TONE: Record<ReturnType<typeof statusBadge>, string> = {
  cancelled: "bg-destructive/10 text-destructive",
  organizer: "bg-secondary text-secondary-foreground",
  accepted: "bg-link/10 text-link",
  declined: "bg-destructive/10 text-destructive",
  tentative: "bg-warning/15 text-warning",
  "no reply": "bg-secondary text-muted-foreground",
};

export type AgendaRowProps = {
  event: AgendaEvent;
  cursor: boolean;
  position: number;
  setSize: number;
  onSelect: (rowId: number) => void;
  onOpen: (rowId: number) => void;
};

/**
 * One agenda entry, the TUI's agenda row: the start as the daemon displays
 * it in local time ("undated" when it has none), the summary or else the
 * subject, and one badge: cancelled, organizer, or the user's own reply.
 */
export const AgendaRow = memo(function AgendaRow({ event, cursor, position, setSize, onSelect, onOpen }: AgendaRowProps) {
  const when = event.start_display || "undated";
  const title = eventTitle(event);
  const badge = statusBadge(event);
  return (
    <div
      role="option"
      id={`agenda-${event.row_id}`}
      aria-selected={cursor}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={`${when}, ${title}, ${badge}`}
      tabIndex={cursor ? 0 : -1}
      data-roving={cursor ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-row-id={event.row_id}
      data-cancelled={event.cancelled || undefined}
      onClick={() => onSelect(event.row_id)}
      onDoubleClick={() => onOpen(event.row_id)}
      className={`flex cursor-default items-center gap-3 border-b border-border px-3 py-2 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : "hover:bg-accent/60"
      }`}
    >
      <time className="w-32 shrink-0 text-xs tabular-nums text-muted-foreground" dateTime={event.start_sort || undefined}>
        {when}
      </time>
      <span className={`min-w-0 flex-1 truncate ${event.cancelled ? "line-through opacity-70" : ""}`}>{title}</span>
      <Badge data-slot="agenda-badge" className={BADGE_TONE[badge]}>
        {badge}
      </Badge>
    </div>
  );
});
