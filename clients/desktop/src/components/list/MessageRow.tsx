import { memo } from "react";
import { CalendarDays, CornerUpLeft, Paperclip, Star } from "lucide-react";
import type { MessageListRow } from "@/protocol/types";
import { senderName, shortDate } from "@/components/list/format";
import { ROW_HEIGHT } from "@/components/list/useWindow";

export type MessageRowProps = {
  row: MessageListRow;
  selected: boolean;
  /** The roving tab stop: the selected row, or the first when none is. */
  tabStop: boolean;
  onSelect: (row: MessageListRow) => void;
  onOpen: (row: MessageListRow) => void;
};

/** One message in the list. Pure: it renders its props and nothing else. */
export const MessageRow = memo(function MessageRow({ row, selected, tabStop, onSelect, onOpen }: MessageRowProps) {
  const unread = !row.flags.seen;
  const subject = row.subject || "(no subject)";
  const sender = senderName(row.from);
  const date = shortDate(row.date_sort);
  const state = [
    unread ? "unread" : null,
    row.flags.flagged ? "flagged" : null,
    row.flags.answered ? "answered" : null,
    row.has_attachments ? "has attachments" : null,
    row.is_invite ? "invitation" : null,
  ].filter(Boolean);
  return (
    <div
      role="option"
      id={`msg-${row.id}`}
      aria-selected={selected}
      aria-label={`${sender}, ${subject}, ${row.date_display || "no date"}${state.length ? `, ${state.join(", ")}` : ""}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-row-id={row.id}
      style={{ height: ROW_HEIGHT }}
      onClick={() => onSelect(row)}
      onDoubleClick={() => onOpen(row)}
      className={`flex cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        selected ? "bg-selection text-selection-foreground" : "hover:bg-accent/60"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        <span aria-hidden="true" className={`size-2 shrink-0 rounded-full ${unread ? "bg-link" : "bg-transparent"}`} />
        <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold" : ""}`}>{sender}</span>
        <span aria-hidden="true" className="flex shrink-0 items-center gap-1 text-muted-foreground">
          {row.is_invite ? <CalendarDays className="size-3.5" /> : null}
          {row.flags.answered ? <CornerUpLeft className="size-3.5" /> : null}
          {row.has_attachments ? <Paperclip className="size-3.5" /> : null}
          {row.flags.flagged ? <Star className="size-3.5 text-warning" /> : null}
        </span>
        <time className="shrink-0 text-xs text-muted-foreground tabular-nums" dateTime={row.date_sort}>
          {date}
        </time>
      </div>
      <div className={`truncate pl-4 ${unread ? "text-foreground" : "text-muted-foreground"} ${selected ? "text-selection-foreground" : ""}`}>
        {subject}
      </div>
    </div>
  );
});
