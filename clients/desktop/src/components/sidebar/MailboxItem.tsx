import { memo } from "react";
import { SidebarMenuBadge, SidebarMenuButton, SidebarMenuItem } from "@/components/ui/sidebar";
import { MAILBOX_ICON } from "@/components/sidebar/icons";
import type { MailboxKind } from "@/lib/gui-types";

export type MailboxItemProps = {
  account: string;
  slug: string;
  label: string;
  kind: MailboxKind;
  unread: number;
  total: number;
  /** 1-9 when the digit key jumps here. */
  digit: number | null;
  active: boolean;
  cursor: boolean;
  onSelect: (account: string, slug: string) => void;
};

/** One sidebar mailbox. Pure: everything it shows comes in as props. */
export const MailboxItem = memo(function MailboxItem(p: MailboxItemProps) {
  const Icon = MAILBOX_ICON[p.kind];
  const count = p.kind === "drafts" ? p.total : p.unread;
  const countLabel = p.kind === "drafts" ? `${p.total} drafts` : `${p.unread} unread of ${p.total}`;
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        isActive={p.active}
        aria-current={p.active ? "page" : undefined}
        tooltip={`${p.label} (${countLabel})`}
        tabIndex={p.cursor ? 0 : -1}
        data-roving={p.cursor ? "active" : undefined}
        data-mailbox={`${p.account}/${p.slug}`}
        aria-label={`${p.label}, ${countLabel}${p.digit ? `, key ${p.digit}` : ""}`}
        onClick={() => p.onSelect(p.account, p.slug)}
        className={p.cursor && !p.active ? "bg-sidebar-accent/50" : undefined}
      >
        <Icon aria-hidden="true" />
        <span>{p.label}</span>
      </SidebarMenuButton>
      {count > 0 ? (
        <SidebarMenuBadge aria-hidden="true" className={p.unread > 0 && p.kind !== "drafts" ? "font-semibold" : "font-normal"}>
          {count}
        </SidebarMenuBadge>
      ) : null}
    </SidebarMenuItem>
  );
});
