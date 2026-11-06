// Fail a build that carries test-only code into what ships.
//
// - dist/, the frontend bundle: the fixture backend (src/fixture/), which only
//   the tests and the browser preview may load, and any JS side of the
//   end-to-end tests' WebDriver (`@wdio/*`, its `__wdio*` globals, a
//   `plugin:wdio` command). `pnpm build` runs this after `vite build`, and
//   `tauri build` runs `pnpm build`. The build's own guard for the fixture is
//   scripts/fixture-guard.ts, which fails `vite build` on any module under
//   src/fixture/; this scan is the backstop behind it.
// - With `--app`, the executables of a built app (Contents/MacOS/*) or one
//   binary: the `e2e` cargo feature's embedded WebDriver server
//   (tauri-plugin-wdio-webdriver) and its window hooks (src-tauri/src/e2e.rs).
//   src-tauri/src/lib.rs refuses the feature in a release build, and
//   scripts/bundle.ts runs this on the app it built: the backstop for a
//   `--debug` bundle, or a release profile with debug assertions on.
//
//   node scripts/check-bundle.ts                checks dist/, fails without a build
//   node scripts/check-bundle.ts --app <path>   checks that .app or binary instead

import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

/** What a check looks for: strings only that code holds, which survive minification or `strip`. */
type Markers = { what: string; strings: string[] };

const FIXTURE: Markers = {
  what: "the fixture backend",
  strings: ["the fixture backend does not answer", "fixture-instance-1", "mpFixture"],
};
const WDIO_JS: Markers = {
  what: "the end-to-end tests' WebDriver JS",
  strings: ["__wdio", "plugin:wdio", "@wdio/"],
};
const WDIO_NATIVE: Markers = {
  what: "the e2e feature's WebDriver server",
  // The plugin's name and port variable, and e2e.rs's WebKit switch.
  strings: ["wdio-webdriver", "TAURI_WEBDRIVER_PORT", "_setWindowOcclusionDetectionEnabled"],
};

function fail(message: string): never {
  console.error(`check-bundle: ${message}`);
  process.exit(1);
}

/** A path relative to the working directory when it is under it, else as given. */
function shown(file: string): string {
  const path = relative(process.cwd(), file);
  return path.startsWith("..") ? file : path;
}

/** `<file>: "<marker>"` for each marker of each set found in each file. */
function scan(files: string[], sets: Markers[]): { what: string; hit: string }[] {
  return files.flatMap((file) => {
    const bytes = readFileSync(file);
    return sets.flatMap(({ what, strings }) =>
      strings.filter((s) => bytes.includes(s)).map((s) => ({ what, hit: `${shown(file)}: "${s}"` })),
    );
  });
}

function report(files: string[], sets: Markers[], where: string): void {
  const found = scan(files, sets);
  if (found.length > 0) {
    const whats = [...new Set(found.map((f) => f.what))].join(" and ");
    fail(`${where} carries ${whats}:\n  ${found.map((f) => f.hit).join("\n  ")}`);
  }
  console.log(`check-bundle: ${files.length} files in ${where}, none carries ${sets.map((s) => s.what).join(" or ")}`);
}

const args = process.argv.slice(2);
const at = args.indexOf("--app");
if (at !== -1) {
  const app = args[at + 1];
  if (!app) fail("--app needs the path of a .app or a binary");
  let files: string[];
  try {
    const macos = join(app, "Contents", "MacOS");
    files = statSync(app).isDirectory()
      ? readdirSync(macos).map((f) => join(macos, f)).filter((f) => statSync(f).isFile())
      : [app];
  } catch (e) {
    fail(`no app to check at ${app}: ${String(e)}`);
  }
  if (files.length === 0) fail(`${app} holds no executable`);
  report(files, [WDIO_NATIVE], app);
} else {
  if (args.length > 0) fail(`unknown argument ${args.join(" ")}`);
  const dist = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");
  let files: string[];
  try {
    files = readdirSync(dist, { recursive: true, encoding: "utf8" })
      .filter((f) => /\.(js|html)$/.test(f))
      .map((f) => join(dist, f));
  } catch (e) {
    fail(`no bundle to check at ${dist}: ${String(e)}`);
  }
  if (files.length === 0) fail(`${dist} holds no .js or .html file`);
  report(files, [FIXTURE, WDIO_JS], "dist/");
}
