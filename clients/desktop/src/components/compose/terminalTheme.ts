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
 * Fetches the symbols font. Text drawn on a canvas, as xterm's WebGL renderer
 * draws it, never starts the download of a `unicode-range` face, so the pane
 * asks for it by name with a character of the range. Resolves at once where
 * the page has no FontFaceSet (jsdom); rejects when the file fails to load.
 */
export async function loadSymbolsFont(fontSize: number): Promise<void> {
  const fonts: FontFaceSet | undefined = document.fonts;
  if (!fonts) return;
  await fonts.load(`${fontSize}px "${SYMBOLS_FONT}"`, SYMBOLS_SAMPLE);
}
