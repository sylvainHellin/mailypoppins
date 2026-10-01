// The embedded editor's PTY, as the webview sees it (#0130, U2): the four
// Tauri commands of `src-tauri/src/terminal.rs` and the output Channel,
// behind `TerminalBridge`, so `TerminalPane.tsx` and its tests never touch
// Tauri. `tauriBridge` is the real one; `src/test/terminal-fake.ts` the fake.
//
// The Channel carries raw output frames as `ArrayBuffer` (the Rust layer's
// `InvokeResponseBody::Raw`), and its last frame is the JSON object
// `{ "exit": { "code": number | null, "signal": number | null } }`, so the
// exit can never overtake the output before it.

import { Channel, invoke } from "@/lib/tauri";
import type { EditorSource } from "@/protocol/generated/gui";

/** How the child ended: an exit code, or the signal that killed it. */
export type TerminalExit = { code: number | null; signal: number | null };

/**
 * What `terminal_spawn` answers. Defined here until U1's ts-rs export lands
 * in `src/protocol/generated/gui/`; U3 switches this to that import.
 */
export type TerminalStarted = {
  /** The session id every later command names. */
  session: number;
  /** The child's pid; null in fixture mode, which spawns nothing. */
  pid: number | null;
  /** The editor command as resolved, for the banner. */
  editor: string;
  /** Where that command came from, as `EditorLaunch.source`. */
  source: EditorSource;
  /** True when fixture mode journaled the spawn instead of running it. */
  fixture: boolean;
};

/** Where a session's output and its exit go. */
export type TerminalSink = {
  onOutput: (bytes: Uint8Array) => void;
  onExit: (exit: TerminalExit) => void;
};

export type TerminalSpawn = { account: string; id: string; path: string; cols: number; rows: number };

export interface TerminalBridge {
  spawn(req: TerminalSpawn, sink: TerminalSink): Promise<TerminalStarted>;
  write(session: number, data: string): Promise<void>;
  resize(session: number, cols: number, rows: number): Promise<void>;
  kill(session: number): Promise<void>;
}

/** One frame as the Channel delivers it. */
export type TerminalFrame = ArrayBuffer | Uint8Array | number[] | { exit: Partial<TerminalExit> };

const intOrNull = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);

/** An ArrayBuffer from any realm: `instanceof` misses one made by another global (jsdom's tests, an iframe). */
const isArrayBuffer = (v: unknown): v is ArrayBuffer =>
  v instanceof ArrayBuffer || Object.prototype.toString.call(v) === "[object ArrayBuffer]";

/**
 * A sink that takes frames in Channel order: bytes go to `onOutput`, the
 * exit frame to `onExit` once, and nothing passes after the exit. Bytes are
 * handed on undecoded, since `Terminal.write` takes a `Uint8Array` and
 * decodes UTF-8 itself across frame boundaries. A frame of neither shape is
 * dropped. Exported for the bridge's tests.
 */
export function frameRouter(sink: TerminalSink): (frame: unknown) => void {
  let exited = false;
  return (frame) => {
    if (exited) return;
    if (isArrayBuffer(frame)) return sink.onOutput(new Uint8Array(frame));
    if (ArrayBuffer.isView(frame)) return sink.onOutput(new Uint8Array(frame.buffer, frame.byteOffset, frame.byteLength));
    // A `Vec<u8>` serialised as JSON, should the Rust side ever send one.
    if (Array.isArray(frame)) return sink.onOutput(Uint8Array.from(frame as number[]));
    if (frame !== null && typeof frame === "object" && "exit" in frame) {
      const exit = (frame as { exit: unknown }).exit as Partial<TerminalExit> | null;
      exited = true;
      sink.onExit({ code: intOrNull(exit?.code), signal: intOrNull(exit?.signal) });
    }
  };
}

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
