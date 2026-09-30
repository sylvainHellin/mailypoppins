import { ChevronLeft } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { Pane } from "@/app/state";

const TITLES: Record<Pane, string> = { sidebar: "Mailboxes", list: "Messages", reader: "Message" };
const PARENT: Record<Pane, Pane | null> = { sidebar: null, list: "sidebar", reader: "list" };

/** The narrow layout's back path: one view at a time, a way up from each. */
export function NarrowBar({ view, onUp }: { view: Pane; onUp: () => void }) {
  const parent = PARENT[view];
  return (
    <div className="flex h-10 shrink-0 items-center gap-1 border-b border-border px-2">
      {parent ? (
        <Button variant="ghost" size="sm" onClick={onUp} aria-label={`Back to ${TITLES[parent]} (Esc)`}>
          <ChevronLeft aria-hidden="true" />
          {TITLES[parent]}
        </Button>
      ) : null}
      <span className="ml-1 text-sm font-semibold">{TITLES[view]}</span>
    </div>
  );
}
