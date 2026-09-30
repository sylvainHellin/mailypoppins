import { Send } from "lucide-react";
import { SidebarMenuItem } from "@/components/ui/sidebar";
import type { OutboxCounts } from "@/protocol/types";

/** The account's outbox, shown only when something waits or failed. */
export function OutboxState({ outbox }: { outbox: OutboxCounts }) {
  if (outbox.queued === 0 && outbox.failed === 0) return null;
  const text = [
    outbox.queued > 0 ? `${outbox.queued} queued` : null,
    outbox.failed > 0 ? `${outbox.failed} failed` : null,
  ]
    .filter(Boolean)
    .join(", ");
  return (
    <SidebarMenuItem>
      <div
        className={`flex h-8 items-center gap-2 rounded-md px-2 text-xs group-data-[collapsible=icon]:justify-center ${outbox.failed > 0 ? "text-warning" : "text-muted-foreground"}`}
        title={`Outbox: ${text}`}
      >
        <Send className="size-4 shrink-0" aria-hidden="true" />
        <span className="truncate group-data-[collapsible=icon]:sr-only">Outbox: {text}</span>
      </div>
    </SidebarMenuItem>
  );
}
