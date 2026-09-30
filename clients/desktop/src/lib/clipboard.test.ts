import { afterEach, describe, expect, it, vi } from "vitest";
import { CLIPBOARD_UNAVAILABLE, copyText } from "@/lib/clipboard";
import type { Action } from "@/app/reducer";

function withClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value });
}

describe("copyText", () => {
  afterEach(() => withClipboard(undefined));

  it("writes the text and says what it copied", async () => {
    const writeText = vi.fn(async () => {});
    withClipboard({ writeText });
    const dispatch = vi.fn<(a: Action) => void>();
    await expect(copyText("mp://work/inbox/1001", "the selector", dispatch)).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("mp://work/inbox/1001");
    expect(dispatch.mock.calls).toEqual([[{ type: "notice", text: "Copied the selector" }]]);
  });

  it("calls the clipboard before it returns, while the key press still counts as a user action", () => {
    const writeText = vi.fn(async () => {});
    withClipboard({ writeText });
    void copyText("x", "x", vi.fn());
    expect(writeText).toHaveBeenCalledTimes(1);
  });

  it("says the clipboard refused it when the write is rejected", async () => {
    withClipboard({ writeText: vi.fn(async () => Promise.reject(new DOMException("denied", "NotAllowedError"))) });
    const dispatch = vi.fn<(a: Action) => void>();
    await expect(copyText("a@b.example", "the address", dispatch)).resolves.toBe(false);
    expect(dispatch.mock.calls).toEqual([[{ type: "notice", text: "The clipboard refused the address" }]]);
  });

  it("says the clipboard refused it when the write throws", async () => {
    withClipboard({
      writeText: () => {
        throw new Error("not allowed");
      },
    });
    const dispatch = vi.fn<(a: Action) => void>();
    await expect(copyText("a", "the link", dispatch)).resolves.toBe(false);
    expect(dispatch.mock.calls).toEqual([[{ type: "notice", text: "The clipboard refused the link" }]]);
  });

  it("says so when the webview has no clipboard", async () => {
    withClipboard(undefined);
    const dispatch = vi.fn<(a: Action) => void>();
    await expect(copyText("a", "the link", dispatch)).resolves.toBe(false);
    expect(dispatch.mock.calls).toEqual([[{ type: "notice", text: CLIPBOARD_UNAVAILABLE }]]);
  });
});
