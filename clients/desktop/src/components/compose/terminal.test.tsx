// The terminal pane's wiring (#0130, U2). jsdom has no canvas and lays
// nothing out, so xterm and its addons are stand-ins that record what the
// pane does with them; the bridge is the FakeBridge.

import { StrictMode } from "react";
import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RESIZE_DEBOUNCE_MS, TerminalPane, terminalKeyFilter, type TerminalPaneProps } from "@/components/compose/TerminalPane";
import { inTerminal, isEditable } from "@/keymap/useKeymap";
import { FakeBridge } from "@/test/terminal-fake";
import { renderApp, shellReady } from "@/test/render";

const xt = vi.hoisted(() => {
  type Addon = { activate(t: unknown): void; dispose(): void };
  const state = {
    size: { cols: 100, rows: 30 },
    webglThrows: false,
    terms: [] as FakeTerminal[],
    webgls: [] as FakeWebgl[],
  };
  class FakeTerminal {
    options: Record<string, unknown>;
    cols = 80;
    rows = 24;
    unicode = { activeVersion: "6" };
    element: HTMLElement | null = null;
    written: Uint8Array[] = [];
    addons: Addon[] = [];
    keyFilter: ((e: KeyboardEvent) => boolean) | null = null;
    focusCount = 0;
    disposed = false;
    private data: ((d: string) => void)[] = [];
    constructor(options: Record<string, unknown>) {
      this.options = { ...options };
      state.terms.push(this);
    }
    open(el: HTMLElement) {
      this.element = el;
    }
    loadAddon(a: Addon) {
      a.activate(this);
      this.addons.push(a);
    }
    onData(h: (d: string) => void) {
      this.data.push(h);
      return { dispose: () => (this.data = this.data.filter((x) => x !== h)) };
    }
    /** What a keystroke in the terminal emits. */
    type(d: string) {
      for (const h of this.data) h(d);
    }
    write(d: Uint8Array) {
      if (this.disposed) throw new Error("write after dispose");
      this.written.push(d);
    }
    focus() {
      this.focusCount++;
    }
    attachCustomKeyEventHandler(h: (e: KeyboardEvent) => boolean) {
      this.keyFilter = h;
    }
    dispose() {
      this.disposed = true;
      for (const a of this.addons) a.dispose();
    }
  }
  class FakeAddon {
    disposed = false;
    activate(_t: unknown) {}
    dispose() {
      this.disposed = true;
    }
  }
  class FakeFit extends FakeAddon {
    term: FakeTerminal | null = null;
    activate(t: unknown) {
      this.term = t as FakeTerminal;
    }
    fit() {
      if (!this.term) return;
      this.term.cols = state.size.cols;
      this.term.rows = state.size.rows;
    }
  }
  class FakeWebgl extends FakeAddon {
    private lost: (() => void)[] = [];
    constructor() {
      super();
      if (state.webglThrows) throw new Error("WebGL2 is not supported");
      state.webgls.push(this);
    }
    onContextLoss(h: () => void) {
      this.lost.push(h);
      return { dispose() {} };
    }
    loseContext() {
      for (const h of this.lost) h();
    }
  }
  return { state, FakeTerminal, FakeAddon, FakeFit, FakeWebgl };
});

vi.mock("@xterm/xterm", () => ({ Terminal: xt.FakeTerminal }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: xt.FakeFit }));
vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: xt.FakeWebgl }));
vi.mock("@xterm/addon-unicode11", () => ({ Unicode11Addon: class extends xt.FakeAddon {} }));
vi.mock("@xterm/addon-clipboard", () => ({ ClipboardAddon: class extends xt.FakeAddon {} }));

/** A ResizeObserver whose callback the test fires. */
const observers: { fire: () => void; disconnected: boolean }[] = [];
const NativeResizeObserver = globalThis.ResizeObserver;

beforeEach(() => {
  xt.state.size = { cols: 100, rows: 30 };
  xt.state.webglThrows = false;
  xt.state.terms = [];
  xt.state.webgls = [];
  observers.length = 0;
  globalThis.ResizeObserver = class {
    disconnected = false;
    constructor(private cb: ResizeObserverCallback) {
      observers.push(this);
    }
    fire() {
      this.cb([], this as unknown as ResizeObserver);
    }
    observe() {}
    unobserve() {}
    disconnect() {
      this.disconnected = true;
    }
  } as unknown as typeof ResizeObserver;
});

afterEach(() => {
  globalThis.ResizeObserver = NativeResizeObserver;
  document.documentElement.className = "";
  document.documentElement.removeAttribute("style");
});

const DRAFT = { account: "work", id: "draft-1", path: "/fixture/work/drafts/draft-1.md" };

function mount(props: Partial<TerminalPaneProps> = {}) {
  const bridge = new FakeBridge();
  const onStarted = vi.fn();
  const onExit = vi.fn();
  const onError = vi.fn();
  const all = { session: DRAFT, bridge, visible: true, onStarted, onExit, onError, ...props };
  const utils = render(<TerminalPane {...all} />);
  const rerender = (more: Partial<TerminalPaneProps>) => utils.rerender(<TerminalPane {...all} {...more} />);
  return { ...utils, rerender, bridge, onStarted, onExit, onError, term: () => xt.state.terms[0] };
}

async function started(m: ReturnType<typeof mount>) {
  await waitFor(() => expect(m.onStarted).toHaveBeenCalledTimes(1));
  return m.term();
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

describe("the terminal pane", () => {
  it("fits, then spawns the editor at the fitted size and focuses the terminal", async () => {
    const m = mount();
    const term = await started(m);
    expect(m.bridge.of("spawn")).toEqual([{ cmd: "spawn", req: { ...DRAFT, cols: 100, rows: 30, theme: "dark" } }]);
    expect(m.onStarted).toHaveBeenCalledWith(expect.objectContaining({ session: 1, editor: `nvim '${DRAFT.path}'` }));
    expect(term.options.allowProposedApi).toBe(true);
    expect(term.unicode.activeVersion).toBe("11");
    expect(term.addons.map((a) => a.constructor.name)).toContain("FakeWebgl");
    expect(term.addons).toHaveLength(4);
    expect(term.focusCount).toBe(1);
    const pane = screen.getByRole("application", { name: "Editor terminal" });
    expect(pane).toHaveAttribute("data-slot", "terminal");
    expect(term.element && pane.contains(term.element)).toBe(true);
  });

  it("spawns once under StrictMode's mount, unmount and mount", async () => {
    const bridge = new FakeBridge();
    const onStarted = vi.fn();
    render(
      <StrictMode>
        <TerminalPane session={DRAFT} bridge={bridge} visible onStarted={onStarted} />
      </StrictMode>,
    );
    await waitFor(() => expect(onStarted).toHaveBeenCalledTimes(1));
    await sleep(20);
    expect(bridge.of("spawn")).toHaveLength(1);
    expect(xt.state.terms.filter((t) => !t.disposed)).toHaveLength(1);
  });

  it("sends what the terminal types to the session, and what was typed before the spawn answered once it does", async () => {
    const bridge = new FakeBridge();
    const release = bridge.holdSpawn();
    const m = mount({ bridge });
    await waitFor(() => expect(bridge.of("spawn")).toHaveLength(1));
    m.term().type("i");
    expect(bridge.of("write")).toEqual([]);
    release();
    const term = await started(m);
    await waitFor(() => expect(bridge.of("write")).toEqual([{ cmd: "write", session: 1, data: "i" }]));
    term.type("Grüße");
    term.type("\u001b");
    await waitFor(() => expect(bridge.of("write").map((c) => c.data)).toEqual(["i", "Grüße", "\u001b"]));
  });

  it("writes output frames to the terminal as bytes, a character split across frames included", async () => {
    const m = mount();
    const term = await started(m);
    const umlaut = new TextEncoder().encode("ü");
    act(() => {
      m.bridge.output(1, "hello ");
      m.bridge.output(1, umlaut.slice(0, 1));
      m.bridge.output(1, umlaut.slice(1));
    });
    expect(term.written.every((b) => b instanceof Uint8Array)).toBe(true);
    expect(new TextDecoder().decode(new Uint8Array(term.written.flatMap((b) => [...b])))).toBe("hello ü");
    expect(text(term.written[0])).toBe("hello ");
  });

  it("writes output that overtakes the spawn's answer", async () => {
    const bridge = new FakeBridge();
    const release = bridge.holdSpawn();
    const m = mount({ bridge });
    await waitFor(() => expect(bridge.of("spawn")).toHaveLength(1));
    release();
    const term = await started(m);
    act(() => bridge.output(1, "~"));
    expect(term.written.map(text)).toEqual(["~"]);
  });

  it("resizes the session once per settled size, and not when the size is unchanged", async () => {
    const m = mount();
    await started(m);
    xt.state.size = { cols: 120, rows: 40 };
    observers[0].fire();
    observers[0].fire();
    observers[0].fire();
    await sleep(RESIZE_DEBOUNCE_MS + 40);
    expect(m.bridge.of("resize")).toEqual([{ cmd: "resize", session: 1, cols: 120, rows: 40 }]);
    observers[0].fire();
    await sleep(RESIZE_DEBOUNCE_MS + 40);
    expect(m.bridge.of("resize")).toHaveLength(1);
  });

  it("hands the exit to onExit once, and writes nothing to the session afterwards", async () => {
    const m = mount();
    const term = await started(m);
    act(() => m.bridge.exit(1, { code: 1, signal: null }));
    expect(m.onExit).toHaveBeenCalledWith({ code: 1, signal: null });
    term.type("x");
    act(() => m.bridge.output(1, "late"));
    act(() => m.bridge.exit(1, { code: 0, signal: null }));
    xt.state.size = { cols: 90, rows: 20 };
    observers[0].fire();
    await sleep(RESIZE_DEBOUNCE_MS + 40);
    expect(m.onExit).toHaveBeenCalledTimes(1);
    expect(m.bridge.of("write")).toEqual([]);
    expect(m.bridge.of("resize")).toEqual([]);
    expect(term.written).toEqual([]);
  });

  it("delivers an exit that overtakes the spawn's answer after onStarted", async () => {
    const bridge = new FakeBridge();
    const release = bridge.holdSpawn();
    const order: string[] = [];
    // The fake routes frames only once spawn has answered, so the overtaking
    // exit is pushed through the sink the pane handed to spawn.
    const spawn = bridge.spawn.bind(bridge);
    let sink: Parameters<FakeBridge["spawn"]>[1] | null = null;
    bridge.spawn = (req, s) => {
      sink = s;
      return spawn(req, s);
    };
    mount({ bridge, onStarted: () => order.push("started"), onExit: () => order.push("exit") });
    await waitFor(() => expect(sink).not.toBeNull());
    sink!.onExit({ code: null, signal: 9 });
    expect(order).toEqual([]);
    release();
    await waitFor(() => expect(order).toEqual(["started", "exit"]));
  });

  it("hides without disposing, and refits and refocuses when shown again", async () => {
    const m = mount();
    const term = await started(m);
    const pane = screen.getByRole("application", { name: "Editor terminal", hidden: true });
    m.rerender({ visible: false });
    expect(pane).not.toBeVisible();
    expect(term.disposed).toBe(false);
    act(() => m.bridge.output(1, "in the background"));
    expect(term.written.map(text)).toEqual(["in the background"]);
    xt.state.size = { cols: 132, rows: 50 };
    m.rerender({ visible: true });
    expect(pane).toBeVisible();
    expect(xt.state.terms).toHaveLength(1);
    expect(term.focusCount).toBe(2);
    expect(m.bridge.of("resize")).toEqual([{ cmd: "resize", session: 1, cols: 132, rows: 50 }]);
  });

  it("disposes xterm and its addons on unmount and kills nothing", async () => {
    const m = mount();
    const term = await started(m);
    m.unmount();
    expect(term.disposed).toBe(true);
    expect(term.addons.every((a) => (a as unknown as { disposed: boolean }).disposed)).toBe(true);
    expect(observers[0].disconnected).toBe(true);
    expect(m.bridge.of("kill")).toEqual([]);
    // A frame after the unmount reaches no disposed terminal.
    m.bridge.output(1, "after");
    expect(term.written).toEqual([]);
  });

  it("falls back to the DOM renderer when WebGL refuses, and when its context is lost", async () => {
    xt.state.webglThrows = true;
    const m = mount();
    const term = await started(m);
    expect(term.addons).toHaveLength(3);
    m.unmount();

    xt.state.webglThrows = false;
    xt.state.terms = [];
    const n = mount();
    await started(n);
    const webgl = xt.state.webgls[0];
    webgl.loseContext();
    expect(webgl.disposed).toBe(true);
  });

  it("reports a refused spawn through onError", async () => {
    const bridge = new FakeBridge();
    const refusal = { kind: "setup", message: "code -w is a GUI editor" };
    bridge.spawnFailure = refusal;
    const m = mount({ bridge });
    await waitFor(() => expect(m.onError).toHaveBeenCalledWith(refusal));
    expect(m.onStarted).not.toHaveBeenCalled();
  });

  it("retheme the terminal when the palette on <html> changes", async () => {
    const root = document.documentElement;
    root.style.setProperty("--background", "#0F213D");
    root.style.setProperty("--terminal-red", "#FF7A85");
    const m = mount();
    const term = await started(m);
    expect(term.options.theme).toMatchObject({ background: "#0F213D", red: "#FF7A85" });
    root.style.setProperty("--background", "#FBFAF6");
    root.classList.add("light");
    await waitFor(() => expect(term.options.theme).toMatchObject({ background: "#FBFAF6" }));
  });

  it("spawns in the palette painted at spawn, and a later change reaches only the next spawn", async () => {
    document.documentElement.classList.add("light");
    const first = mount();
    await started(first);
    expect(first.bridge.of("spawn").map((c) => c.req.theme)).toEqual(["light"]);
    document.documentElement.classList.replace("light", "dark");
    await sleep(20);
    expect(first.bridge.of("spawn")).toHaveLength(1);
    first.unmount();
    const second = mount();
    await waitFor(() => expect(second.bridge.of("spawn").map((c) => c.req.theme)).toEqual(["dark"]));
  });
});

describe("keys in the terminal", () => {
  const key = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);

  it("go to the editor, Escape, Tab and Ctrl keys included, and Cmd combinations to the browser", async () => {
    const m = mount();
    const term = await started(m);
    expect(term.keyFilter).toBe(terminalKeyFilter);
    for (const init of [{ key: "Escape" }, { key: "Tab" }, { key: "b", ctrlKey: true }, { key: "j" }, { key: ":" }, { key: "x", altKey: true }]) {
      expect(terminalKeyFilter(key(init))).toBe(true);
    }
    for (const init of [{ key: "c", metaKey: true }, { key: "v", metaKey: true }, { key: "q", metaKey: true }, { key: ",", metaKey: true }]) {
      expect(terminalKeyFilter(key(init))).toBe(false);
    }
  });

  it("count as an editable target for the app keymap", async () => {
    const m = mount();
    await started(m);
    const pane = screen.getByRole("application", { name: "Editor terminal" });
    const textarea = document.createElement("textarea");
    const cell = document.createElement("div");
    pane.firstElementChild!.append(textarea, cell);
    expect(isEditable(cell)).toBe(true);
    expect(isEditable(textarea)).toBe(true);
    expect(inTerminal(cell)).toBe(true);
    expect(inTerminal(document.body)).toBe(false);
  });

  it("reach no app key, not the palette's : nor Escape's blur", async () => {
    const { user } = renderApp();
    await shellReady();
    const pane = document.createElement("div");
    pane.dataset.slot = "terminal";
    const textarea = document.createElement("textarea");
    pane.append(textarea);
    document.body.append(pane);
    textarea.focus();
    await user.keyboard(":");
    await user.keyboard("{Escape}");
    expect(textarea).toHaveFocus();
    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
    pane.remove();
  });
});
