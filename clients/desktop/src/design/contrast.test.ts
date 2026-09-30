/// <reference types="node" />
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { contrast, evaluate, MINIMUM, rootTokens } from "@/design/contrast";

const css = readFileSync(resolve(process.cwd(), "src/index.css"), "utf8");
const doc = readFileSync(resolve(process.cwd(), "docs/design-tokens.md"), "utf8");

describe("the token contrast pass", () => {
  it("computes WCAG ratios", () => {
    expect(contrast("#000000", "#FFFFFF")).toBeCloseTo(21, 5);
    expect(contrast("#777777", "#777777")).toBeCloseTo(1, 5);
  });

  it("keeps every pair at or above its minimum", () => {
    const failing = evaluate(rootTokens(css))
      .filter((r) => !r.pass)
      .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)} < ${MINIMUM[r.need]}`);
    expect(failing).toEqual([]);
  });

  it("documents every pair with its current ratio (run `pnpm contrast` after a token change)", () => {
    for (const r of evaluate(rootTokens(css))) {
      expect(doc).toContain(`| \`${r.fg}\` | \`${r.bg}\` | \`${r.fgHex}\` on \`${r.bgHex}\` | ${r.ratio.toFixed(2)}:1 |`);
    }
  });
});
