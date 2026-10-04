import type { ReactNode } from "react";
import { ArrowLeft } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useDispatch } from "@/app/store";
import { usePaneFocused } from "@/hooks/use-pane-focused";
import { VIEW_TITLES } from "@/app/views";
import type { View } from "@/app/state";

/**
 * A full-pane view no unit has filled yet: its title, one line, and the way
 * back to Mail. The region is named after the view, as the filled view will be.
 */
export function EmptyView({ view, children }: { view: Exclude<View, "mail">; children?: ReactNode }) {
  const dispatch = useDispatch();
  const focused = usePaneFocused("list");
  const title = VIEW_TITLES[view];
  return (
    <section
      aria-label={title}
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-focused={focused}
      data-view={view}
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 items-baseline justify-between gap-2 border-b border-border px-3 py-2">
        <h2 className="truncate text-sm font-semibold">{title}</h2>
        <Button size="xs" variant="ghost" onClick={() => dispatch({ type: "switch_view", view: "mail" })} title="Back to Mail (Esc)">
          <ArrowLeft aria-hidden="true" />
          Mail
        </Button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto p-4 text-sm text-muted-foreground">{children}</div>
    </section>
  );
}
