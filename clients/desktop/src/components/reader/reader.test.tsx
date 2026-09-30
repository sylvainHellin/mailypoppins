import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, mock } from "@/test/tauri-mock";
import type { InterceptedUrl } from "@/lib/gui-types";

const reader = () => screen.getByRole("complementary", { name: "Reader" });

async function openHostile(user: ReturnType<typeof renderApp>["user"]) {
  // Row 1006 is the sixth message of work/inbox.
  await user.keyboard("jjjjjj");
  return within(reader()).findByTitle("Message body: Action required: verify your account");
}

function intercept(url: string, source: InterceptedUrl["source"] = "navigation"): void {
  act(() => emit({ type: "link_intercepted", url: { url, at: 1_700_000_000_000, source } }));
}

describe("the reader frame", () => {
  it("loads the mpmsg URL in a script-free sandbox, with no referrer", async () => {
    const { user } = renderApp();
    await shellReady();
    const frame = await openHostile(user);
    expect(frame.tagName).toBe("IFRAME");
    expect(frame.getAttribute("sandbox")).toBe("allow-popups");
    expect(frame.getAttribute("src")).toBe("mpmsg://localhost/work/1006");
    expect(frame.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(frame).not.toHaveAttribute("srcdoc");
  });

  it("shows a skeleton until the frame loads", async () => {
    const { user } = renderApp();
    await shellReady();
    const frame = await openHostile(user);
    expect(reader().querySelector('[data-slot="reader-body-loading"]')).not.toBeNull();
    fireEvent.load(frame);
    expect(reader().querySelector('[data-slot="reader-body-loading"]')).toBeNull();
  });

  it("no longer fetches a plain-text body: the frame is the only path", async () => {
    const { user } = renderApp();
    await shellReady();
    await openHostile(user);
    expect(mock.calls.some((c) => c.cmd === "message_text")).toBe(false);
  });
});

describe("intercepted links", () => {
  it("shows a refused link in the reader footer and opens it only on the click, once", async () => {
    const { user } = renderApp();
    await shellReady();
    await openHostile(user);
    const url = "https://evil.example/?from=plain-link";
    expect(screen.queryByRole("region", { name: "Blocked link" })).toBeNull();
    intercept(url);

    const notice = await within(reader()).findByRole("region", { name: "Blocked link" });
    expect(within(notice).getByText(url)).toHaveAttribute("title", url);
    expect(mock.calls.filter((c) => c.cmd === "open_external")).toHaveLength(0);

    await user.click(within(notice).getByRole("button", { name: "Open in browser" }));
    const opens = mock.calls.filter((c) => c.cmd === "open_external");
    expect(opens).toHaveLength(1);
    expect(opens[0].args).toEqual({ url });
  });

  it("copies the link without opening anything", async () => {
    const { user } = renderApp();
    await shellReady();
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const url = "https://evil.example/?from=target-blank";
    intercept(url, "new_window");
    const notice = await screen.findByRole("region", { name: "Blocked link" });
    await user.click(within(notice).getByRole("button", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith(url);
    expect(mock.calls.some((c) => c.cmd === "open_external")).toBe(false);
  });

  it("cannot hand a non-web scheme to the opener, and the notice dismisses", async () => {
    const { user } = renderApp();
    await shellReady();
    intercept("file:///etc/hosts");
    const notice = await screen.findByRole("region", { name: "Blocked link" });
    expect(within(notice).getByRole("button", { name: "Open in browser" })).toBeDisabled();
    await user.click(within(notice).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByRole("region", { name: "Blocked link" })).toBeNull();
  });

  it("does not raise a notice for the opener stub's own log line", async () => {
    renderApp();
    await shellReady();
    intercept("https://example.com/", "open_external_stub");
    expect(screen.queryByRole("region", { name: "Blocked link" })).toBeNull();
  });

  it("lists the intercept log from the palette", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.interceptLog = [
      { url: "https://evil.example/?from=plain-link", at: 1, source: "navigation" },
      { url: "https://evil.example/?from=target-blank", at: 2, source: "new_window" },
    ];
    await user.keyboard(":");
    await user.keyboard("Show intercepted links");
    await user.keyboard("{Enter}");
    const dialog = await screen.findByRole("dialog", { name: "Intercepted links" });
    await waitFor(() => expect(within(dialog).getAllByRole("listitem")).toHaveLength(2));
    // Newest first.
    expect(within(dialog).getAllByRole("listitem")[0]).toHaveTextContent("from=target-blank");
    expect(mock.calls.some((c) => c.cmd === "intercepted_urls")).toBe(true);
    expect(mock.calls.some((c) => c.cmd === "open_external")).toBe(false);
  });
});
