// Fail the build when the bundle in dist/ carries the fixture backend
// (src/fixture/), which only the tests and the browser preview may load.
// `pnpm build` runs it after `vite build`, and `tauri build` runs `pnpm build`.
//
//   node scripts/check-bundle.ts          checks dist/, fails without a build

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

/** Strings only the fixture backend and the preview hold, which survive minification. */
const MARKERS = ["the fixture backend does not answer", "fixture-instance-1", "mpFixture"];

const dist = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");

let files: string[];
try {
  files = readdirSync(dist, { recursive: true, encoding: "utf8" }).filter((f) => /\.(js|html)$/.test(f));
} catch (e) {
  console.error(`check-bundle: no bundle to check at ${dist}: ${String(e)}`);
  process.exit(1);
}
if (files.length === 0) {
  console.error(`check-bundle: ${dist} holds no .js or .html file`);
  process.exit(1);
}

const found = files.flatMap((f) => {
  const text = readFileSync(join(dist, f), "utf8");
  return MARKERS.filter((m) => text.includes(m)).map((m) => `${relative(process.cwd(), join(dist, f))}: "${m}"`);
});
if (found.length > 0) {
  console.error(`check-bundle: the bundle carries the fixture backend:\n  ${found.join("\n  ")}`);
  process.exit(1);
}
console.log(`check-bundle: ${files.length} files, no fixture backend`);
