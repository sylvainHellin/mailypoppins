/// <reference types="node" />
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

// Vitest runs from clients/desktop (jsdom's URL cannot resolve file: URLs).
const SRC = resolve(process.cwd(), "src") + "/";
// shadcn's `components/ui` is named on its own so that no change to the walk
// can quietly exempt it; the set drops the files `components` already found.
const GUARDED = ["components", "components/ui", "app"];

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return files(p);
    return /\.(tsx?|css)$/.test(name) && !/\.test\.tsx?$/.test(name) ? [p] : [];
  });
}

// A hex colour (#abc, #aabbcc, #aabbccdd) or an arbitrary Tailwind colour class.
const HEX = /(?<![\w&/])#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})\b/;
const ARBITRARY = /\b(?:bg|text|border|ring|outline|fill|stroke|from|to|via|shadow|decoration|caret|accent)-\[#/;
const FUNCTIONAL = /\b(?:rgba?|hsla?|oklch|oklab|lab|lch)\(\s*[\d.]/;
// A Tailwind palette class (`bg-black/10`, `text-slate-400`), which bypasses the tokens.
const PALETTE =
  /\b(?:bg|text|border|ring|fill|stroke|outline|from|to|via|shadow|decoration|divide|placeholder|caret|accent)-(?:black|white|slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)(?:-\d+)?(?:\/\d+)?\b/;

describe("the colour guard", () => {
  it("finds no palette value in src/components, src/components/ui or src/app", () => {
    const offenders: string[] = [];
    const scanned = new Set(GUARDED.flatMap((dir) => files(join(SRC, dir))));
    expect([...scanned].some((f) => f.includes("/components/ui/"))).toBe(true);
    for (const file of scanned) {
      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          if (HEX.test(line) || ARBITRARY.test(line) || FUNCTIONAL.test(line) || PALETTE.test(line)) {
            offenders.push(`${file.slice(SRC.length)}:${i + 1}: ${line.trim()}`);
          }
        });
    }
    expect(offenders).toEqual([]);
  });

  it("catches what it is meant to catch", () => {
    expect(HEX.test('className="text-[#FF6700]"')).toBe(true);
    expect(ARBITRARY.test('className="bg-[#0C1B33]"')).toBe(true);
    expect(FUNCTIONAL.test("color: rgb(12, 27, 51)")).toBe(true);
    expect(HEX.test('href="#top"')).toBe(false);
    expect(PALETTE.test('className="fixed inset-0 bg-black/10"')).toBe(true);
    expect(PALETTE.test('className="text-slate-400"')).toBe(true);
    expect(PALETTE.test('className="ring-white"')).toBe(true);
    expect(PALETTE.test('className="bg-overlay text-muted-foreground"')).toBe(false);
  });
});
