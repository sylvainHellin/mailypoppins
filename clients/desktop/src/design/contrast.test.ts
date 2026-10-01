/// <reference types="node" />
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { blockTokens, contrast, evaluate, MINIMUM, PALETTES, paletteTokens, rootTokens } from "@/design/contrast";

const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf8");
const doc = readFileSync(resolve(process.cwd(), "docs/design-tokens.md"), "utf8");

describe("the token contrast pass", () => {
  it("computes WCAG ratios", () => {
    expect(contrast("#000000", "#FFFFFF")).toBeCloseTo(21, 5);
    expect(contrast("#777777", "#777777")).toBeCloseTo(1, 5);
  });

  it("reads each palette from its own block", () => {
    expect(rootTokens(css).foreground).toBe("#F4F1E8");
    expect(paletteTokens(css, "light").foreground).toBe("#0C1B33");
    expect(paletteTokens(css, "light")["reader-canvas"]).toBe("#FFFFFF");
  });

  it("gives the light palette every colour of the dark one but the reader canvas", () => {
    const light = blockTokens(css, ":root.light");
    const missing = Object.keys(rootTokens(css)).filter((name) => name !== "reader-canvas" && !(name in light));
    expect(missing).toEqual([]);
  });

  for (const palette of PALETTES) {
    describe(`the ${palette.name} palette`, () => {
      const rows = evaluate(paletteTokens(css, palette.name));

      it("keeps every pair at or above its minimum", () => {
        const failing = rows
          .filter((r) => !r.pass)
          .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)} < ${MINIMUM[r.need]}`);
        expect(failing).toEqual([]);
      });

      it("documents every pair with its current ratio (run `pnpm contrast` after a token change)", () => {
        const section = doc.slice(doc.indexOf(`### ${palette.title}\n`));
        for (const r of rows) {
          expect(section).toContain(`| \`${r.fg}\` | \`${r.bg}\` | \`${r.fgHex}\` on \`${r.bgHex}\` | ${r.ratio.toFixed(2)}:1 |`);
        }
      });
    });
  }
});
