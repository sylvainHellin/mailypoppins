import { Activity, CalendarDays, Settings, Users, type LucideIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import {
  SidebarGroup,
  SidebarGroupContent,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";
import type { View } from "@/app/state";

type Entry = { view: Exclude<View, "mail">; label: string; icon: LucideIcon; keys: string | null };

const ENTRIES: Entry[] = [
  { view: "contacts", label: "Contacts", icon: Users, keys: "Space c" },
  { view: "calendar", label: "Calendar", icon: CalendarDays, keys: "Space a" },
  { view: "settings", label: "Settings", icon: Settings, keys: null },
];

/**
 * The views beside Mail, at the foot of the sidebar: each entry shows its
 * view, and the one shown is the current page. Like the outbox line they are
 * pointer entries out of the pane's tab order; their keys are `Space c` and
 * `Space a`, and the palette opens all three. Activity stays disabled until
 * its unit brings the log.
 */
export function ViewEntries({ current, onOpen }: { current: View; onOpen: (view: Exclude<View, "mail">) => void }) {
  return (
    <SidebarGroup className="mt-auto">
      <SidebarGroupContent>
        <SidebarMenu aria-label="Views">
          {ENTRIES.map(({ view, label, icon: Icon, keys }) => {
            const active = current === view;
            return (
              <SidebarMenuItem key={view}>
                <SidebarMenuButton
                  isActive={active}
                  aria-current={active ? "page" : undefined}
                  tabIndex={-1}
                  tooltip={keys ? `${label} (${keys})` : label}
                  aria-label={keys ? `${label}, key ${keys}` : label}
                  data-view-entry={view}
                  onClick={() => onOpen(view)}
                >
                  <Icon aria-hidden="true" />
                  <span>{label}</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            );
          })}
          <SidebarMenuItem>
            <SidebarMenuButton
              aria-disabled="true"
              tabIndex={-1}
              tooltip="Activity (M4)"
              className="text-disabled-foreground aria-disabled:opacity-100"
            >
              <Activity aria-hidden="true" />
              <span>Activity</span>
            </SidebarMenuButton>
            <Badge
              variant="outline"
              className="pointer-events-none absolute top-1.5 right-1 h-5 text-disabled-foreground group-data-[collapsible=icon]:hidden"
            >
              M4
            </Badge>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}
