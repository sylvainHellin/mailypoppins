// A build plugin that fails `vite build` when any module under src/fixture/
// is loaded: the fixture backend is for the tests and the browser preview
// only (docs/browser-preview.md). It catches an import of a single export,
// which tree-shaking would otherwise reduce to nothing check-bundle.ts can
// find in dist/. vite.config.ts adds it; the dev server and vitest skip it.

import path from "node:path";
import type { Plugin } from "vite";

export const FIXTURE_GUARD = "mp-fixture-guard";

export function fixtureGuard(src: string): Plugin {
  const dir = path.join(src, "fixture") + path.sep;
  return {
    name: FIXTURE_GUARD,
    apply: "build",
    load(id) {
      const file = id.split("?")[0];
      if (file.startsWith(dir)) {
        throw new Error(
          `${path.relative(path.dirname(src), file)} is part of the fixture backend, which only the tests and the browser preview may load; an app module imports it`,
        );
      }
      return null;
    },
  };
}
