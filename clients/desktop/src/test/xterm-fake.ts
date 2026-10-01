// Stand-ins for @xterm/xterm and the four addons the terminal pane loads,
// for tests that render the whole app with an embedded editor: jsdom has no
// canvas and lays nothing out. One module serves all five packages:
//
//   vi.mock("@xterm/xterm", () => import("@/test/xterm-fake"));
//
// and the same line for each addon. `terms` lists every terminal made.

type Addon = { activate(t: unknown): void; dispose(): void };

export const terms: Terminal[] = [];

export class Terminal {
  options: Record<string, unknown>;
  cols = 80;
  rows = 24;
  unicode = { activeVersion: "6" };
  written: Uint8Array[] = [];
  focusCount = 0;
  disposed = false;
  private data: ((d: string) => void)[] = [];
  private addons: Addon[] = [];
  constructor(options: Record<string, unknown>) {
    this.options = { ...options };
    terms.push(this);
  }
  open(_el: HTMLElement) {}
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
    this.written.push(d);
  }
  focus() {
    this.focusCount++;
  }
  attachCustomKeyEventHandler(_h: (e: KeyboardEvent) => boolean) {}
  dispose() {
    this.disposed = true;
    for (const a of this.addons) a.dispose();
  }
}

class FakeAddon {
  activate(_t: unknown) {}
  dispose() {}
}

export class FitAddon extends FakeAddon {
  fit() {}
}

export class WebglAddon extends FakeAddon {
  onContextLoss(_h: () => void) {
    return { dispose() {} };
  }
}

export class Unicode11Addon extends FakeAddon {}
export class ClipboardAddon extends FakeAddon {}
