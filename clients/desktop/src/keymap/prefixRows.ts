// The which-key popup's rows: the continuations of an armed prefix that the
// keymap would carry out in this view and focus, labelled from the catalog,
// as the TUI's `prefix_popup_rows` (clients/tui/src/app/mod.rs).

import type { AppState } from "@/app/state";
import { GUI_ENTRIES, paletteEntries, type PaletteEntry } from "@/keymap/catalog";
import { prefixRuns } from "@/keymap/useKeymap";

/** The family names of the TUI's `prefix_family_name`, so the two titles agree. */
export function prefixFamilyName(key: string): string {
  switch (key) {
    case "f":
      return "find";
    case "c":
      return "compose";
    case "g":
      return "go";
    case "t":
      return "thread";
    case "s":
      return "system";
    case " ":
      return "view";
    default:
      return "keys";
  }
}

export type PrefixRow = { key: string; label: string };

let entries: PaletteEntry[] | null = null;

/**
 * One row per continuation of `prefix` that runs here, in catalog order (the
 * KEYMAP rows, then the desktop's own), deduplicated by key. A row such as
 * `gj / gk` shows when either of its keys runs, under the catalog's whole key
 * as the TUI shows it (`gg / G`, though `G` is no continuation of `g`); a
 * KEYMAP row whose action the desktop leaves to a notice is left out.
 */
export function prefixPopupRows(s: AppState, prefix: string): PrefixRow[] {
  entries ??= [...paletteEntries(), ...GUI_ENTRIES];
  const lead = prefix === " " ? "Space " : prefix;
  const seen = new Set<string>();
  const rows: PrefixRow[] = [];
  for (const entry of entries) {
    if (!entry.id) continue;
    for (const keys of entry.keys) {
      const combos = keys
        .split(" / ")
        .map((k) => k.trim())
        .filter((k) => k.length > lead.length && k.startsWith(lead) && !seen.has(k));
      if (!combos.some((k) => prefixRuns(s, k))) continue;
      for (const k of combos) seen.add(k);
      rows.push({ key: keys, label: entry.label });
    }
  }
  return rows;
}
