// The embedded editor (ticket 0130, U2): one xterm.js terminal on one PTY session
// of the Rust layer, reached through a TerminalBridge (src/lib/terminal.ts).
// docs/shell.md, "Compose", "The terminal pane", is the description.
//
// The pane owns the terminal, not the session: unmounting it disposes xterm
// and its addons and kills nothing, and `bridge.kill` is the owner's call.
// Mount it under a `key` per session; its session prop is read once.

import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { ClipboardAddon } from "@xterm/addon-clipboard";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { tauriBridge, type TerminalBridge, type TerminalExit, type TerminalStarted } from "@/lib/terminal";
import { terminalFont, terminalTheme } from "@/components/compose/terminalTheme";
import { currentScheme } from "@/app/theme";

/** How long a run of container resizes settles before the PTY hears of it. */
export const RESIZE_DEBOUNCE_MS = 50;

export type TerminalPaneProps = {
  /** The draft the editor opens; read at mount. */
  session: { account: string; id: string; path: string };
  /** Read at mount; the real one over Tauri by default. */
  bridge?: TerminalBridge;
  /** Hidden keeps the terminal and its buffer; shown again, it refits and takes the focus. */
  visible: boolean;
  onStarted?: (started: TerminalStarted) => void;
  /** The child's exit, delivered once and always after `onStarted`. */
  onExit?: (exit: TerminalExit) => void;
  /** A refused spawn, write or resize, as the bridge rejected it. */
  onError?: (error: unknown) => void;
};

/**
 * Keys reach xterm, and so the editor, except Cmd combinations, which go to
 * the browser and the app: the menu items, copy and paste of the webview.
 * xterm consults this for keydown, keypress and keyup alike.
 */
export const terminalKeyFilter = (e: KeyboardEvent): boolean => !e.metaKey;

/** WebGL when the webview grants it, xterm's DOM renderer otherwise or once the context is lost. */
function loadWebgl(term: Terminal): void {
  let webgl: WebglAddon | null = null;
  try {
    webgl = new WebglAddon();
    const addon = webgl;
    addon.onContextLoss(() => addon.dispose());
    term.loadAddon(addon);
  } catch {
    try {
      webgl?.dispose();
    } catch {
      // A half-activated addon may not dispose cleanly; the DOM renderer stays either way.
    }
  }
}

/** The fit addon measures nothing in a hidden or unlaid container and throws in some. */
function safeFit(fit: FitAddon): void {
  try {
    fit.fit();
  } catch {
    // Keep the current size.
  }
}

type Live = { sync: () => void; focus: () => void };

export function TerminalPane({ session, bridge = tauriBridge, visible, onStarted, onExit, onError }: TerminalPaneProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const live = useRef<Live | null>(null);
  const latest = useRef({ session, bridge, visible, onStarted, onExit, onError });
  latest.current = { session, bridge, visible, onStarted, onExit, onError };
  const wasVisible = useRef(visible);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const { session: draft, bridge: pty } = latest.current;
    const report = (e: unknown) => latest.current.onError?.(e);

    const term = new Terminal({ allowProposedApi: true, theme: terminalTheme(), ...terminalFont(host) });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.loadAddon(new ClipboardAddon());
    term.attachCustomKeyEventHandler(terminalKeyFilter);
    term.open(host);
    loadWebgl(term);
    safeFit(fit);

    let disposed = false;
    let started: number | null = null;
    let exited = false;
    let heldExit: TerminalExit | null = null;
    // Keys typed while the spawn is in flight, sent once it answers.
    let typed = "";
    let sent = { cols: term.cols, rows: term.rows };

    const sync = () => {
      if (disposed) return;
      safeFit(fit);
      if (started === null || exited) return;
      if (term.cols === sent.cols && term.rows === sent.rows) return;
      sent = { cols: term.cols, rows: term.rows };
      pty.resize(started, sent.cols, sent.rows).catch(report);
    };

    let settle: ReturnType<typeof setTimeout> | undefined;
    const resized = new ResizeObserver(() => {
      clearTimeout(settle);
      settle = setTimeout(sync, RESIZE_DEBOUNCE_MS);
    });
    resized.observe(host);

    const input = term.onData((data) => {
      if (exited) return;
      if (started === null) typed += data;
      else pty.write(started, data).catch(report);
    });

    // src/app/theme.ts repaints by toggling `dark` and `light` on <html>.
    const repaint = new MutationObserver(() => {
      term.options.theme = terminalTheme();
    });
    repaint.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });

    const start = () => {
      sent = { cols: term.cols, rows: term.rows };
      const sink = {
        // Output may overtake the spawn's answer; it is the terminal's either way.
        onOutput: (bytes: Uint8Array) => {
          if (!disposed) term.write(bytes);
        },
        onExit: (exit: TerminalExit) => {
          exited = true;
          typed = "";
          if (started === null) heldExit = exit;
          else latest.current.onExit?.(exit);
        },
      };
      pty.spawn({ ...draft, cols: sent.cols, rows: sent.rows, theme: currentScheme() }, sink).then(
        (answer) => {
          started = answer.session;
          latest.current.onStarted?.(answer);
          if (heldExit) latest.current.onExit?.(heldExit);
          if (disposed || exited) return;
          if (typed) pty.write(answer.session, typed).catch(report);
          typed = "";
          sync();
          if (latest.current.visible) term.focus();
        },
        (e) => report(e),
      );
    };
    // A task later, so React's StrictMode remount (mount, unmount, mount)
    // cancels the first pane's spawn instead of starting two editors.
    const pending = setTimeout(start, 0);

    live.current = { sync, focus: () => term.focus() };
    return () => {
      disposed = true;
      clearTimeout(pending);
      clearTimeout(settle);
      resized.disconnect();
      repaint.disconnect();
      input.dispose();
      // Disposes every addon loaded into it; the session lives on.
      term.dispose();
      live.current = null;
    };
  }, []);

  useEffect(() => {
    if (visible && !wasVisible.current) {
      live.current?.sync();
      live.current?.focus();
    }
    wasVisible.current = visible;
  }, [visible]);

  return (
    <div
      data-slot="terminal"
      role="application"
      aria-label="Editor terminal"
      hidden={!visible}
      className="flex h-full min-h-0 w-full flex-col bg-background p-2"
    >
      <div ref={hostRef} className="min-h-0 flex-1 overflow-hidden font-mono text-sm" />
    </div>
  );
}
