// The colour theme (clients/desktop/docs/shell.md, "Settings"): dark, light,
// or the system's, stored as the `theme` key of desktop.json. index.html
// paints dark before any script runs, main.tsx starts reading the stored
// theme before the first render, and that theme replaces dark once
// `setting_get` answers. The store keeps the theme for the Settings view;
// whoever changes it paints it here, so no render repaints it.

import { useEffect, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

export type Theme = "dark" | "light" | "system";

/** The palette a theme paints with. */
export type Scheme = "dark" | "light";

export const THEMES: Theme[] = ["dark", "light", "system"];

/** What an unset or unreadable setting means. */
export const DEFAULT_THEME: Theme = "dark";

export const THEME_LABELS: Record<Theme, string> = { dark: "Dark", light: "Light", system: "System" };

const SYSTEM_DARK = "(prefers-color-scheme: dark)";

/** A stored value as a theme; anything else is the default. */
export function parseTheme(value: string | null | undefined): Theme {
  return THEMES.includes(value as Theme) ? (value as Theme) : DEFAULT_THEME;
}

/** The system's scheme; dark where the webview cannot say. */
export function systemScheme(win: Window = window): Scheme {
  const query = typeof win.matchMedia === "function" ? win.matchMedia(SYSTEM_DARK) : null;
  return query && !query.matches ? "light" : "dark";
}

/** Put one palette on the document: the `dark` or `light` class, `color-scheme` and its meta tag. */
export function paintScheme(scheme: Scheme, doc: Document = document): void {
  const root = doc.documentElement;
  root.classList.toggle("dark", scheme === "dark");
  root.classList.toggle("light", scheme === "light");
  root.style.colorScheme = scheme;
  const meta = doc.querySelector<HTMLMetaElement>('meta[name="color-scheme"]');
  if (meta) meta.content = scheme;
}

/**
 * The palette on the document now, as `paintScheme` left it: what the
 * embedded editor takes at spawn (ticket 0137). Dark unless `light` is painted.
 */
export function currentScheme(doc: Document = document): Scheme {
  return doc.documentElement.classList.contains("light") ? "light" : "dark";
}

/** The listener `system` keeps on the media query, removed by the next `applyTheme`. */
let detach: (() => void) | null = null;

/**
 * Paint `theme` now. `system` paints the system's scheme and follows its
 * changes until another theme is applied; dark or light stops following.
 * Answers the scheme painted.
 */
export function applyTheme(theme: Theme, win: Window = window): Scheme {
  detach?.();
  detach = null;
  if (theme !== "system") {
    paintScheme(theme, win.document);
    return theme;
  }
  const query = typeof win.matchMedia === "function" ? win.matchMedia(SYSTEM_DARK) : null;
  const scheme = systemScheme(win);
  paintScheme(scheme, win.document);
  if (query) {
    const follow = (e: MediaQueryListEvent) => paintScheme(e.matches ? "dark" : "light", win.document);
    query.addEventListener("change", follow);
    detach = () => query.removeEventListener("change", follow);
  }
  return scheme;
}

/** The stored theme; the default when unset or when the file does not read. */
export async function loadTheme(): Promise<Theme> {
  try {
    return parseTheme(await cmd.settingGet("theme"));
  } catch {
    return DEFAULT_THEME;
  }
}

/** The read main.tsx started, until the store takes it. */
let preload: Promise<Theme> | null = null;

/** Start reading the stored theme, and paint it as soon as it answers. */
export function preloadTheme(): Promise<Theme> {
  preload ??= loadTheme().then((theme) => {
    applyTheme(theme);
    return theme;
  });
  return preload;
}

/**
 * Keep the stored theme in the store: the read main.tsx started, else a read
 * of its own (a test, or StrictMode's second mount), painted when it answers.
 */
export function useStoredTheme(dispatch: Dispatch<Action>): void {
  useEffect(() => {
    let live = true;
    const read = preload ?? preloadTheme();
    preload = null;
    void read.then((theme) => {
      if (live) dispatch({ type: "theme_set", theme });
    });
    return () => {
      live = false;
    };
  }, [dispatch]);
}

/** Paint and keep `theme` at once, then store it; a refused write says so on the notice line. */
export async function saveTheme(dispatch: Dispatch<Action>, theme: Theme): Promise<void> {
  applyTheme(theme);
  dispatch({ type: "theme_set", theme });
  try {
    await cmd.settingSet("theme", theme);
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The theme was not saved: ${asGuiError(e).message}`, level: "error" });
  }
}
