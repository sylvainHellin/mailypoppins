// Regenerate src/keymap/keymap.json from `mp dump-keys --json`, the same
// KEYMAP data the TUI help overlay and the website read.
//
//   pnpm gen:keymap                 MP_BIN, else `mp` on PATH; fails without one
//   node scripts/gen-keymap.ts --if-bin
//                                   the prebuild hook: regenerates only when
//                                   MP_BIN is set, else keeps the committed
//                                   file, so a checkout builds without mp and
//                                   an older `mp` on PATH never rewrites it

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

type Binding = { key: string; action: string };
type Section = { title: string; bindings: Binding[] };

const out = join(dirname(fileURLToPath(import.meta.url)), "..", "src/keymap/keymap.json");
const optional = process.argv.includes("--if-bin");
const bin = process.env.MP_BIN;

if (optional && !bin) {
  console.log("gen-keymap: MP_BIN unset, keeping the committed src/keymap/keymap.json");
  process.exit(0);
}

let raw: string;
try {
  raw = execFileSync(bin ?? "mp", ["dump-keys", "--json"], { encoding: "utf8" });
} catch (e) {
  console.error(`gen-keymap: could not run \`${bin ?? "mp"} dump-keys --json\`: ${String(e)}`);
  process.exit(1);
}

const parsed: unknown = JSON.parse(raw);
if (
  !Array.isArray(parsed) ||
  !parsed.every(
    (s: Section) =>
      typeof s.title === "string" &&
      Array.isArray(s.bindings) &&
      s.bindings.every((b) => typeof b.key === "string" && typeof b.action === "string"),
  )
) {
  console.error("gen-keymap: dump-keys did not answer [{title, bindings: [{key, action}]}]");
  process.exit(1);
}

const next = `${JSON.stringify(parsed, null, 2)}\n`;
let prev = "";
try {
  prev = readFileSync(out, "utf8");
} catch {
  // first generation
}
if (prev !== next) {
  writeFileSync(out, next);
  console.log(`gen-keymap: wrote ${(parsed as Section[]).length} sections to src/keymap/keymap.json`);
} else {
  console.log("gen-keymap: src/keymap/keymap.json is current");
}
