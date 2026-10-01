// A TerminalBridge for tests (src/lib/terminal.ts): it records every call,
// answers `spawn` with a fresh session id, and lets a test push output and the
// exit frame through the same router the real Channel uses.

import { frameRouter, type TerminalBridge, type TerminalExit, type TerminalSink, type TerminalSpawn, type TerminalStarted } from "@/lib/terminal";

export type BridgeCall =
  | { cmd: "spawn"; req: TerminalSpawn }
  | { cmd: "write"; session: number; data: string }
  | { cmd: "resize"; session: number; cols: number; rows: number }
  | { cmd: "kill"; session: number };

export class FakeBridge implements TerminalBridge {
  calls: BridgeCall[] = [];
  /** Each spawned session's frame router, by session id. */
  private routes = new Map<number, (frame: unknown) => void>();
  private nextSession = 1;
  /** Set to make the next `spawn` reject with it. */
  spawnFailure: unknown = null;
  /** Set to hold the next `spawn` until the returned release is called. */
  private gate: Promise<void> | null = null;

  /** Hold the next `spawn` answer; call the result to let it through. */
  holdSpawn(): () => void {
    let release!: () => void;
    this.gate = new Promise((r) => (release = r));
    return release;
  }

  async spawn(req: TerminalSpawn, sink: TerminalSink): Promise<TerminalStarted> {
    this.calls.push({ cmd: "spawn", req });
    const gate = this.gate;
    this.gate = null;
    if (gate) await gate;
    if (this.spawnFailure !== null) {
      const e = this.spawnFailure;
      this.spawnFailure = null;
      throw e;
    }
    const session = this.nextSession++;
    this.routes.set(session, frameRouter(sink));
    return { session, pid: 4000 + session, editor: `nvim '${req.path}'`, source: "probe", fixture: false };
  }

  async write(session: number, data: string): Promise<void> {
    this.calls.push({ cmd: "write", session, data });
  }

  async resize(session: number, cols: number, rows: number): Promise<void> {
    this.calls.push({ cmd: "resize", session, cols, rows });
  }

  async kill(session: number): Promise<void> {
    this.calls.push({ cmd: "kill", session });
  }

  /** Push an output frame, as UTF-8 bytes when given a string. */
  output(session: number, data: string | Uint8Array): void {
    const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
    this.frame(session, bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
  }

  /** Push the exit frame, the session's last. */
  exit(session: number, exit: TerminalExit = { code: 0, signal: null }): void {
    this.frame(session, { exit });
  }

  /** Push any frame on a session's channel. */
  frame(session: number, frame: unknown): void {
    const route = this.routes.get(session);
    if (!route) throw new Error(`no session ${session} was spawned`);
    route(frame);
  }

  of<C extends BridgeCall["cmd"]>(cmd: C): Extract<BridgeCall, { cmd: C }>[] {
    return this.calls.filter((c): c is Extract<BridgeCall, { cmd: C }> => c.cmd === cmd);
  }
}
