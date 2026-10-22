import path from "node:path";
import { describe, expect, it } from "vitest";
import type { ConfigEnv, Plugin, UserConfig } from "vite";
import config from "../vite.config";
import { FIXTURE_GUARD, fixtureGuard } from "./fixture-guard";

const src = path.resolve(import.meta.dirname, "../src");

function load(plugin: Plugin, id: string): unknown {
  const hook = plugin.load as (this: unknown, id: string) => unknown;
  return hook.call({}, id);
}

describe("the fixture guard", () => {
  it("fails a build that loads any module under src/fixture/, a single export's included", () => {
    const guard = fixtureGuard(src);
    expect(guard.apply).toBe("build");
    for (const file of ["fixture/backend.ts", "fixture/terminal-bridge.ts", "fixture/preview.ts"]) {
      expect(() => load(guard, path.join(src, file))).toThrow(/fixture backend/);
      expect(() => load(guard, `${path.join(src, file)}?v=1`)).toThrow(/fixture backend/);
    }
    for (const file of ["lib/tauri.ts", "components/compose/TerminalPane.fixture.tsx", "fixture.ts"]) {
      expect(load(guard, path.join(src, file))).toBeNull();
    }
  });

  it("is one of the build's plugins", () => {
    const make = config as (env: ConfigEnv) => UserConfig;
    const plugins = (make({ command: "build", mode: "production" }).plugins ?? []).flat() as Plugin[];
    expect(plugins.map((p) => p?.name)).toContain(FIXTURE_GUARD);
  });
});
