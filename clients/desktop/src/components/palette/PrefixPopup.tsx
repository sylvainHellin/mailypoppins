import { useMemo } from "react";
import { Kbd } from "@/components/ui/kbd";
import { screenFor, type AppState } from "@/app/state";
import { isEditable } from "@/keymap/useKeymap";
import { usePendingPrefix } from "@/keymap/pendingPrefix";
import { prefixFamilyName, prefixPopupRows, type PrefixRow } from "@/keymap/prefixRows";

/** Past this many rows the popup lays them out in two columns. */
const ONE_COLUMN_MAX = 8;

function Column({ rows }: { rows: PrefixRow[] }) {
  return (
    <ul className="grid grid-cols-[auto_1fr] content-start items-center gap-x-3 gap-y-1">
      {rows.map((r) => (
        <li key={r.key} data-testid="prefix-row" data-key={r.key} className="contents">
          <Kbd className="font-mono text-link">{r.key}</Kbd>
          <span className="whitespace-nowrap">{r.label}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * The which-key popup for an armed family prefix, the TUI's
 * `render_prefix_popup`: titled by the family, one row per continuation the
 * keymap would carry out in this view and focus. It follows the keymap's
 * pending prefix (src/keymap/pendingPrefix.ts), so the next key, the
 * timeout and leaving the window all close it; an open dialog hides it.
 * The live region stays mounted, so a screen reader hears each family.
 */
export function PrefixPopup({ state }: { state: AppState }) {
  const pending = usePendingPrefix();
  const shown =
    pending !== null &&
    state.overlay === null &&
    screenFor(state) === "shell" &&
    !isEditable(document.activeElement);
  // The rows read the state only while a prefix is armed.
  const rows = useMemo(() => (shown && pending ? prefixPopupRows(state, pending.key) : []), [shown, pending, state]);
  const title = pending ? prefixFamilyName(pending.key) : "";
  const columns = rows.length > ONE_COLUMN_MAX ? [rows.slice(0, Math.ceil(rows.length / 2)), rows.slice(Math.ceil(rows.length / 2))] : [rows];

  return (
    <div role="status" aria-live="polite" data-slot="prefix-popup" className="contents">
      {rows.length > 0 ? (
        <div
          className="pointer-events-none fixed bottom-14 left-1/2 z-40 max-w-[calc(100vw-2rem)] -translate-x-1/2 rounded-lg border border-ring bg-popover px-3 pt-1.5 pb-2 text-sm text-popover-foreground shadow-md"
        >
          <div data-testid="prefix-title" className="mb-1.5 text-xs font-medium text-link">
            {title}
          </div>
          <div className="flex gap-6">
            {columns.map((c, i) => (
              <Column key={i} rows={c} />
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}
