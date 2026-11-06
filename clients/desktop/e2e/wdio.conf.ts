// WebdriverIO's configuration for the end-to-end specs (PERSO-271): the
// debug app built with the `e2e` feature, on the fixtures, driven through the
// WebDriver server the feature embeds (`tauri-plugin-wdio-webdriver`).
// scripts/e2e.ts builds the app and runs this; E2E_APP names the binary.
//
// Every run gets its own scratch directory under /var/tmp, and the app sees
// it as its home, its temp dir (the fixture's draft files), its data and
// config directories: no daemon, no network, nothing of the real mailbox.

import { browser } from "@wdio/globals";
import type { TauriCapabilities } from "@wdio/tauri-service";
import { mkdirSync } from "node:fs";
import { join } from "node:path";

// Both set by scripts/e2e.ts: the launcher and every worker load this file,
// so the scratch directory is made once, before any of them.
const app = process.env.E2E_APP;
const scratch = process.env.E2E_SCRATCH;
if (!app || !scratch) throw new Error("E2E_APP and E2E_SCRATCH are unset; run `pnpm test:e2e`");
const dir = (name: string) => {
  const path = join(scratch, name);
  mkdirSync(path, { recursive: true });
  return path;
};

const env: Record<string, string> = {
  HOME: dir("home"),
  TMPDIR: dir("tmp"),
  MAILYPOPPINS_DATA_DIR: dir("data"),
  MAILYPOPPINS_CONFIG_DIR: dir("config"),
  MAILYPOPPINS_DAEMON_AUTOSTART: "0",
  MP_DESKTOP_FIXTURE: "1",
  MP_DESKTOP_STUB_OPENER: "1",
  // A GUI editor, so a draft never opens in the embedded terminal; the
  // fixture runs no editor anyway, it journals the command.
  MP_DESKTOP_EDITOR: "open -t {path}",
  MP_DESKTOP_WINDOW_SIZE: "1400x900",
  MP_DESKTOP_LOG: "debug",
};

export const config: WebdriverIO.Config = {
  runner: "local",
  specs: ["./specs/**/*.e2e.ts"],
  // One app per run: the service starts it once in the launcher's
  // `onPrepare`, and each spec file gets its own worker and session on that
  // app, one file after another, so the files share the fixture's state but
  // not their module-level variables.
  maxInstances: 1,
  capabilities: [
    ({
      browserName: "tauri",
      "tauri:options": { application: app },
      "wdio:tauriServiceOptions": {
        driverProvider: "embedded",
        appArgs: ["--fixture"],
        env,
        captureBackendLogs: true,
        backendLogLevel: "info",
      },
    } satisfies TauriCapabilities) as WebdriverIO.Capabilities,
  ],
  services: [["@wdio/tauri-service", {}]],
  logLevel: "warn",
  outputDir: join(scratch, "logs"),
  bail: 0,
  waitforTimeout: 10_000,
  connectionRetryTimeout: 60_000,
  connectionRetryCount: 1,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: { ui: "bdd", timeout: 60_000 },
  async before() {
    // The app has one window. An explicit switch to it turns off the
    // service's focus recovery, which asks `tauri-plugin-wdio` (not in this
    // app) for the window states before every find and waits 5 s for an
    // answer that never comes.
    await browser.switchToWindow(await browser.getWindowHandle());
  },
};
