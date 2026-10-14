import { memo, type MouseEvent } from "react";
import { CalendarDays, ChevronRight, CornerUpLeft, Paperclip, Pencil } from "lucide-react";
import type { ConversationRow as Conversation } from "@/protocol/types";
import { senderName, shortDate } from "@/components/list/format";
import { FlagToggle, MarkBox, PendingMark, UnreadToggle } from "@/components/list/RowControls";
import { ROW_HEIGHT } from "@/components/list/useWindow";

export type ConversationRowProps = {
  row: Conversation;
  /** Its members show under it. */
  expanded: boolean;
  /** It has more than one member, so it expands at all. */
  expandable: boolean;
  /** The cursor: the row the reader shows (its `latest`). */
  cursor: boolean;
  marked: boolean;
  /** Any row of the list is marked; `aria-selected` then names the marks. */
  anyMarked: boolean;
  /** A change to one of its members here the daemon has not confirmed yet. */
  pending: boolean;
  tabStop: boolean;
  /** 1-based, with the visible sequence's length, members included. */
  position: number;
  setSize: number;
  onSelect: (threadId: string) => void;
  onOpen: (threadId: string) => void;
  onMark: (threadId: string, range: boolean) => void;
  onToggleExpand: (threadId: string) => void;
  onToggleFlag: (row: Conversation) => void;
  onToggleRead: (row: Conversation) => void;
};

/** The participants as names, newest first, as the row prints them. */
export function participantNames(row: Conversation): string {
  const names = row.participants.map(senderName);
  return names.length > 0 ? names.join(", ") : senderName(row.latest.from);
}

/**
 * One conversation of a mailbox (ticket 0142): a disclosure chevron, the
 * participants, the count, the unread count, the badges a message row draws
 * and a draft marker, then the subject of its newest message here. A
 * conversation of one message reads as a message row does. Pure.
 */
export const ConversationRow = memo(function ConversationRow({
  row,
  expanded,
  expandable,
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
  onToggleExpand,
  onToggleFlag,
  onToggleRead,
}: ConversationRowProps) {
  const latest = row.latest;
  const unread = row.unread > 0;
  const subject = latest.subject || "(no subject)";
  const who = participantNames(row);
  const date = shortDate(row.date_sort);
  const state = [
    row.count > 1 ? `${row.count} messages` : null,
    row.count > 1 && unread ? `${row.unread} unread` : unread ? "unread" : null,
    expandable ? (expanded ? "expanded" : "collapsed") : null,
    marked ? "marked" : null,
    pending ? "change pending" : null,
    row.flagged ? "flagged" : null,
    row.has_draft ? "has a draft" : null,
    latest.flags.answered ? "answered" : null,
    row.has_attachments ? "has attachments" : null,
    latest.is_invite ? "invitation" : null,
  ].filter(Boolean);
  const onClick = (e: MouseEvent) => {
    if (e.shiftKey) onMark(row.thread_id, true);
    else if (e.metaKey || e.ctrlKey) onMark(row.thread_id, false);
    else onSelect(row.thread_id);
  };
  return (
    <div
      role="option"
      id={`conv-${row.thread_id}`}
      aria-selected={anyMarked ? marked : cursor}
      aria-busy={pending || undefined}
      aria-expanded={expandable ? expanded : undefined}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={`${who}, ${subject}, ${row.date_display || "no date"}${state.length ? `, ${state.join(", ")}` : ""}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-marked={marked || undefined}
      data-pending={pending || undefined}
      data-thread-id={row.thread_id}
      data-row-id={latest.id}
      style={{ height: ROW_HEIGHT }}
      onClick={onClick}
      onDoubleClick={() => onOpen(row.thread_id)}
      className={`group flex cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : marked ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        <MarkBox marked={marked} anyMarked={anyMarked} onToggle={(range) => onMark(row.thread_id, range)} />
        <UnreadToggle unread={unread} onToggle={() => onToggleRead(row)} />
        <span className={`flex min-w-0 flex-1 items-center gap-1 ${unread ? "font-semibold" : ""}`}>
          {expandable ? (
            <button
              type="button"
              tabIndex={-1}
              aria-label={expanded ? "Collapse conversation (h)" : "Expand conversation (l)"}
              aria-expanded={expanded}
              title={expanded ? "Collapse (h)" : "Expand (l)"}
              data-slot="disclosure"
              onClick={(e) => {
                e.stopPropagation();
                onToggleExpand(row.thread_id);
              }}
              onDoubleClick={(e) => e.stopPropagation()}
              className="inline-flex size-4 shrink-0 cursor-pointer items-center justify-center rounded-sm text-muted-foreground outline-none focus-visible:outline-2 focus-visible:outline-ring"
            >
              <ChevronRight aria-hidden="true" className={`size-3.5 transition-transform ${expanded ? "rotate-90" : ""}`} />
            </button>
          ) : null}
          <span className="min-w-0 truncate">{who}</span>
          {row.count > 1 ? (
            <span data-slot="conversation-count" className="shrink-0 text-xs text-muted-foreground tabular-nums">
              ({row.count})
            </span>
          ) : null}
        </span>
        <span className="flex shrink-0 items-center gap-1 text-muted-foreground">
          {pending ? <PendingMark /> : null}
          {row.count > 1 && unread ? (
            <span data-slot="conversation-unread" aria-hidden="true" className="rounded-sm bg-link px-1 text-[10px] leading-4 font-semibold text-background tabular-nums">
              {row.unread}
            </span>
          ) : null}
          {row.has_draft ? <Pencil aria-hidden="true" data-slot="conversation-draft" className="size-3.5" /> : null}
          {latest.is_invite ? <CalendarDays aria-hidden="true" className="size-3.5" /> : null}
          {latest.flags.answered ? <CornerUpLeft aria-hidden="true" className="size-3.5" /> : null}
          {row.has_attachments ? <Paperclip aria-hidden="true" className="size-3.5" /> : null}
          <FlagToggle flagged={row.flagged} onToggle={() => onToggleFlag(row)} />
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
