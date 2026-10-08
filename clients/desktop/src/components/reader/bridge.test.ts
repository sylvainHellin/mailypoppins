// The reader frame's bridge (src-tauri/src/reader.rs `BRIDGE`), read out of
// the Rust source and run in a jsdom child frame: what it obeys, what it
// forwards, and what it ignores. jsdom enforces no sandbox and no CSP, so
// these check the script's own logic only; docs/reader.md lists the webview
// cases.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

const rust = readFileSync(resolve(process.cwd(), "src-tauri/src/reader.rs"), "utf8");
const BRIDGE = /pub const BRIDGE: &str = r#"(.*?)"#;/s.exec(rust)?.[1] ?? "";

/** A child frame running the bridge, with a scrolling element jsdom does not provide. */
function bridgedFrame() {
  const frame = document.createElement("iframe");
  document.body.appendChild(frame);
  const win = frame.contentWindow as Window & typeof globalThis;
  const doc = frame.contentDocument as Document;
  const scroller = doc.documentElement;
  Object.defineProperty(doc, "scrollingElement", { configurable: true, value: scroller });
  const script = doc.createElement("script");
  script.textContent = BRIDGE;
  doc.head.appendChild(script);
  // jsdom's own window, which vitest's global `window` only mirrors.
  const parent = win.parent;
  const fromParent = (data: unknown, source: Window | null = parent) =>
    win.dispatchEvent(new win.MessageEvent("message", { data, source }));
  const press = (init: KeyboardEventInit) => {
    const e = new win.KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
    doc.body.dispatchEvent(e);
    return e;
  };
  const posted = () => vi.spyOn(parent, "postMessage").mockImplementation(() => {});
  return { frame, win, doc, scroller, fromParent, press, posted };
}

afterEach(() => {
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

describe("the reader bridge", () => {
  it("is found in the Rust source", () => {
    expect(BRIDGE.length).toBeGreaterThan(100);
    expect(BRIDGE).not.toMatch(/<\/script/i);
  });

  it("scrolls by a finite distance and to either end, on the parent's messages only", () => {
    const { win, scroller, fromParent } = bridgedFrame();
    fromParent({ type: "scroll", dy: 48 });
    expect(scroller.scrollTop).toBe(48);
    fromParent({ type: "scroll", dy: -20 });
    expect(scroller.scrollTop).toBe(28);
    fromParent({ type: "scrollTo", y: "top" });
    expect(scroller.scrollTop).toBe(0);
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 900 });
    fromParent({ type: "scrollTo", y: "bottom" });
    expect(scroller.scrollTop).toBe(900);

    scroller.scrollTop = 10;
    for (const data of [
      { type: "scroll", dy: Infinity },
      { type: "scroll", dy: NaN },
      { type: "scroll", dy: "30" },
      { type: "scroll" },
      { type: "scrollTo", y: "middle" },
      { type: "key", key: "j" },
      "scroll",
      null,
    ]) {
      fromParent(data);
    }
    // From the frame itself, or from no window: ignored.
    fromParent({ type: "scroll", dy: 48 }, win);
    fromParent({ type: "scroll", dy: 48 }, null);
    expect(scroller.scrollTop).toBe(10);
  });

  it("forwards each key to the parent with its modifiers, and keeps the frame from scrolling natively", () => {
    const { press, posted } = bridgedFrame();
    const post = posted();
    const j = press({ key: "j", code: "KeyJ" });
    expect(j.defaultPrevented).toBe(true);
    const space = press({ key: " ", code: "Space" });
    expect(space.defaultPrevented).toBe(true);
    const down = press({ key: "ArrowDown", code: "ArrowDown", shiftKey: true });
    expect(down.defaultPrevented).toBe(true);
    expect(post.mock.calls).toEqual([
      [{ type: "key", key: "j", code: "KeyJ", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false }, "*"],
      [{ type: "key", key: " ", code: "Space", ctrlKey: false, metaKey: false, altKey: false, shiftKey: false }, "*"],
      [
        { type: "key", key: "ArrowDown", code: "ArrowDown", ctrlKey: false, metaKey: false, altKey: false, shiftKey: true },
        "*",
      ],
    ]);
  });

  it("keeps the default of a Ctrl or Cmd combination, so copy still works, and forwards it all the same", () => {
    const { press, posted } = bridgedFrame();
    const post = posted();
    expect(press({ key: "c", metaKey: true }).defaultPrevented).toBe(false);
    expect(press({ key: "d", ctrlKey: true }).defaultPrevented).toBe(false);
    expect(post).toHaveBeenCalledTimes(2);
    expect(post.mock.calls[1][0]).toMatchObject({ type: "key", key: "d", ctrlKey: true });
  });

  it("does nothing outside a frame", () => {
    const scroller = document.documentElement;
    Object.defineProperty(document, "scrollingElement", { configurable: true, value: scroller });
    const post = vi.spyOn(window, "postMessage").mockImplementation(() => {});
    const script = document.createElement("script");
    script.textContent = BRIDGE;
    document.head.appendChild(script);
    const e = new KeyboardEvent("keydown", { key: "j", bubbles: true, cancelable: true });
    document.body.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(false);
    expect(post).not.toHaveBeenCalled();
  });

  it("skips a key typed into an IME composition", () => {
    const { press, posted } = bridgedFrame();
    const post = posted();
    expect(press({ key: "a", isComposing: true }).defaultPrevented).toBe(false);
    expect(post).not.toHaveBeenCalled();
  });

});
