// The embedded editor's PTY, as the webview sees it (ticket 0130, U2): the four
// Tauri commands of `src-tauri/src/terminal.rs` and the output Channel,
// behind `TerminalBridge`, so `TerminalPane.tsx` and its tests never touch
// Tauri. `tauriBridge` is the real one; `src/test/terminal-fake.ts` the fake.
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

/**
 * `theme` is the palette painted at spawn (`currentScheme`): Neovim and Vim
 * take it as their `background` and colorscheme, and every editor as
 * `MP_DESKTOP_THEME`; a later theme change reaches the next spawn.
 */
export type TerminalSpawn = { account: string; id: string; path: string; cols: number; rows: number; theme: Scheme };

export interface TerminalBridge {
  spawn(req: TerminalSpawn, sink: TerminalSink): Promise<TerminalStarted>;
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
