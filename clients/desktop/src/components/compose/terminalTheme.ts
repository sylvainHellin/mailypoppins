// The embedded terminal's colours and font, read from the design tokens of
// src/index.css (docs/design-tokens.md, "Terminal"). The palette values live
// in index.css alone; this file only names the tokens each xterm slot takes.

import type { ITheme } from "@xterm/xterm";

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

/** The app's mono font, as the host's `font-mono text-sm` classes resolve it. */
export function terminalFont(host: Element): { fontFamily: string; fontSize: number } {
  const style = getComputedStyle(host);
  const size = Number.parseFloat(style.fontSize);
  return {
    fontFamily: style.fontFamily.trim() || "monospace",
    fontSize: Number.isFinite(size) && size > 0 ? size : 14,
  };
}
