import { useCallback } from "react";
import { Mail } from "lucide-react";
import { Sidebar, SidebarContent, SidebarHeader, SidebarSeparator } from "@/components/ui/sidebar";
import { AccountGroup, type SidebarMailbox } from "@/components/sidebar/AccountGroup";
import { ViewEntries } from "@/components/sidebar/ViewEntries";
import { kindOfRole } from "@/components/sidebar/icons";
import { useAppState, useDispatch } from "@/app/store";
import { accountNames, type AppState, type View } from "@/app/state";
import type { AccountState, SyncHealthState } from "@/protocol/types";
import { outboxSummary, type OutboxSummary } from "@/app/outbox";

type AccountView = {
  name: string;
  runtime: AccountState;
  health: SyncHealthState;
  outbox: OutboxSummary;
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
      // The listing once this window read it, which moves on every
      // invalidation; `list_accounts` only moves on a bootstrap.
      outbox: outboxSummary(s, name),
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
  const onOpenOutbox = useCallback((account: string) => dispatch({ type: "open_outbox", account }), [dispatch]);
  const onOpenView = useCallback((view: Exclude<View, "mail">) => dispatch({ type: "switch_view", view }), [dispatch]);
  // Outside Mail no mailbox is the current page, and the digits jump nowhere.
  const mail = s.view === "mail";
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
              outboxOpen={s.outboxView?.account === a.name}
              mailShown={mail}
              onOpenOutbox={onOpenOutbox}
              mailboxes={a.mailboxes}
              selected={s.selection}
              cursor={cursor}
              digits={mail && a.name === selectedAccount}
              onSelect={onSelect}
            />
          ))}
          <ViewEntries current={s.view} onOpen={onOpenView} />
        </nav>
      </SidebarContent>
    </Sidebar>
  );
}
