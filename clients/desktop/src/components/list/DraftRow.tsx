import { memo } from "react";
import { CircleAlert, CircleCheck } from "lucide-react";
import type { DraftEntry } from "@/protocol/types";
import { shortDate } from "@/components/list/format";
import { ROW_HEIGHT } from "@/components/list/useWindow";

export type DraftRowProps = {
  draft: DraftEntry;
  selected: boolean;
  tabStop: boolean;
  onSelect: (id: string) => void;
};

/** One draft in the Drafts listing. Pure. */
export const DraftRow = memo(function DraftRow({ draft, selected, tabStop, onSelect }: DraftRowProps) {
  const subject = draft.subject || "(no subject)";
  const to = draft.to || "(no recipient)";
  return (
    <div
      role="option"
      aria-selected={selected}
      aria-label={`Draft to ${to}, ${subject}, ${draft.ready ? "ready to send" : "not ready"}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      style={{ height: ROW_HEIGHT }}
      onClick={() => onSelect(draft.id)}
      className={`flex cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        selected ? "bg-selection text-selection-foreground" : "hover:bg-accent/60"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        {draft.ready ? (
          <CircleCheck aria-hidden="true" className="size-3.5 shrink-0 text-link" />
        ) : (
          <CircleAlert aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
        )}
        <span className="min-w-0 flex-1 truncate">{to}</span>
        <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{shortDate(draft.date ?? "")}</span>
      </div>
      <div className="truncate pl-5 text-muted-foreground">{subject}</div>
    </div>
  );
});
