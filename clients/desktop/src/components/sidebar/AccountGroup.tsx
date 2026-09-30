import { SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarMenu } from "@/components/ui/sidebar";
import { Skeleton } from "@/components/ui/skeleton";
import { MailboxItem } from "@/components/sidebar/MailboxItem";
import { OutboxState } from "@/components/sidebar/OutboxState";
import { SyncHealthBadge } from "@/components/sidebar/SyncHealthBadge";
import { kindOfRole } from "@/components/sidebar/icons";
import type { MailboxKind } from "@/lib/gui-types";
import type { AccountState, OutboxCounts, SyncHealthState } from "@/protocol/types";

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
  outbox: OutboxCounts;
  mailboxes: SidebarMailbox[] | null;
  selected: { account: string | null; mailbox: string | null };
  cursor: { account: string; slug: string } | null;
  /** Whether the digit keys address this account's mailboxes. */
  digits: boolean;
  onSelect: (account: string, slug: string) => void;
};

export function AccountGroup(p: AccountGroupProps) {
  const headingId = `account-${p.name}`;
  return (
    <SidebarGroup aria-labelledby={headingId}>
      <SidebarGroupLabel id={headingId} className="gap-2">
        <span className="truncate">{p.name}</span>
        <SyncHealthBadge health={p.health} />
        {p.runtime !== "ready" ? (
          <span className="text-muted-foreground font-normal">{p.runtime}</span>
        ) : null}
      </SidebarGroupLabel>
      <SidebarGroupContent>
        <SidebarMenu>
          {p.mailboxes === null
            ? [0, 1, 2].map((i) => (
                <li key={i} className="px-2 py-1">
                  <Skeleton className="h-6 w-full" />
                </li>
              ))
            : p.mailboxes.map((m, i) => (
                <MailboxItem
                  key={m.slug}
                  account={p.name}
                  slug={m.slug}
                  label={m.label}
                  kind={m.kind}
                  unread={m.unread}
                  total={m.total}
                  digit={p.digits && i < 9 ? i + 1 : null}
                  active={p.selected.account === p.name && p.selected.mailbox === m.slug}
                  cursor={p.cursor?.account === p.name && p.cursor.slug === m.slug}
                  onSelect={p.onSelect}
                />
              ))}
          <OutboxState outbox={p.outbox} />
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}

export { kindOfRole };
