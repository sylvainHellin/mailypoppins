import { memo, type MouseEvent } from "react";
import { CircleAlert, CircleCheck, Pencil } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { StatusPill } from "@/components/compose/DraftPreview";
import type { DraftItem } from "@/app/state";
import { shortDate } from "@/components/list/format";
import { MarkBox, PendingMark } from "@/components/list/RowControls";
import { ROW_HEIGHT } from "@/components/list/useWindow";

export type DraftRowProps = {
  draft: DraftItem;
  /** A compose session has the draft open in the editor. */
  editing: boolean;
  /** The cursor: the draft the reader shows. */
  cursor: boolean;
  marked: boolean;
  /** Any row is marked; `aria-selected` then names the marks. */
  anyMarked: boolean;
  pending: boolean;
  tabStop: boolean;
  /** 1-based, with the list's length. */
  position: number;
  setSize: number;
  onSelect: (id: string) => void;
  onMark: (id: string, range: boolean) => void;
};

/**
 * One draft in the Drafts listing, with its status (draft, approved), an
 * `invalid` badge for a file that does not parse (why in its title and its
 * name), and whether the editor has it open. Pure.
 */
export const DraftRow = memo(function DraftRow({ draft, editing, cursor, marked, anyMarked, pending, tabStop, position, setSize, onSelect, onMark }: DraftRowProps) {
  const invalid = draft.diagnostic !== null || !draft.valid;
  const subject = draft.subject || (invalid ? draft.id : "(no subject)");
  const to = draft.to || "(no recipient)";
  const state = invalid
    ? `invalid: ${draft.diagnostic ?? "does not parse"}`
    : `${draft.status}, ${draft.ready ? "ready to send" : "not ready"}`;
  const onClick = (e: MouseEvent) => {
    if (e.shiftKey) onMark(draft.id, true);
    else if (e.metaKey || e.ctrlKey) onMark(draft.id, false);
    else onSelect(draft.id);
  };
  return (
    <div
      role="option"
      aria-selected={anyMarked ? marked : cursor}
      aria-busy={pending || undefined}
      aria-posinset={position}
      aria-setsize={setSize}
      aria-label={`Draft to ${to}, ${subject}, ${state}${editing ? ", open in the editor" : ""}${pending ? ", change pending" : ""}${marked ? ", marked" : ""}`}
      tabIndex={tabStop ? 0 : -1}
      data-roving={tabStop ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-marked={marked || undefined}
      data-draft-id={draft.id}
      title={invalid ? (draft.diagnostic ?? "The file does not parse") : undefined}
      style={{ height: ROW_HEIGHT }}
      onClick={onClick}
      className={`group flex cursor-default flex-col justify-center gap-0.5 border-b border-border px-3 text-sm outline-none select-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring ${
        cursor ? "bg-selection text-selection-foreground" : marked ? "bg-accent text-accent-foreground" : "hover:bg-accent/60"
      }`}
    >
      <div className="flex min-w-0 items-center gap-2">
        <MarkBox marked={marked} anyMarked={anyMarked} onToggle={(range) => onMark(draft.id, range)} />
        {draft.ready ? (
          <CircleCheck aria-hidden="true" className="size-3.5 shrink-0 text-link" />
        ) : (
          <CircleAlert aria-hidden="true" className={`size-3.5 shrink-0 ${invalid ? "text-destructive" : "text-muted-foreground"}`} />
        )}
        <span className="min-w-0 flex-1 truncate">{to}</span>
        {pending ? <PendingMark /> : null}
        {editing ? (
          <Badge variant="secondary" data-slot="draft-editing" aria-hidden="true">
            <Pencil />
            editing
          </Badge>
        ) : null}
        <StatusPill status={invalid ? "invalid" : draft.status} hidden />
        <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{shortDate(draft.date ?? "")}</span>
      </div>
      <div className="truncate pl-11 text-muted-foreground">{subject}</div>
    </div>
  );
});
