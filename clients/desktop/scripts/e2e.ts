// Build the debug app with the embedded WebDriver server and run the
// end-to-end specs against it on the fixtures (PERSO-271, e2e/).
//
//   pnpm test:e2e                          build, then run every spec
//   pnpm test:e2e --no-build               run against the last build
//   pnpm test:e2e -- --spec e2e/specs/02-reader.e2e.ts   anything after `--` goes to `wdio run`
//
// 1. `tauri build --debug --no-bundle --features e2e`: the frontend through
//    `pnpm build` (with its fixture check), embedded in a debug binary that
//    carries the `e2e` feature, which src/lib.rs refuses in a release build.
// 2. `wdio run e2e/wdio.conf.ts` with E2E_APP naming that binary; the service
//    starts it with `--fixture` and a scratch home under /var/tmp. Its window
//    is transparent, click-through and behind every other window, and the
//    app never becomes the active one (src-tauri/src/e2e.rs), so a run
//    leaves whoever works on the Mac undisturbed.
//
// The binary lands in <target dir>/debug/mp-desktop, the target dir being
// $CARGO_TARGET_DIR or src-tauri/target, as for every other build here.

import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const args = process.argv.slice(2);
const dashdash = args.indexOf("--");
const own = dashdash === -1 ? args : args.slice(0, dashdash);
const passthrough = dashdash === -1 ? [] : args.slice(dashdash + 1);
const unknown = own.filter((a) => a !== "--no-build");
if (unknown.length > 0) {
  console.error(`e2e: unknown argument ${unknown.join(" ")}; wdio's own go after \`--\``);
  process.exit(2);
}

const target = process.env.CARGO_TARGET_DIR ?? join(desktop, "src-tauri", "target");
const app = join(target, "debug", "mp-desktop");

if (!own.includes("--no-build")) {
  console.log(`e2e: building the debug app with the e2e feature into ${target}`);
  execFileSync("pnpm", ["exec", "tauri", "build", "--debug", "--no-bundle", "--features", "e2e"], {
    cwd: desktop,
    stdio: "inherit",
  });
}
if (!existsSync(app)) {
  console.error(`e2e: no app at ${app}; run without --no-build`);
  process.exit(1);
}

const scratch = mkdtempSync("/var/tmp/mp-e2e-run-");
console.log(`e2e: running the specs against ${app}, in the background; scratch and logs in ${scratch}`);
try {
  execFileSync("pnpm", ["exec", "wdio", "run", join(desktop, "e2e", "wdio.conf.ts"), ...passthrough], {
    cwd: desktop,
    stdio: "inherit",
    env: { ...process.env, E2E_APP: app, E2E_SCRATCH: scratch },
  });
} catch {
  process.exit(1);
}
