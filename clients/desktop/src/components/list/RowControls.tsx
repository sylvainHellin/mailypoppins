// The pointer affordances inside a list row: the mark box, and the unread
// and flag toggles. Each is out of the tab order (the list keeps one roving
// tab stop) and has its key: `v`, `u`, `*`.

import type { MouseEvent, ReactNode } from "react";
import { Check, LoaderCircle, Star } from "lucide-react";

/** A click inside a row that must not select or open the row. */
function own(handler: (e: MouseEvent) => void) {
  return (e: MouseEvent) => {
    e.stopPropagation();
    handler(e);
  };
}

const stop = (e: MouseEvent) => e.stopPropagation();

function RowButton({
  label,
  pressed,
  onClick,
  className,
  children,
  role,
}: {
  label: string;
  pressed: boolean;
  onClick: (e: MouseEvent) => void;
  className: string;
  children: ReactNode;
  role?: "checkbox";
}) {
  return (
    <button
      type="button"
      tabIndex={-1}
      role={role}
      aria-label={label}
      aria-pressed={role ? undefined : pressed}
      aria-checked={role ? pressed : undefined}
      title={label}
      onClick={own(onClick)}
      onDoubleClick={stop}
      className={`inline-flex shrink-0 cursor-pointer items-center justify-center rounded-sm outline-none focus-visible:outline-2 focus-visible:outline-ring ${className}`}
    >
      {children}
    </button>
  );
}

/** The multi-select box: a click toggles the row's mark, Shift+click marks the range. */
export function MarkBox({
  marked,
  anyMarked,
  onToggle,
}: {
  marked: boolean;
  /** Any row of the list is marked: every box shows, not only on hover. */
  anyMarked: boolean;
  onToggle: (range: boolean) => void;
}) {
  return (
    <RowButton
      role="checkbox"
      label="Mark"
      pressed={marked}
      onClick={(e) => onToggle(e.shiftKey)}
      className={`size-4 border ${
        marked ? "border-link bg-link text-background" : "border-input text-transparent"
      } ${marked || anyMarked ? "" : "opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100"}`}
    >
      <Check aria-hidden="true" className="size-3" />
    </RowButton>
  );
}

/** The unread dot as a toggle: pressed while the row is unread. */
export function UnreadToggle({ unread, onToggle }: { unread: boolean; onToggle: () => void }) {
  return (
    <RowButton
      label="Unread"
      pressed={unread}
      onClick={onToggle}
      className={`size-4 ${unread ? "" : "opacity-0 group-hover:opacity-100"}`}
    >
      <span aria-hidden="true" className={`size-2 rounded-full ${unread ? "bg-link" : "border border-input"}`} />
    </RowButton>
  );
}

/** The star as a toggle: pressed while the row is flagged. */
export function FlagToggle({ flagged, onToggle }: { flagged: boolean; onToggle: () => void }) {
  return (
    <RowButton
      label="Flagged"
      pressed={flagged}
      onClick={onToggle}
      className={`size-5 ${flagged ? "text-warning" : "text-muted-foreground opacity-0 group-hover:opacity-100"}`}
    >
      <Star aria-hidden="true" className={`size-3.5 ${flagged ? "fill-current" : ""}`} />
    </RowButton>
  );
}

/** A change the daemon has not confirmed yet. */
export function PendingMark() {
  return <LoaderCircle aria-hidden="true" data-slot="pending" className="size-3.5 animate-spin text-muted-foreground" />;
}
