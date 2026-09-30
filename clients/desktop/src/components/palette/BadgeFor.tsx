import { Badge } from "@/components/ui/badge";
import type { Badge as BadgeKind } from "@/keymap/catalog";

const TITLE: Record<BadgeKind, string> = {
  M2: "Arrives with mutations (M2)",
  M3: "Arrives with compose (M3)",
  M4: "Arrives in M4",
  soon: "Arrives in the next M1 unit",
  key: "A key binding, not a command",
  menu: "In the app menu",
  later: "Not in the desktop client yet",
};

export function BadgeFor({ badge }: { badge: BadgeKind }) {
  return (
    <Badge variant="outline" className="text-muted-foreground" title={TITLE[badge]}>
      {badge}
    </Badge>
  );
}
