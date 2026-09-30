import { useCallback } from "react";
import { Mail } from "lucide-react";
import { Sidebar, SidebarContent, SidebarHeader, SidebarSeparator } from "@/components/ui/sidebar";
import { AccountGroup, type SidebarMailbox } from "@/components/sidebar/AccountGroup";
import { FutureEntries } from "@/components/sidebar/FutureEntries";
import { kindOfRole } from "@/components/sidebar/icons";
import { useAppState, useDispatch } from "@/app/store";
import { accountNames, type AppState } from "@/app/state";
import type { AccountState, OutboxCounts, SyncHealthState } from "@/protocol/types";

type AccountView = {
  name: string;
  runtime: AccountState;
  health: SyncHealthState;
  outbox: OutboxCounts;
  mailboxes: SidebarMailbox[] | null;
};

export function sidebarModel(s: AppState): AccountView[] {
  return accountNames(s).map((name) => {
    const info = s.accounts.data?.find((a) => a.name === name);
    const snap = s.bootstrap?.snapshot.accounts.find((a) => a.name === name);
    const listing = s.mailboxes[name]?.data;
    const mailboxes: SidebarMailbox[] | null = listing
      ? listing.mailboxes.map((m) => ({ slug: m.slug, label: m.label, kind: m.kind, unread: m.unread, total: m.total }))
      : (s.bootstrap?.snapshot.mailboxes[name]?.map((m) => ({
          slug: m.slug,
          label: m.label,
          kind: kindOfRole(m.role),
          unread: m.unread,
          total: m.total,
        })) ?? null);
    return {
      name,
      runtime: info?.runtime_state ?? snap?.state ?? "opening",
      health: info?.sync_health ?? snap?.sync_health.state ?? "unknown",
      outbox: info?.outbox ?? s.bootstrap?.snapshot.outbox[name] ?? { queued: 0, failed: 0 },
      mailboxes,
    };
  });
}

export function AppSidebar({ collapsible }: { collapsible: "icon" | "none" }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const onSelect = useCallback(
    (account: string, slug: string) => dispatch({ type: "select_mailbox", account, slug, focus: "list" }),
    [dispatch],
  );
  const cursor =
    s.sidebarCursor ??
    (s.selection.account && s.selection.mailbox ? { account: s.selection.account, slug: s.selection.mailbox } : null);
  const selectedAccount = s.selection.account;
  return (
    <Sidebar variant="inset" collapsible={collapsible} className={collapsible === "none" ? "w-full" : undefined}>
      <SidebarHeader>
        <div className="flex h-8 items-center gap-2 px-2 text-sm font-semibold">
          <Mail className="size-4 shrink-0 text-link" aria-hidden="true" />
          <span className="truncate group-data-[collapsible=icon]:sr-only">mailypoppins</span>
        </div>
      </SidebarHeader>
      <SidebarSeparator />
      <SidebarContent>
        <nav
          aria-label="Accounts and mailboxes"
          data-pane="sidebar"
          className="flex min-h-0 flex-1 flex-col"
          onFocus={() => dispatch({ type: "pane_focused", pane: "sidebar" })}
        >
          {sidebarModel(s).map((a) => (
            <AccountGroup
              key={a.name}
              name={a.name}
              runtime={a.runtime}
              health={a.health}
              outbox={a.outbox}
              mailboxes={a.mailboxes}
              selected={s.selection}
              cursor={cursor}
              digits={a.name === selectedAccount}
              onSelect={onSelect}
            />
          ))}
          <FutureEntries />
        </nav>
      </SidebarContent>
    </Sidebar>
  );
}
