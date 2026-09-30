import { memo, type MouseEvent } from "react";
import { CalendarDays, Cloud, Paperclip, Star } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { senderName, shortDate } from "@/components/list/format";
import { MarkBox, PendingMark } from "@/components/list/RowControls";
import type { SearchHit } from "@/app/state";

export type SearchHitRowProps = {
  hit: SearchHit;
  /** The mailbox as the sidebar names it. */
  mailboxLabel: string;
  /** The cursor: the hit the reader shows. */
  cursor: boolean;
  marked: boolean;
  /** Any hit is marked; `aria-selected` then names the marks. */
  anyMarked: boolean;
  pending: boolean;
  tabStop: boolean;
  /** 1-based, with the number of hits. */
  position: number;
  setSize: number;
  onSelect: (hit: SearchHit) => void;
  onOpen: (hit: SearchHit) => void;
  onMark: (hit: SearchHit, range: boolean) => void;
};

/**
 * One search result, with the mailbox it was found in as a badge. A hit the
 * store has never ingested (server-only) is listed but cannot open: fetching
 * it into the store is a mutation, which the read-only milestone does not do.
 */
export const SearchHitRow = memo(function SearchHitRow({
  hit,
  mailboxLabel,
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
}: SearchHitRowProps) {
  const openable = hit.row_id !== null && hit.selector !== null;
  const unread = !hit.flags.seen;
  const subject = hit.subject || "(no subject)";
  const sender = senderName(hit.from);
  const onClick = (e: MouseEvent) => {
    if (!openable) return;
    if (e.shiftKey) onMark(hit, true);
    else if (e.metaKey || e.ctrlKey) onMark(hit, false);
    else onSelect(hit);
  };
  const state = [
    `in ${mailboxLabel}`,
    marked ? "marked" : null,
    pending ? "change pending" : null,
    openable ? null : "on the server only",
    unread ? "unread" : null,
    hit.flags.flagged ? "flagged" : null,
    hit.has_attachments ? "has attachments" : null,
  ].filter(Boolean);
  return (
    <div
      role="option"
      aria-selected={anyMarked ? marked : cursor}
      aria-busy={pending || undefined}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-disabled={openable ? undefined : true}
      aria-label={`${sender}, ${subject}, ${hit.date_display || "no date"}, ${state.join(", ")}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-hit={hit.key}
      data-cursor={cursor || undefined}
      data-marked={marked || undefined}
      data-row-id={hit.row_id ?? undefined}
      onClick={onClick}
      onDoubleClick={() => openable && onOpen(hit)}
      className={`group flex h-16 cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor
          ? "bg-selection text-selection-foreground"
          : marked
            ? "bg-accent text-accent-foreground"
            : openable
              ? "hover:bg-accent/60"
              : "text-muted-foreground"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        {openable ? (
          <MarkBox marked={marked} anyMarked={anyMarked} onToggle={(range) => onMark(hit, range)} />
        ) : (
          <span aria-hidden="true" className="size-4 shrink-0" />
        )}
        <span aria-hidden="true" className={`size-2 shrink-0 rounded-full ${unread ? "bg-link" : "bg-transparent"}`} />
        <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold" : ""}`}>{sender}</span>
        <span aria-hidden="true" className="flex shrink-0 items-center gap-1 text-muted-foreground">
          {pending ? <PendingMark /> : null}
          {hit.is_invite ? <CalendarDays className="size-3.5" /> : null}
          {hit.has_attachments ? <Paperclip className="size-3.5" /> : null}
          {hit.flags.flagged ? <Star className="size-3.5 text-warning" /> : null}
        </span>
        <time className="shrink-0 text-xs text-muted-foreground tabular-nums" dateTime={hit.date_sort}>
          {shortDate(hit.date_sort)}
        </time>
      </div>
      <div className="flex min-w-0 items-center gap-2 pl-10">
        <span className={`min-w-0 flex-1 truncate ${cursor ? "text-selection-foreground" : "text-muted-foreground"}`}>
          {subject}
        </span>
        {openable ? null : (
          <Badge variant="outline" data-slot="server-only-badge">
            <Cloud aria-hidden="true" />
            server only
          </Badge>
        )}
        <Badge variant="secondary" data-slot="mailbox-badge">
          {mailboxLabel}
        </Badge>
      </div>
    </div>
  );
});
