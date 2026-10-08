import { Activity, CalendarDays, CircleArrowUp, RotateCw, Settings, Users, type LucideIcon } from "lucide-react";
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
 * `Space a`, and the palette opens all three. Activity is no view: it opens
 * the activity log dialog, as `s l` does. The update entry shows only while
 * an update is known (src/app/updates.ts, `updateEntryLabel`): it installs
 * the update, or restarts into one already installed.
 */
export function ViewEntries({
  current,
  onOpen,
  onActivity,
  update = null,
}: {
  current: View;
  onOpen: (view: Exclude<View, "mail">) => void;
  onActivity: () => void;
  update?: { label: string; restart: boolean; onChoose: () => void } | null;
}) {
  return (
    <SidebarGroup className="mt-auto">
      <SidebarGroupContent>
        <SidebarMenu aria-label="Views">
          {update ? (
            <SidebarMenuItem>
              <SidebarMenuButton tabIndex={-1} tooltip={update.label} aria-label={update.label} data-view-entry="update" onClick={update.onChoose}>
                {update.restart ? <RotateCw aria-hidden="true" className="text-link" /> : <CircleArrowUp aria-hidden="true" className="text-link" />}
                <span>{update.label}</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ) : null}
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
              tabIndex={-1}
              tooltip="Activity log (s l)"
              aria-label="Activity log, key s l"
              data-view-entry="activity"
              onClick={onActivity}
            >
              <Activity aria-hidden="true" />
              <span>Activity</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}
