// Build the app bundle and DMG with the matching `mp` inside it, as the
// release workflow does (#0132).
//
//   pnpm bundle                                  the host's target triple
//   pnpm bundle --target x86_64-apple-darwin     another one (cross-compiled)
//   MP_SIDECAR_BIN=/path/to/mp pnpm bundle       that binary instead of a fresh build
//   pnpm bundle -- --bundles app                 anything after `--` goes to `tauri build`
//
// 1. Builds the root crate's `mp` in release for the target (cargo honours
//    CARGO_TARGET_DIR), unless MP_SIDECAR_BIN names one.
// 2. Copies it to src-tauri/binaries/mp-<target>, the name `bundle.externalBin`
//    wants; the bundler drops the suffix, so the app holds Contents/MacOS/mp,
//    which is where connector.rs looks first.
// 3. Runs `tauri build --target <target>` with src-tauri/tauri.bundle.conf.json
//    (the externalBin entry, kept out of tauri.conf.json so `tauri dev` and
//    `cargo test` need no sidecar) and the bundle's version set to mp's.

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const desktop = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(desktop, "../..");

const args = process.argv.slice(2);
const dashdash = args.indexOf("--");
const own = dashdash === -1 ? args : args.slice(0, dashdash);
const passthrough = dashdash === -1 ? [] : args.slice(dashdash + 1);
const at = own.indexOf("--target");
const target =
  at === -1
    ? execFileSync("rustc", ["--print", "host-tuple"], { encoding: "utf8" }).trim()
    : own[at + 1];
if (!target) {
  console.error("bundle: --target needs a target triple");
  process.exit(1);
}

let bin = process.env.MP_SIDECAR_BIN;
if (!bin) {
  console.log(`bundle: building mp for ${target}`);
  execFileSync("cargo", ["build", "--release", "--locked", "--bin", "mp", "--target", target], {
    cwd: root,
    stdio: "inherit",
  });
  bin = join(process.env.CARGO_TARGET_DIR ?? join(root, "target"), target, "release", "mp");
}
if (!existsSync(bin)) {
  console.error(`bundle: no mp at ${bin}`);
  process.exit(1);
}

// The version the bundle carries: mp's own, so the app, its DMG name and the
// daemon it starts agree. A cross-compiled binary cannot run here, so the
// root crate's manifest is the fallback.
let version: string;
try {
  version = execFileSync(bin, ["--version"], { encoding: "utf8" }).trim().split(/\s+/).pop() ?? "";
} catch {
  version = execFileSync("cargo", ["pkgid", "--manifest-path", join(root, "Cargo.toml"), "-p", "mailypoppins"], {
    encoding: "utf8",
  })
    .trim()
    .split(/[#@]/)
    .pop() ?? "";
}
if (!/^\d/.test(version)) {
  console.error(`bundle: could not read mp's version (got ${JSON.stringify(version)})`);
  process.exit(1);
}

const binaries = join(desktop, "src-tauri", "binaries");
mkdirSync(binaries, { recursive: true });
const sidecar = join(binaries, `mp-${target}`);
copyFileSync(bin, sidecar);
chmodSync(sidecar, 0o755);
console.log(`bundle: ${bin} (mailypoppins ${version}) -> ${sidecar}`);

execFileSync(
  "pnpm",
  [
    "exec",
    "tauri",
    "build",
    "--target",
    target,
    "--config",
    join(desktop, "src-tauri", "tauri.bundle.conf.json"),
    "--config",
    JSON.stringify({ version }),
    ...passthrough,
  ],
  { cwd: desktop, stdio: "inherit" },
);
const out = join(process.env.CARGO_TARGET_DIR ?? join(desktop, "src-tauri", "target"), target, "release", "bundle");
console.log(`bundle: done, in ${out}`);
