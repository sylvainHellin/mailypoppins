// Presentation preferences (pane sizes, sidebar collapsed, notices hidden) live in the GUI's
// localStorage, never in the daemon: they are not domain state.

import { DEFAULT_PREFS, LIST_WIDTH_MAX, LIST_WIDTH_MIN, type Prefs } from "@/app/state";

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
