// The output Channel's frames (ticket 0130): raw bytes, then the exit
// object, routed to a TerminalSink in Channel order. Kept apart from
// `src/lib/terminal.ts`, which imports Tauri, so the test mock of
// `@/lib/tauri` can route frames without importing itself.

import type { TerminalExit } from "@/protocol/generated/gui";

export type { TerminalExit };

/** Where a session's output and its exit go. */
export type TerminalSink = {
  onOutput: (bytes: Uint8Array) => void;
  onExit: (exit: TerminalExit) => void;
};

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
