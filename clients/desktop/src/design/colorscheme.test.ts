/// <reference types="node" />
// The embedded editor's colorscheme (ticket 0137, U2) carries the design
// tokens of both palettes as hex values, since Vim reads no CSS: this keeps
// them equal to src/index.css, and keeps every hex value of the file inside
// its two token blocks.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { PALETTES, paletteTokens } from "@/design/contrast";

const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf8");
const vim = readFileSync(resolve(process.cwd(), "src-tauri/resources/nvim/colors/mailypoppins.vim"), "utf8");

/** The `'name': '#hex'` entries of the dictionary `let s:<name> = { ... }`. */
function vimTokens(name: string): Record<string, string> {
  const start = vim.indexOf(`let s:${name} = {`);
  if (start < 0) throw new Error(`no s:${name} in the colorscheme`);
  const end = vim.indexOf("\\ }", start);
  const out: Record<string, string> = {};
  for (const m of vim.slice(start, end).matchAll(/'([a-z-]+)':\s*'(#[0-9A-Fa-f]{6})'/g)) out[m[1]] = m[2];
  return out;
}

/** The ANSI index of each `--terminal-*` slot, as xterm numbers them. */
const ANSI = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].flatMap((c, i) => [
  [`terminal-${c}`, i] as const,
  [`terminal-bright-${c}`, i + 8] as const,
]);

const BASE = ["background", "foreground", "muted", "accent", "primary", "border", "destructive", "selection"];

describe("the mailypoppins colorscheme", () => {
  for (const palette of PALETTES) {
    it(`carries the ${palette.name} palette of index.css`, () => {
      const tokens = paletteTokens(css, palette.name);
      const ours = vimTokens(palette.name);
      const names = [...BASE, ...ANSI.map(([name]) => name)];
      expect(Object.keys(ours).sort()).toEqual([...names].sort());
      for (const name of names) {
        expect(ours[name]?.toUpperCase(), `--${name}`).toBe(tokens[name]?.toUpperCase());
      }
    });
  }

  it("has no hex value outside its two token blocks", () => {
    const outside = vim
      .split("\n")
      .filter((line) => /#[0-9A-Fa-f]{3,8}\b/.test(line))
      .filter((line) => !/^\s*\\ '[a-z-]+': '#[0-9A-Fa-f]{6}',$/.test(line));
    expect(outside).toEqual([]);
  });

  it("gives a terminal slot its own ANSI index in cterm", () => {
    const uses = [...vim.matchAll(/\[s:t\['(terminal-[a-z-]+)'\], (\d+)\]/g)];
    expect(uses.length).toBeGreaterThan(5);
    const index = new Map<string, number>(ANSI);
    for (const [, name, cterm] of uses) expect(Number(cterm), name).toBe(index.get(name));
  });
});
