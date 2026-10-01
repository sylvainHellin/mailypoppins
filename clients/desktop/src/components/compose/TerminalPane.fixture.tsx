// The terminal pane without a daemon or a PTY, for the eye: `pnpm dev` (or
// `pnpm tauri dev`) and open the page at `#terminal-fixture`; main.tsx mounts
// this in place of the app in a dev build only. The bridge echoes what is
// typed, prints the 16 ANSI colours, and reports each resize.

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { TerminalPane } from "@/components/compose/TerminalPane";
import { applyTheme } from "@/app/theme";
import type { TerminalBridge, TerminalSink, TerminalSpawn, TerminalStarted } from "@/lib/terminal";

const ESC = "\u001b";
const NAMES = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

/** The 16 ANSI colours as text and as backgrounds, one row per intensity. */
function swatches(): string {
  const row = (base: number, bright: boolean) =>
    NAMES.map((n, i) => `${ESC}[${base + i}m${(bright ? "bright " : "") + n}${ESC}[0m`).join(" ");
  const blocks = (base: number) => NAMES.map((_, i) => `${ESC}[${base + i}m  ${ESC}[0m`).join("");
  return [row(30, false), row(90, true), blocks(40) + " " + blocks(100)].join("\r\n");
}

export class EchoBridge implements TerminalBridge {
  private sinks = new Map<number, TerminalSink>();
  private next = 1;

  async spawn(req: TerminalSpawn, sink: TerminalSink): Promise<TerminalStarted> {
    const session = this.next++;
    this.sinks.set(session, sink);
    setTimeout(() => {
      this.say(session, `Fixture terminal for ${req.path}, ${req.cols}x${req.rows}; it echoes what you type.\r\n`);
      this.say(session, `${swatches()}\r\n\r\n${ESC}[1mbold${ESC}[0m ${ESC}[4munderline${ESC}[0m ${ESC}[7minverse${ESC}[0m 日本語 Grüße 🙂\r\n\r\n$ `);
    }, 0);
    return { session, pid: null, editor: "echo (fixture)", source: "fallback", fixture: true };
  }

  async write(session: number, data: string): Promise<void> {
    this.say(session, data.replace(/\r/g, "\r\n$ ").replace(/\u007f/g, "\b \b"));
  }

  async resize(session: number, cols: number, rows: number): Promise<void> {
    this.say(session, `\r\n${ESC}[2m(resized to ${cols}x${rows})${ESC}[0m\r\n$ `);
  }

  async kill(session: number): Promise<void> {
    this.sinks.get(session)?.onExit({ code: null, signal: 15 });
    this.sinks.delete(session);
  }

  private say(session: number, text: string): void {
    this.sinks.get(session)?.onOutput(new TextEncoder().encode(text));
  }
}

export function TerminalFixture() {
  const [bridge] = useState(() => new EchoBridge());
  const [visible, setVisible] = useState(true);
  const [status, setStatus] = useState("Starting");
  const [session, setSession] = useState<number | null>(null);
  const toggleTheme = () => applyTheme(document.documentElement.classList.contains("light") ? "dark" : "light");
  return (
    <div className="flex h-screen flex-col gap-2 bg-sidebar p-3 text-foreground">
      <div className="flex items-center gap-2 text-sm">
        <Button size="sm" variant="secondary" onClick={toggleTheme}>
          Toggle theme
        </Button>
        <Button size="sm" variant="secondary" onClick={() => setVisible((v) => !v)}>
          {visible ? "Hide" : "Show"}
        </Button>
        <Button size="sm" variant="secondary" disabled={session === null} onClick={() => session !== null && void bridge.kill(session)}>
          Kill
        </Button>
        <span role="status" className="text-muted-foreground">
          {status}
        </span>
      </div>
      <div className="min-h-0 flex-1 overflow-hidden rounded-md border">
        <TerminalPane
          session={{ account: "fixture", id: "terminal", path: "/fixture/terminal.md" }}
          bridge={bridge}
          visible={visible}
          onStarted={(s) => {
            setSession(s.session);
            setStatus(`Session ${s.session}: ${s.editor}`);
          }}
          onExit={(e) => setStatus(`Exited: code ${e.code ?? "none"}, signal ${e.signal ?? "none"}`)}
          onError={(e) => setStatus(`Error: ${String(e)}`)}
        />
      </div>
    </div>
  );
}
