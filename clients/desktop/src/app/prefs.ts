// Presentation preferences (pane sizes, sidebar collapsed, notices hidden, the accounts listed
// flat rather than by conversation, the actions last run from the palette) live in the GUI's
// localStorage, never in the daemon: they are not domain state.

import { DEFAULT_PREFS, LIST_WIDTH_MAX, LIST_WIDTH_MIN, PALETTE_RECENT_MAX, type Prefs } from "@/app/state";

const strings = (v: unknown): v is string[] => Array.isArray(v) && v.every((a) => typeof a === "string");

export const PREFS_KEY = "mailypoppins.desktop.prefs.v1";

export function loadPrefs(storage: Storage | undefined = globalThis.localStorage): Prefs {
  try {
    const raw = storage?.getItem(PREFS_KEY);
    if (!raw) return DEFAULT_PREFS;
    const p = JSON.parse(raw) as Partial<Prefs>;
    const listWidth =
      typeof p.listWidth === "number" && Number.isFinite(p.listWidth)
        ? Math.max(LIST_WIDTH_MIN, Math.min(LIST_WIDTH_MAX, p.listWidth))
        : DEFAULT_PREFS.listWidth;
    return {
      sidebarCollapsed: typeof p.sidebarCollapsed === "boolean" ? p.sidebarCollapsed : false,
      listWidth,
      activityHidden: typeof p.activityHidden === "boolean" ? p.activityHidden : false,
      flatListAccounts: strings(p.flatListAccounts) ? [...new Set(p.flatListAccounts)] : DEFAULT_PREFS.flatListAccounts,
      paletteRecent: strings(p.paletteRecent)
        ? [...new Set(p.paletteRecent)].slice(0, PALETTE_RECENT_MAX)
        : DEFAULT_PREFS.paletteRecent,
    };
  } catch {
    return DEFAULT_PREFS;
  }
}

export function savePrefs(prefs: Prefs, storage: Storage | undefined = globalThis.localStorage): void {
  try {
    storage?.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    // A full or disabled storage costs the preference, nothing else.
  }
}
