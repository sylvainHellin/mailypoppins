import { useCallback, useEffect, useRef } from "react";
import { AgendaRow } from "@/components/calendar/AgendaRow";
import type { AgendaEvent } from "@/protocol/types";

export type AgendaListProps = {
  rows: AgendaEvent[];
  cursor: number | null;
  /** Moves whenever the keyboard moved the cursor, so the row is scrolled into view. */
  focusSeq: number;
  onSelect: (rowId: number) => void;
  onOpen: (rowId: number) => void;
};

/** The agenda's rows, a single-select listbox named "Agenda". */
export function AgendaList({ rows, cursor, focusSeq, onSelect, onOpen }: AgendaListProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>('[data-cursor="true"]')?.scrollIntoView?.({ block: "nearest" });
  }, [cursor, focusSeq]);
  const select = useCallback((rowId: number) => onSelect(rowId), [onSelect]);
  const open = useCallback((rowId: number) => onOpen(rowId), [onOpen]);
  return (
    <div ref={ref} role="listbox" aria-label="Agenda">
      {rows.map((event, i) => (
        <AgendaRow
          key={event.row_id}
          event={event}
          cursor={event.row_id === cursor}
          position={i + 1}
          setSize={rows.length}
          onSelect={select}
          onOpen={open}
        />
      ))}
    </div>
  );
}
