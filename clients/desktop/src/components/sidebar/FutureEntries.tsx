import { Activity, CalendarDays, Settings, Users, type LucideIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import {
  SidebarGroup,
  SidebarGroupContent,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";

const ENTRIES: { label: string; icon: LucideIcon }[] = [
  { label: "Contacts", icon: Users },
  { label: "Calendar", icon: CalendarDays },
  { label: "Settings", icon: Settings },
  { label: "Activity", icon: Activity },
];

/** Places the plan gives the sidebar, disabled until M4 brings them. */
export function FutureEntries() {
  return (
    <SidebarGroup className="mt-auto">
      <SidebarGroupContent>
        <SidebarMenu>
          {ENTRIES.map(({ label, icon: Icon }) => (
            <SidebarMenuItem key={label}>
              <SidebarMenuButton
                aria-disabled="true"
                tabIndex={-1}
                tooltip={`${label} (M4)`}
                className="text-disabled-foreground aria-disabled:opacity-100"
              >
                <Icon aria-hidden="true" />
                <span>{label}</span>
              </SidebarMenuButton>
              <Badge
                variant="outline"
                className="pointer-events-none absolute top-1.5 right-1 h-5 text-disabled-foreground group-data-[collapsible=icon]:hidden"
              >
                M4
              </Badge>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}
