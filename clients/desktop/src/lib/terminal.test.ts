import { describe, expect, it, vi } from "vitest";
import { Channel, invoke } from "@/lib/tauri";
import { frameRouter, tauriBridge, type TerminalExit, type TerminalStarted } from "@/lib/terminal";

function sink() {
  const out: number[][] = [];
  const exits: TerminalExit[] = [];
  return { out, exits, onOutput: (b: Uint8Array) => out.push([...b]), onExit: (e: TerminalExit) => exits.push(e) };
}

describe("the terminal frame router", () => {
  it("passes ArrayBuffer, Uint8Array and number-array frames on as bytes", () => {
    const s = sink();
    const route = frameRouter(s);
    route(new Uint8Array([104, 105]).buffer);
    const backing = new Uint8Array([0, 195, 188, 0]);
    route(new Uint8Array(backing.buffer, 1, 2));
    route([33]);
    expect(s.out).toEqual([[104, 105], [195, 188], [33]]);
  });

  it("ends with the exit frame, read defensively, and drops everything after it", () => {
    const s = sink();
    const route = frameRouter(s);
    route({ unknown: true });
    route(null);
    route({ exit: { code: "0", signal: 15 } });
    route(new Uint8Array([1]).buffer);
    route({ exit: { code: 0, signal: null } });
    expect(s.exits).toEqual([{ code: null, signal: 15 }]);
    expect(s.out).toEqual([]);
  });
});

describe("the Tauri bridge", () => {
  it("spawns with the request and an output Channel that feeds the sink", async () => {
    const answer: TerminalStarted = { session: 7, pid: 123, editor: "nvim", source: "editor", fixture: false };
    vi.mocked(invoke).mockImplementationOnce(async () => answer);
    const s = sink();
    const req = { account: "work", id: "d1", path: "/d/d1.md", cols: 80, rows: 24, theme: "light" as const };
    await expect(tauriBridge.spawn(req, s)).resolves.toEqual(answer);
    const [cmd, args] = vi.mocked(invoke).mock.lastCall!;
    expect(cmd).toBe("terminal_spawn");
    const { output, ...rest } = args as Record<string, unknown>;
    expect(rest).toEqual(req);
    expect(output).toBeInstanceOf(Channel);
    const channel = output as Channel<unknown>;
    channel.onmessage(new Uint8Array([111, 107]).buffer);
    channel.onmessage({ exit: { code: 0, signal: null } });
    expect(s.out).toEqual([[111, 107]]);
    expect(s.exits).toEqual([{ code: 0, signal: null }]);
  });

  it("names the session in write, resize and kill", async () => {
    for (let i = 0; i < 3; i++) vi.mocked(invoke).mockImplementationOnce(async () => undefined);
    await tauriBridge.write(7, "ihi\u001b");
    await tauriBridge.resize(7, 120, 40);
    await tauriBridge.kill(7);
    expect(vi.mocked(invoke).mock.calls.slice(-3)).toEqual([
      ["terminal_write", { session: 7, data: "ihi\u001b" }],
      ["terminal_resize", { session: 7, cols: 120, rows: 40 }],
      ["terminal_kill", { session: 7 }],
    ]);
  });
});
