import { memo, type MouseEvent } from "react";
import { CalendarDays, CornerUpLeft, Paperclip } from "lucide-react";
import type { MessageListRow } from "@/protocol/types";
import { senderName, shortDate } from "@/components/list/format";
import { FlagToggle, MarkBox, PendingMark, UnreadToggle } from "@/components/list/RowControls";
import { ROW_HEIGHT } from "@/components/list/useWindow";

export type MessageRowProps = {
  row: MessageListRow;
  /** The cursor: the row the reader shows. */
  cursor: boolean;
  /** In the multi-select. */
  marked: boolean;
  /** Any row of the list is marked; `aria-selected` then names the marks. */
  anyMarked: boolean;
  /** A change to the row the daemon has not confirmed yet. */
  pending: boolean;
  /** The roving tab stop: the selected row, or the first when none is. */
  tabStop: boolean;
  /** 1-based, with the list's length: the set is complete only unwindowed. */
  position: number;
  setSize: number;
  onSelect: (row: MessageListRow) => void;
  onOpen: (row: MessageListRow) => void;
  /** Cmd/Ctrl+click or the box toggles the mark; Shift marks the range. */
  onMark: (row: MessageListRow, range: boolean) => void;
  onToggleFlag: (row: MessageListRow) => void;
  onToggleRead: (row: MessageListRow) => void;
};

/** One message in the list. Pure: it renders its props and nothing else. */
export const MessageRow = memo(function MessageRow({
  row,
  cursor,
  marked,
  anyMarked,
  pending,
  tabStop,
  position,
  setSize,
  onSelect,
  onOpen,
  onMark,
  onToggleFlag,
  onToggleRead,
}: MessageRowProps) {
  const unread = !row.flags.seen;
  const subject = row.subject || "(no subject)";
  const sender = senderName(row.from);
  const date = shortDate(row.date_sort);
  const state = [
    marked ? "marked" : null,
    pending ? "change pending" : null,
    unread ? "unread" : null,
    row.flags.flagged ? "flagged" : null,
    row.flags.answered ? "answered" : null,
    row.has_attachments ? "has attachments" : null,
    row.is_invite ? "invitation" : null,
  ].filter(Boolean);
  const onClick = (e: MouseEvent) => {
    if (e.shiftKey) onMark(row, true);
    else if (e.metaKey || e.ctrlKey) onMark(row, false);
    else onSelect(row);
  };
  return (
    <div
      role="option"
      id={`msg-${row.id}`}
      aria-selected={anyMarked ? marked : cursor}
      aria-busy={pending || undefined}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={`${sender}, ${subject}, ${row.date_display || "no date"}${state.length ? `, ${state.join(", ")}` : ""}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-marked={marked || undefined}
      data-pending={pending || undefined}
      data-row-id={row.id}
      style={{ height: ROW_HEIGHT }}
      onClick={onClick}
      onDoubleClick={() => onOpen(row)}
      className={`group flex cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : marked ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        <MarkBox marked={marked} anyMarked={anyMarked} onToggle={(range) => onMark(row, range)} />
        <UnreadToggle unread={unread} onToggle={() => onToggleRead(row)} />
        <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold" : ""}`}>{sender}</span>
        <span className="flex shrink-0 items-center gap-1 text-muted-foreground">
          {pending ? <PendingMark /> : null}
          {row.is_invite ? <CalendarDays aria-hidden="true" className="size-3.5" /> : null}
          {row.flags.answered ? <CornerUpLeft aria-hidden="true" className="size-3.5" /> : null}
          {row.has_attachments ? <Paperclip aria-hidden="true" className="size-3.5" /> : null}
          <FlagToggle flagged={row.flags.flagged} onToggle={() => onToggleFlag(row)} />
        </span>
        <time className="shrink-0 text-xs text-muted-foreground tabular-nums" dateTime={row.date_sort}>
          {date}
        </time>
      </div>
      <div
        className={`truncate pl-12 ${unread ? "text-foreground" : "text-muted-foreground"} ${cursor ? "text-selection-foreground" : ""}`}
      >
        {subject}
      </div>
    </div>
  );
});
