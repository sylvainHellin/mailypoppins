/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

/**
 * The browser preview (`pnpm preview:browser`, docs/browser-preview.md): the
 * dev server alone, with `@/lib/tauri` answered by the fixture backend. A
 * build in this mode is refused, so no bundle can carry the backend.
 */
const PREVIEW_MODE = "browser";

// https://vite.dev/config/
export default defineConfig(({ command, mode }) => {
  const preview = mode === PREVIEW_MODE;
  if (preview && command === "build") {
    throw new Error(`the ${PREVIEW_MODE} mode is the dev server's browser preview and never builds a bundle`);
  }
  const src = path.resolve(import.meta.dirname, "./src");
  return {
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: [
        ...(preview ? [{ find: /^@\/lib\/tauri$/, replacement: path.join(src, "fixture/preview.ts") }] : []),
        { find: "@", replacement: src },
      ],
    },

    // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
    //
    // 1. prevent Vite from obscuring rust errors
    clearScreen: false,
    // 2. tauri expects a fixed port, fail if that port is not available; the
    //    preview takes its own, so it runs beside `tauri dev`
    server: {
      port: preview ? 1430 : 1420,
      strictPort: true,
      host: preview ? undefined : host || false,
      hmr:
        host && !preview
          ? {
              protocol: "ws",
              host,
              port: 1421,
            }
          : undefined,
      watch: {
        // 3. tell Vite to ignore watching `src-tauri`
        ignored: ["**/src-tauri/**"],
      },
    },

    test: {
      environment: "jsdom",
      setupFiles: ["./src/test/setup.ts"],
      css: false,
      include: ["src/**/*.test.{ts,tsx}"],
      // vitest's 5 s default times out dozens of tests at once when parallel
      // cargo builds push the load average past about 30; 20 s still catches a
      // real hang.
      testTimeout: 20_000,
      hookTimeout: 20_000,
    },
  };
});
