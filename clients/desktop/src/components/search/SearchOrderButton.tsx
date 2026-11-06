import { ArrowDownUp } from "lucide-react";
import { Button } from "@/components/ui/button";
import { searchOrderLabel } from "@/app/search";
import type { SearchState } from "@/app/state";

/**
 * The results header's order control (PERSO-273): it names the order the
 * daemon echoed for the shown hits, not the one asked for, and a click runs
 * the search again in the next of newest, oldest and best match first, as
 * `S` does.
 */
export function SearchOrderButton({ search, onCycle }: { search: SearchState; onCycle: () => void }) {
  const label = searchOrderLabel(search.echoed);
  return (
    <Button
      size="xs"
      variant="ghost"
      data-slot="search-order"
      aria-label={`Search order: ${label ?? "not answered yet"}. Change it (S)`}
      title="Order the results: newest, oldest, best match first (S)"
      onClick={onCycle}
      className="min-w-0 text-muted-foreground"
    >
      <ArrowDownUp aria-hidden="true" />
      <span className="truncate">{label ?? "Order"}</span>
    </Button>
  );
}
