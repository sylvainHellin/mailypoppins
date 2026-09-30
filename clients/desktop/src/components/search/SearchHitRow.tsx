import { memo } from "react";
import { CalendarDays, Cloud, Paperclip, Star } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { senderName, shortDate } from "@/components/list/format";
import type { SearchHit } from "@/app/state";

export type SearchHitRowProps = {
  hit: SearchHit;
  /** The mailbox as the sidebar names it. */
  mailboxLabel: string;
  selected: boolean;
  tabStop: boolean;
  /** 1-based, with the number of hits. */
  position: number;
  setSize: number;
  onSelect: (hit: SearchHit) => void;
  onOpen: (hit: SearchHit) => void;
};

/**
 * One search result, with the mailbox it was found in as a badge. A hit the
 * store has never ingested (server-only) is listed but cannot open: fetching
 * it into the store is a mutation, which the read-only milestone does not do.
 */
export const SearchHitRow = memo(function SearchHitRow({
  hit,
  mailboxLabel,
  selected,
  tabStop,
  position,
  setSize,
  onSelect,
  onOpen,
}: SearchHitRowProps) {
  const openable = hit.row_id !== null && hit.selector !== null;
  const unread = !hit.flags.seen;
  const subject = hit.subject || "(no subject)";
  const sender = senderName(hit.from);
  const state = [
    `in ${mailboxLabel}`,
    openable ? null : "on the server only",
    unread ? "unread" : null,
    hit.flags.flagged ? "flagged" : null,
    hit.has_attachments ? "has attachments" : null,
  ].filter(Boolean);
  return (
    <div
      role="option"
      aria-selected={selected}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-disabled={openable ? undefined : true}
      aria-label={`${sender}, ${subject}, ${hit.date_display || "no date"}, ${state.join(", ")}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-hit={hit.key}
      data-row-id={hit.row_id ?? undefined}
      onClick={() => openable && onSelect(hit)}
      onDoubleClick={() => openable && onOpen(hit)}
      className={`flex h-16 cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        selected ? "bg-selection text-selection-foreground" : openable ? "hover:bg-accent/60" : "text-muted-foreground"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        <span aria-hidden="true" className={`size-2 shrink-0 rounded-full ${unread ? "bg-link" : "bg-transparent"}`} />
        <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold" : ""}`}>{sender}</span>
        <span aria-hidden="true" className="flex shrink-0 items-center gap-1 text-muted-foreground">
          {hit.is_invite ? <CalendarDays className="size-3.5" /> : null}
          {hit.has_attachments ? <Paperclip className="size-3.5" /> : null}
          {hit.flags.flagged ? <Star className="size-3.5 text-warning" /> : null}
        </span>
        <time className="shrink-0 text-xs text-muted-foreground tabular-nums" dateTime={hit.date_sort}>
          {shortDate(hit.date_sort)}
        </time>
      </div>
      <div className="flex min-w-0 items-center gap-2 pl-4">
        <span className={`min-w-0 flex-1 truncate ${selected ? "text-selection-foreground" : "text-muted-foreground"}`}>
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
