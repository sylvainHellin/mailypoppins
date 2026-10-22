// The vitest stand-in for src/lib/tauri.ts (src/test/setup.ts mocks the module
// with this one): the fixture backend, its entry points wrapped in `vi.fn` so
// a test can read their calls or replace an answer.

import { vi } from "vitest";
import * as backend from "@/fixture/backend";

export * from "@/fixture/backend";

const mockWindow = {
  onCloseRequested: vi.fn(backend.fixtureWindow.onCloseRequested),
  destroy: vi.fn(backend.fixtureWindow.destroy),
};

export const getCurrentWindow = () => mockWindow;
export const invoke = vi.fn(backend.invoke);
export const homeDir = vi.fn(backend.homeDir);
export const openPicker = vi.fn(backend.openPicker);
export const listen = vi.fn(backend.listen);
