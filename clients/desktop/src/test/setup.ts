import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { PREFS_KEY } from "@/app/prefs";
import { FLAT_PREFS } from "@/test/prefs";

// No test reaches the real Tauri bridge: src/lib/tauri.ts is the one module
// that imports @tauri-apps/api, and every test gets the fixture-backed mock.
vi.mock("@/lib/tauri", () => import("@/test/tauri-mock"));

/** The window width the matchMedia stub answers against. */
export function setWidth(px: number): void {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: px });
  window.dispatchEvent(new Event("resize"));
}

function matchMedia(query: string): MediaQueryList {
  const m = /\(min-width:\s*(\d+)px\)/.exec(query);
  const matches = m ? window.innerWidth >= Number(m[1]) : false;
  return {
    matches,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  };
}
Object.defineProperty(window, "matchMedia", { configurable: true, writable: true, value: matchMedia });

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver ??= ResizeObserverStub as unknown as typeof ResizeObserver;
Element.prototype.scrollIntoView ??= function scrollIntoView() {};
Element.prototype.hasPointerCapture ??= () => false;
Element.prototype.setPointerCapture ??= () => {};
Element.prototype.releasePointerCapture ??= () => {};

// The whole app reads its prefs from localStorage: it starts with the fixture
// accounts listed flat, as the tests written before the conversation list
// expect; a test of that list renders with `initialState()` (ticket 0142).
beforeEach(() => {
  localStorage.setItem(PREFS_KEY, JSON.stringify(FLAT_PREFS));
});

afterEach(() => {
  cleanup();
  localStorage.clear();
  setWidth(1400);
});
setWidth(1400);
