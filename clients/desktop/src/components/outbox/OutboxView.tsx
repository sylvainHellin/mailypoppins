import { ArrowLeft, RotateCw, Trash2 } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { runAction } from "@/app/actions";
import { cursorRow, outboxRows, outboxSummary, retryable, rowAction, stateLabel } from "@/app/outbox";
import { useAppState, useDispatch } from "@/app/store";
import { usePaneFocused } from "@/hooks/use-pane-focused";
import type { OutboxAction } from "@/app/state";
import type { OutboxRow } from "@/protocol/types";

/** A unix timestamp in local time to the minute, as `mp outbox list` prints it. */
export function localMinute(ts: number): string {
  if (ts <= 0) return "-";
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

const CHIP_TONE: Record<string, string> = {
  failed: "bg-destructive/10 text-destructive",
  partial: "bg-warning/15 text-warning",
  sent_pending_append: "bg-secondary text-secondary-foreground",
  pending_send: "bg-secondary text-secondary-foreground",
};

/** The lines `mp outbox list` indents under a row, in the GUI's words. */
export function rowNotes(row: OutboxRow): string[] {
  const notes: string[] = [];
  if (row.target_mailbox) {
    notes.push(row.state === "sent_pending_append" ? `Sent copy owed to ${row.target_mailbox}` : `Sent copy goes to ${row.target_mailbox}`);
  }
  if (row.never_submitted) notes.push("Never submitted; the next sync sends it");
  for (const [address, reason] of row.rejected) notes.push(`Never delivered to ${address} (${reason})`);
  // A `done` row owes nobody anything more: the note says who missed out.
  if (row.outstanding.length > 0 && row.state !== "done") notes.push(`Still to deliver to ${row.outstanding.join(", ")}`);
  if (row.last_error) notes.push(`${row.partial ? "Outcome" : "Last error"}: ${row.last_error}`);
  return notes;
}

function OutboxRowItem({
  row,
  cursor,
  action,
  onSelect,
  onRetry,
  onDiscard,
}: {
  row: OutboxRow;
  cursor: boolean;
  action: OutboxAction | null;
  onSelect: () => void;
  onRetry: () => void;
  onDiscard: () => void;
}) {
  const label = stateLabel(row);
  const busy = action?.kind === "retry" ? "retrying" : action?.kind === "discard" ? "discarding" : null;
  const notes = rowNotes(row);
  return (
    <li
      aria-label={`Row ${row.id}, ${label}${busy ? `, ${busy}` : ""}`}
      aria-current={cursor || undefined}
      aria-busy={busy !== null || undefined}
      tabIndex={cursor ? 0 : -1}
      data-roving={cursor ? "active" : undefined}
      data-cursor={cursor || undefined}
      data-outbox-row={row.id}
      data-state={row.partial ? "partial" : row.state}
      onClick={onSelect}
      className="flex flex-col gap-1 border-b border-border px-3 py-2 text-sm outline-none data-[cursor]:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
    >
      <div className="flex items-center gap-2">
        <Badge data-slot="outbox-state" className={CHIP_TONE[row.partial ? "partial" : row.state] ?? "bg-secondary text-secondary-foreground"}>
          {label}
        </Badge>
        <span className="font-medium tabular-nums">Row {row.id}</span>
        <span className="text-xs text-muted-foreground tabular-nums">{localMinute(row.updated)}</span>
        {busy ? <span className="text-xs text-muted-foreground">{busy === "retrying" ? "Retrying…" : "Discarding…"}</span> : null}
        <span className="ml-auto flex gap-1">
          {retryable(row) ? (
            <Button size="xs" variant="outline" disabled={busy !== null} aria-label={`Retry row ${row.id}`} title="Retry (R)" onClick={(e) => (e.stopPropagation(), onRetry())}>
              <RotateCw aria-hidden="true" />
              Retry
            </Button>
          ) : null}
          <Button size="xs" variant="ghost" disabled={busy !== null} aria-label={`Discard row ${row.id}`} title="Discard (d)" onClick={(e) => (e.stopPropagation(), onDiscard())}>
            <Trash2 aria-hidden="true" />
            Discard
          </Button>
        </span>
      </div>
      <span className="truncate font-mono text-xs text-muted-foreground">{row.message_id}</span>
      {notes.map((n) => (
        <span key={n} data-slot="outbox-note" className="text-xs">
          {n}
        </span>
      ))}
    </li>
  );
}

/**
 * The outbox of one account, in the list pane in place of the mailbox list
 * (clients/desktop/docs/shell.md, "Outbox"): every unfinished row with its
 * state, what it still owes, and Retry and Discard behind a confirmation.
 * Escape brings the mailbox list back.
 */
export function OutboxView() {
  const s = useAppState();
  const dispatch = useDispatch();
  const focused = usePaneFocused("list");
  const view = s.outboxView;
  if (!view) return null;
  const account = view.account;
  const l = s.outbox[account];
  const listing = l?.data ?? null;
  const rows = outboxRows(s, account);
  const cursor = cursorRow(s);
  const counts = outboxSummary(s, account);
  const act = (id: "outbox_retry" | "outbox_discard", rowId: number) => {
    dispatch({ type: "outbox_select", row_id: rowId });
    // The cursor has moved in the model; run against the row clicked.
    runAction(id, { ...s, outboxView: { ...view, cursor: rowId } }, dispatch);
  };

  return (
    <section
      aria-label={`Outbox of ${account}`}
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-focused={focused}
      data-view="outbox"
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 flex-col gap-1 border-b border-border px-3 py-2">
        <div className="flex items-baseline justify-between gap-2">
          <h2 className="truncate text-sm font-semibold">Outbox: {account}</h2>
          <Button size="xs" variant="ghost" onClick={() => dispatch({ type: "close_outbox" })} title="Back to the mailbox (Esc)">
            <ArrowLeft aria-hidden="true" />
            Mailbox
          </Button>
        </div>
        {/* Mounted before the listing lands, so the first counts are announced. */}
        <p role="status" data-slot="outbox-counts" className="text-xs text-muted-foreground tabular-nums">
          {listing?.ever_used ? `${counts.queued} working, ${counts.failed} failed, ${counts.partial} partly delivered` : null}
        </p>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {l?.error && !listing ? (
          <p role="alert" className="p-4 text-sm text-destructive">
            The outbox did not load: {l.error.message}
          </p>
        ) : !listing ? (
          <div aria-busy="true" aria-label="Loading the outbox" className="flex flex-col gap-2 p-3">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-12 w-full" />
            ))}
          </div>
        ) : !listing.ever_used ? (
          <p className="p-4 text-sm text-muted-foreground">Nothing has been queued yet.</p>
        ) : rows.length === 0 ? (
          <p className="p-4 text-sm text-muted-foreground">The outbox is clear.</p>
        ) : (
          <ul aria-label={`Outbox rows of ${account}`}>
            {rows.map((row) => (
              <OutboxRowItem
                key={row.id}
                row={row}
                cursor={row.id === cursor?.id}
                action={rowAction(s, account, row.id)}
                onSelect={() => dispatch({ type: "outbox_select", row_id: row.id })}
                onRetry={() => act("outbox_retry", row.id)}
                onDiscard={() => act("outbox_discard", row.id)}
              />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
