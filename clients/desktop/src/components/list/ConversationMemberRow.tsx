import { memo, type MouseEvent } from "react";
import { CornerUpLeft, Pencil, Star } from "lucide-react";
import type { ThreadMessage } from "@/protocol/types";
import { senderAddress, senderName, shortDate } from "@/components/list/format";
import { MarkBox, PendingMark } from "@/components/list/RowControls";
import { ROW_HEIGHT } from "@/components/list/useWindow";

export type ConversationMemberRowProps = {
  threadId: string;
  member: ThreadMessage;
  /** Its key in a conversation cursor (`memberKey`). */
  memberKey: string;
  /** The sidebar label of the mailbox it lives in, `Drafts` for a draft. */
  mailboxLabel: string;
  /** Whether it is the last member of its conversation, where the group ends. */
  last: boolean;
  cursor: boolean;
  marked: boolean;
  anyMarked: boolean;
  pending: boolean;
  tabStop: boolean;
  position: number;
  setSize: number;
  onSelect: (threadId: string, member: string) => void;
  onOpen: (threadId: string, member: string) => void;
  onMark: (member: ThreadMessage, range: boolean) => void;
};

/**
 * One member under an expanded conversation row (ticket 0142), indented: the
 * mailbox it lives in where a message row has its sender, then the sender,
 * the markers and the date. Every action on it applies to it alone, in its
 * own mailbox. Pure.
 *
 * A member the daemon calls `own` (PERSO-179), from the account's own address
 * whatever mailbox it is in or held in Sent, reads apart from the others: its
 * mailbox chip takes the ocean tint and its sender reads "Me", with the
 * address it went out from on the second line.
 *
 * The members sit indented on a muted tint, lighter than their open header's,
 * with the ocean rail the header starts down their left edge; the last one
 * closes the group with a framing-tinted bottom rule (PERSO-179), so the
 * conversation reads as one block and the row after it as the next one.
 */
export const ConversationMemberRow = memo(function ConversationMemberRow({
  threadId,
  member,
  memberKey,
  mailboxLabel,
  last,
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
}: ConversationMemberRowProps) {
  const draft = member.draft_id ?? null;
  const unread = draft === null && !member.flags.seen;
  const own = member.own;
  const sender = draft !== null ? "Draft" : own ? "Me" : senderName(member.from);
  const detail = draft !== null ? "Draft reply" : own ? senderAddress(member.from) : member.from || "(no sender)";
  const date = shortDate(member.date_display);
  const state = [
    marked ? "marked" : null,
    pending ? "change pending" : null,
    unread ? "unread" : null,
    member.flags.flagged ? "flagged" : null,
    member.flags.answered ? "answered" : null,
    member.current ? "the listed message" : null,
  ].filter(Boolean);
  const onClick = (e: MouseEvent) => {
    if (e.shiftKey) onMark(member, true);
    else if (e.metaKey || e.ctrlKey) onMark(member, false);
    else onSelect(threadId, memberKey);
  };
  return (
    <div
      role="option"
      aria-selected={anyMarked ? marked : cursor}
      aria-busy={pending || undefined}
      aria-level={2}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={`${mailboxLabel}: ${sender}, ${member.date_display || "no date"}${state.length ? `, ${state.join(", ")}` : ""}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-marked={marked || undefined}
      data-member={memberKey}
      data-own={own || undefined}
      data-last={last || undefined}
      data-row-id={draft === null ? member.id : undefined}
      data-draft-id={draft ?? undefined}
      style={{ height: ROW_HEIGHT }}
      onClick={onClick}
      onDoubleClick={() => onOpen(threadId, memberKey)}
      className={`group flex cursor-default flex-col justify-center gap-0.5 border-b py-0 pr-3 pl-12 text-sm shadow-[inset_2px_0_0_var(--color-framing)] outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        last ? "border-framing/40" : "border-border"
      } ${cursor ? "bg-selection text-selection-foreground" : marked ? "bg-accent text-accent-foreground" : "bg-muted/50 hover:bg-accent/60"}`}
    >
      <div className="flex min-w-0 items-center gap-2">
        <MarkBox marked={marked} anyMarked={anyMarked} onToggle={(range) => onMark(member, range)} />
        <span
          data-slot="member-mailbox"
          className={`shrink-0 rounded-sm border px-1 text-xs leading-4 ${
            own
              ? `border-framing/60 bg-link/10 ${cursor ? "text-selection-foreground" : "text-link"}`
              : "border-border text-muted-foreground"
          }`}
        >
          {mailboxLabel}
        </span>
        <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold" : ""}`}>{sender}</span>
        <span className="flex shrink-0 items-center gap-1 text-muted-foreground">
          {pending ? <PendingMark /> : null}
          {draft !== null ? <Pencil aria-hidden="true" className="size-3.5" /> : null}
          {member.flags.answered ? <CornerUpLeft aria-hidden="true" className="size-3.5" /> : null}
          {member.flags.flagged ? <Star aria-hidden="true" className="size-3.5 fill-current text-warning" /> : null}
        </span>
        <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{date}</span>
      </div>
      <div className={`truncate pl-6 ${cursor ? "text-selection-foreground" : "text-muted-foreground"}`}>
        {detail}
      </div>
    </div>
  );
});
