import { TriangleAlert, LoaderCircle, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { SearchState } from "@/app/state";

function describe(search: SearchState): string {
  const n = search.hits.length;
  const results = `${n} ${n === 1 ? "result" : "results"}`;
  const where = `${search.mode === "server" ? "on the server" : "in the store"}${search.mailbox ? ` in ${search.mailbox}` : ""}`;
  switch (search.status) {
    case "searching":
      if (search.mode === "server") return `Starting the server search… ${results} so far`;
      return search.chain ? "Searching the store, then the server…" : "Searching the store…";
    case "running":
      return `Searching the server… ${results} so far`;
    case "done": {
      const unreachable = search.summary?.unreachable ?? 0;
      return `${results} ${where}${unreachable ? `, ${unreachable} mailbox${unreachable === 1 ? "" : "es"} unreachable` : ""}`;
    }
    case "cancelled":
      return `Server search cancelled, ${results}`;
    case "failed":
      return `The search failed: ${search.error ?? "unknown error"}`;
    case "dropped":
      return `The server search was dropped: ${search.error ?? "the daemon restarted"}`;
  }
}

/** The search's progress, its outcome, and the cancel while the server runs. */
export function SearchStatusBar({
  search,
  onCancel,
  onServer,
  onExit,
}: {
  search: SearchState;
  onCancel: () => void;
  onServer: () => void;
  onExit: () => void;
}) {
  const busy = search.status === "searching" || search.status === "running";
  const bad = search.status === "failed" || search.status === "dropped";
  return (
    <div
      data-slot="search-status"
      data-status={search.status}
      className="flex min-h-7 items-center gap-2 text-xs"
    >
      {busy ? <LoaderCircle aria-hidden="true" className="size-3.5 shrink-0 animate-spin text-link" /> : null}
      {bad ? <TriangleAlert aria-hidden="true" className="size-3.5 shrink-0 text-destructive" /> : null}
      {/* A failure wraps, so the daemon's whole sentence reads (PERSO-115); progress stays one line. */}
      <span
        role={bad ? "alert" : "status"}
        title={describe(search)}
        className={`min-w-0 flex-1 ${bad ? "break-words whitespace-normal text-destructive" : "truncate text-muted-foreground"}`}
      >
        {describe(search)}
      </span>
      {search.mode === "server" && busy ? (
        <Button size="xs" variant="outline" disabled={search.status !== "running"} onClick={onCancel}>
          Cancel
        </Button>
      ) : null}
      {search.mode === "local" && search.status === "done" ? (
        <Button size="xs" variant="outline" onClick={onServer}>
          Search server
        </Button>
      ) : null}
      <Button size="icon-xs" variant="ghost" aria-label="Close the search (Escape)" onClick={onExit}>
        <X aria-hidden="true" />
      </Button>
    </div>
  );
}
