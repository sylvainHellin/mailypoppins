import type { SyncHealthState } from "@/protocol/types";
import { HEALTH } from "@/components/sidebar/icons";

/** The account's last-sync verdict: an icon with a text alternative. */
export function SyncHealthBadge({ health }: { health: SyncHealthState }) {
  const h = HEALTH[health];
  const Icon = h.icon;
  return (
    <span className={`inline-flex items-center ${h.className}`} title={h.label}>
      <Icon className="size-3.5" aria-hidden="true" />
      <span className="sr-only">{h.label}</span>
    </span>
  );
}
