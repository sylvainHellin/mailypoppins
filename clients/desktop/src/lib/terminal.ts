// The embedded editor's PTY, as the webview sees it (ticket 0130, U2): the four
// Tauri commands of `src-tauri/src/terminal.rs` and the output Channel,
// behind `TerminalBridge`, so `TerminalPane.tsx` and its tests never touch
// Tauri. `tauriBridge` is the real one; `src/fixture/terminal-bridge.ts` the fixture backend's.
//
// The Channel carries raw output frames as `ArrayBuffer` (the Rust layer's
// `InvokeResponseBody::Raw`), and its last frame is the JSON object
// `{ "exit": { "code": number | null, "signal": number | null } }`, so the
// exit can never overtake the output before it.

import { Channel, invoke } from "@/lib/tauri";
import { frameRouter, type TerminalExit, type TerminalFrame, type TerminalSink } from "@/lib/terminal-frames";
import type { TerminalStarted } from "@/protocol/generated/gui";
import type { Scheme } from "@/app/theme";

/** What `terminal_spawn` answers, generated from `src-tauri/src/terminal.rs`. */
export type { TerminalStarted };
export type { TerminalExit, TerminalSink };

/** What an editor session runs on: a draft, or a received message's rendition. */
export type DraftRun = { account: string; id: string; path: string };

/** What the agent's session runs on (PERSO-315): the batch prompt, and today as `YYYY-MM-DD`. */
export type AgentRun = { text: string; date: string };

/**
 * The PTY's size, and `theme`, the palette painted at spawn (`currentScheme`):
 * Neovim and Vim take it as their `background` and colorscheme, and every
 * program as `MP_DESKTOP_THEME`; a later theme change reaches the next spawn.
 */
export type PtyLook = { cols: number; rows: number; theme: Scheme };

export type TerminalSpawn = DraftRun & PtyLook;

/** `agent_spawn`'s arguments, the Channel aside. */
export type AgentSpawn = AgentRun & PtyLook;

/** A PTY session: started on an `R` (a draft by default), then written to, resized and killed by its id. */
export interface TerminalBridge<R = DraftRun> {
  spawn(req: R & PtyLook, sink: TerminalSink): Promise<TerminalStarted>;
  write(session: number, data: string): Promise<void>;
  resize(session: number, cols: number, rows: number): Promise<void>;
  kill(session: number): Promise<void>;
}

export { frameRouter, type TerminalFrame };

/** The real bridge, over `invoke` and one Channel per session. */
export const tauriBridge: TerminalBridge = {
  spawn(req, sink) {
    const output = new Channel<TerminalFrame>();
    output.onmessage = frameRouter(sink);
    return invoke<TerminalStarted>("terminal_spawn", { ...req, output });
  },
  write: (session, data) => invoke<void>("terminal_write", { session, data }),
  resize: (session, cols, rows) => invoke<void>("terminal_resize", { session, cols, rows }),
  kill: (session) => invoke<void>("terminal_kill", { session }),
};

/**
 * The agent's bridge (src-tauri/src/handoff.rs): `agent_spawn` writes the
 * batch prompt and starts the `[agent]` command on it; the session is then a
 * terminal session as an editor's.
 */
export const agentBridge: TerminalBridge<AgentRun> = {
  spawn(req, sink) {
    const output = new Channel<TerminalFrame>();
    output.onmessage = frameRouter(sink);
    return invoke<TerminalStarted>("agent_spawn", { ...req, output });
  },
  write: tauriBridge.write,
  resize: tauriBridge.resize,
  kill: tauriBridge.kill,
};
