import { Send } from "lucide-react";
import { SidebarMenuButton, SidebarMenuItem } from "@/components/ui/sidebar";
import { summaryText, type OutboxSummary } from "@/app/outbox";

/**
 * The account's outbox line, shown only when something waits, failed or
 * went to only some of its recipients; it opens the account's outbox (`g o`).
 */
export function OutboxState({ account, outbox, active, onOpen }: { account: string; outbox: OutboxSummary; active: boolean; onOpen: (account: string) => void }) {
  const text = summaryText(outbox);
  if (!text) return null;
  const alert = outbox.failed > 0 || outbox.partial > 0;
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        isActive={active}
        aria-current={active ? "page" : undefined}
        tooltip={`Outbox: ${text}`}
        tabIndex={-1}
        data-outbox={account}
        aria-label={`Outbox of ${account}: ${text}, key g o`}
        onClick={() => onOpen(account)}
        className={`text-xs ${alert ? "text-warning" : "text-muted-foreground"}`}
      >
        <Send aria-hidden="true" />
        <span className="truncate">Outbox: {text}</span>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}
