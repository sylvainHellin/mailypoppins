// The embedded terminal's colours and font, read from the design tokens of
// src/index.css (docs/design-tokens.md, "Terminal"). The palette values live
// in index.css alone; this file only names the tokens each xterm slot takes.

import type { ITheme } from "@xterm/xterm";
import { DEFAULT_TERMINAL_FONT, type TerminalFontSetting } from "@/lib/terminal-font";

/** xterm's theme slot, and the token that fills it. */
export const TERMINAL_TOKENS = {
  background: "--background",
  foreground: "--foreground",
  cursor: "--foreground",
  cursorAccent: "--background",
  selectionBackground: "--selection",
  selectionForeground: "--selection-foreground",
  selectionInactiveBackground: "--muted",
  scrollbarSliderBackground: "--border",
  scrollbarSliderHoverBackground: "--input",
  scrollbarSliderActiveBackground: "--input",
  black: "--terminal-black",
  red: "--terminal-red",
  green: "--terminal-green",
  yellow: "--terminal-yellow",
  blue: "--terminal-blue",
  magenta: "--terminal-magenta",
  cyan: "--terminal-cyan",
  white: "--terminal-white",
  brightBlack: "--terminal-bright-black",
  brightRed: "--terminal-bright-red",
  brightGreen: "--terminal-bright-green",
  brightYellow: "--terminal-bright-yellow",
  brightBlue: "--terminal-bright-blue",
  brightMagenta: "--terminal-bright-magenta",
  brightCyan: "--terminal-bright-cyan",
  brightWhite: "--terminal-bright-white",
} satisfies Partial<Record<keyof ITheme, string>>;

/**
 * The theme the palette on `<html>` paints now. A token the page does not
 * define (a test's jsdom has no stylesheet) leaves its slot to xterm's default.
 */
export function terminalTheme(root: Element = document.documentElement): ITheme {
  const style = getComputedStyle(root);
  const theme: Record<string, string> = {};
  for (const [slot, token] of Object.entries(TERMINAL_TOKENS)) {
    const value = style.getPropertyValue(token).trim();
    if (value) theme[slot] = value;
  }
  return theme as ITheme;
}

/**
 * The Nerd Fonts symbols the app ships, declared in terminal-font.css for the
 * private use areas alone, where editor plugins put their icons.
 */
export const SYMBOLS_FONT = "Symbols Nerd Font Mono";

/** A character of the symbols font's range (nf-md-note), for the load to name. */
const SYMBOLS_SAMPLE = "\u{F0188}";

/**
 * The app's mono font, as the host's `font-mono text-sm` classes resolve it,
 * with the symbols font after it for the icons no system mono font carries.
 * The terminal font setting (src/app/terminalFont.ts) puts its family, in
 * double quotes, before that stack, and its size in place of the host's; its
 * family holds no quote, comma or backslash, so the quotes always close.
 */
export function terminalFont(
  host: Element,
  setting: TerminalFontSetting = DEFAULT_TERMINAL_FONT,
): { fontFamily: string; fontSize: number } {
  const style = getComputedStyle(host);
  const size = Number.parseFloat(style.fontSize);
  const chosen = setting.family ? `"${setting.family}", ` : "";
  return {
    fontFamily: `${chosen}${style.fontFamily.trim() || "monospace"}, "${SYMBOLS_FONT}"`,
    fontSize: setting.size ?? (Number.isFinite(size) && size > 0 ? size : 14),
  };
}

/**
 * The symbols font's load at each size, per page: settled `true` once it
 * arrived, `false` once it failed. Shared, so the panes waiting on one arrival
 * see it in one batch. A failed load is not retried: the font is a bundled
 * file, and a later arrival would rebuild an atlas the panes that saw the
 * failure still draw from.
 */
const symbolsLoads = new WeakMap<FontFaceSet, Map<number, { arrived: boolean; load: Promise<boolean> }>>();

/**
 * Fetches the symbols font at a size, unless the page has it there already.
 * Text drawn on a canvas, as xterm's WebGL renderer draws it, never starts the
 * download of a `unicode-range` face, so the pane asks for it by name with a
 * character of the range.
 *
 * Returns null when there is nothing to wait for: the font is there already,
 * or the page has no FontFaceSet (jsdom). Otherwise the promise of the one
 * load at that size every pane shares, `true` when the font arrives and
 * `false` when it failed, which warns once in the console.
 */
export function symbolsFontArrival(fontSize: number): Promise<boolean> | null {
  const fonts: FontFaceSet | undefined = document.fonts;
  if (!fonts) return null;
  let bySize = symbolsLoads.get(fonts);
  if (!bySize) symbolsLoads.set(fonts, (bySize = new Map()));
  const known = bySize.get(fontSize);
  if (known) return known.arrived ? null : known.load;
  const font = `${fontSize}px "${SYMBOLS_FONT}"`;
  if (fonts.check?.(font, SYMBOLS_SAMPLE)) {
    bySize.set(fontSize, { arrived: true, load: Promise.resolve(true) });
    return null;
  }
  const entry = { arrived: false, load: Promise.resolve(false) };
  entry.load = fonts.load(font, SYMBOLS_SAMPLE).then(
    () => {
      entry.arrived = true;
      return true;
    },
    (e: unknown) => {
      console.warn("the terminal's symbols font did not load; Nerd Font icons draw blank", e);
      return false;
    },
  );
  bySize.set(fontSize, entry);
  return entry.load;
}
