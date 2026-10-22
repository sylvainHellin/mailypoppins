import { ChevronRight } from "lucide-react";
import { SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarMenu } from "@/components/ui/sidebar";
import { Skeleton } from "@/components/ui/skeleton";
import { MailboxItem } from "@/components/sidebar/MailboxItem";
import { OutboxState } from "@/components/sidebar/OutboxState";
import { SyncHealthBadge } from "@/components/sidebar/SyncHealthBadge";
import type { MailboxKind } from "@/lib/gui-types";
import type { AccountState, SyncHealthState } from "@/protocol/types";
import type { OutboxSummary } from "@/app/outbox";

export type SidebarMailbox = {
  slug: string;
  label: string;
  kind: MailboxKind;
  unread: number;
  total: number;
};

export type AccountGroupProps = {
  name: string;
  runtime: AccountState;
  health: SyncHealthState;
  outbox: OutboxSummary;
  /** The outbox view shows this account's outbox. */
  outboxOpen: boolean;
  /** The outbox view shows some account's outbox, so no mailbox is the current page. */
  outboxShown: boolean;
  /** Mail is the view shown, so the selected mailbox or the open outbox is the current page. */
  mailShown: boolean;
  mailboxes: SidebarMailbox[] | null;
  /** The inbox's unread count, shown on the header while the account is collapsed. */
  inboxUnread: number | null;
  /** Whether the mailboxes show; a collapsed account draws its header and outbox line only (ticket 0147). */
  expanded: boolean;
  onToggle: (account: string) => void;
  selected: { account: string | null; mailbox: string | null };
  cursor: { account: string; slug: string } | null;
  /** Whether the digit keys address this account's mailboxes. */
  digits: boolean;
  onSelect: (account: string, slug: string) => void;
  onOpenOutbox: (account: string) => void;
};

/**
 * One account in the sidebar. Its header is a button that expands or
 * collapses the account without selecting it; out of the tab order, as the
 * outbox line is, so the sidebar keeps its one roving stop on a mailbox.
 */
export function AccountGroup(p: AccountGroupProps) {
  const headingId = `account-${p.name}`;
  const contentId = `account-${p.name}-mailboxes`;
  const collapsedUnread = !p.expanded && p.inboxUnread ? p.inboxUnread : 0;
  return (
    <SidebarGroup aria-labelledby={headingId}>
      <SidebarGroupLabel
        render={<button type="button" />}
        id={headingId}
        tabIndex={-1}
        aria-expanded={p.expanded}
        aria-controls={contentId}
        data-account-toggle={p.name}
        onClick={() => p.onToggle(p.name)}
        className="w-full gap-2 text-left hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
      >
        <ChevronRight aria-hidden="true" className={`transition-transform ${p.expanded ? "rotate-90" : ""}`} />
        <span className="truncate">{p.name}</span>
        <SyncHealthBadge health={p.health} />
        {p.runtime !== "ready" ? (
          <span className="text-muted-foreground font-normal">{p.runtime}</span>
        ) : null}
        {collapsedUnread ? (
          <span data-slot="account-unread" className="ml-auto font-semibold tabular-nums">
            <span className="sr-only">, </span>
            {collapsedUnread}
            <span className="sr-only"> unread in the inbox</span>
          </span>
        ) : null}
      </SidebarGroupLabel>
      <SidebarGroupContent id={contentId}>
        <SidebarMenu>
          {!p.expanded
            ? null
            : p.mailboxes === null
            ? [0, 1, 2].map((i) => (
                <li key={i} className="px-2 py-1">
                  <Skeleton className="h-6 w-full" />
                </li>
              ))
            : p.mailboxes.map((m, i) => {
                const selected = p.selected.account === p.name && p.selected.mailbox === m.slug;
                return (
                  <MailboxItem
                    key={m.slug}
                    account={p.name}
                    slug={m.slug}
                    label={m.label}
                    kind={m.kind}
                    unread={m.unread}
                    total={m.total}
                    digit={p.digits && i < 9 ? i + 1 : null}
                    active={p.mailShown && !p.outboxShown && selected}
                    selected={selected}
                    cursor={p.cursor?.account === p.name && p.cursor.slug === m.slug}
                    onSelect={p.onSelect}
                  />
                );
              })}
          <OutboxState account={p.name} outbox={p.outbox} active={p.mailShown && p.outboxOpen} onOpen={p.onOpenOutbox} />
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}
