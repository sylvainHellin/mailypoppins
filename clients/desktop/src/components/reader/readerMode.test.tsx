import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { parseReaderMode } from "@/app/readerMode";
import { GUI_ENTRIES, paletteEntries } from "@/keymap/catalog";
import * as cmd from "@/lib/commands";
import { renderApp, shellReady } from "@/test/render";
import { mock, resetMock } from "@/test/tauri-mock";

const reader = () => screen.getByRole("complementary", { name: "Reader" });
const frame = () => reader().querySelector("iframe");
const textPane = () => within(reader()).findByLabelText(/^Message text: /);
const calls = (name: string) => mock.calls.filter((c) => c.cmd === name);

/** Set the stored body of work/inbox 1001, the first row. */
function setBody(body: string | null) {
  const row = mock.rows.work.inbox.find((r) => r.id === 1001);
  if (!row) throw new Error("no row 1001");
  (row as { body?: string | null }).body = body;
}

/** Open the first message of the inbox and wait for its headers. */
async function openFirst(user: ReturnType<typeof renderApp>["user"]) {
  await user.keyboard("j");
  await within(reader()).findByRole("toolbar", { name: "Message actions" });
}

describe("the reader mode", () => {
  it("tt shows the stored text in place of the frame, stores the mode, and tt again brings the frame back", async () => {
    const { user } = renderApp();
    await shellReady();
    await openFirst(user);
    expect(frame()).not.toBeNull();
    await user.keyboard("tt");
    const pane = await textPane();
    expect(pane.textContent).toContain("the quarterly ledger");
    expect(frame()).toBeNull();
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("text"));
    expect(calls("message_text").map((c) => c.args)).toEqual([{ account: "work", row_id: 1001 }]);
    await user.keyboard("tt");
    await waitFor(() => expect(frame()).not.toBeNull());
    expect(within(reader()).queryByLabelText(/^Message text: /)).toBeNull();
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("html"));
    // A second visit to text mode reads the cached text.
    await user.keyboard("tt");
    await textPane();
    expect(calls("message_text")).toHaveLength(1);
  });

  it("tt works from the reader pane, and tb still opens the HTML in text mode", async () => {
    const { user } = renderApp();
    await shellReady();
    await openFirst(user);
    act(() => document.getElementById("mp-reader-scroll")?.focus());
    await user.keyboard("tt");
    await textPane();
    await user.keyboard("tb");
    await waitFor(() => expect(calls("html_open").map((c) => c.args)).toEqual([{ account: "work", row_id: 1001 }]));
    expect(await screen.findByText("Opened in browser")).toBeInTheDocument();
  });

  it("tt from the sidebar changes nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await openFirst(user);
    await user.keyboard("gm");
    await user.keyboard("tt");
    expect(frame()).not.toBeNull();
    expect(mock.settings.has("reader_mode")).toBe(false);
  });

  it("the toolbar's HTML | Text group switches the mode, the current one pressed", async () => {
    const { user } = renderApp();
    await shellReady();
    await openFirst(user);
    const group = within(reader()).getByRole("group", { name: "Reader mode" });
    expect(within(group).getByRole("button", { name: "HTML" })).toHaveAttribute("aria-pressed", "true");
    await user.click(within(group).getByRole("button", { name: "Text" }));
    await textPane();
    expect(within(group).getByRole("button", { name: "Text" })).toHaveAttribute("aria-pressed", "true");
    expect(within(group).getByRole("button", { name: "HTML" })).toHaveAttribute("aria-pressed", "false");
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("text"));
  });

  it("the stored mode is read at startup", async () => {
    const { user } = renderApp(1400, () => mock.settings.set("reader_mode", "text"));
    await shellReady();
    await waitFor(() => expect(calls("setting_get").map((c) => c.args)).toContainEqual({ key: "reader_mode" }));
    await openFirst(user);
    await textPane();
    expect(frame()).toBeNull();
  });

  it("an unset or unknown stored value is html", async () => {
    expect(parseReaderMode(null)).toBe("html");
    expect(parseReaderMode("markdown")).toBe("html");
    expect(parseReaderMode("text")).toBe("text");
    resetMock();
    expect(await cmd.settingSet("reader_mode", " text ")).toBe("text");
    await expect(cmd.settingSet("reader_mode", "markdown")).rejects.toMatchObject({ kind: "setup" });
    expect(await cmd.settingGet("reader_mode")).toBe("text");
  });

  it("a refused write keeps the mode for this window and says so", async () => {
    const { user } = renderApp();
    await shellReady();
    await openFirst(user);
    mock.failing.set("setting_set", { kind: "internal", message: "could not write desktop.json" });
    await user.keyboard("tt");
    await textPane();
    expect(await screen.findByText("The reader mode was not saved: could not write desktop.json")).toBeInTheDocument();
  });
});

describe("the text pane", () => {
  it("keeps the line breaks and mutes the quoted lines", async () => {
    const body = "Hi,\n\nsee below.\n> the first quoted line\n  >> a deeper one\nThanks\n";
    const { user } = renderApp(1400, () => {
      setBody(body);
      mock.settings.set("reader_mode", "text");
    });
    await shellReady();
    await openFirst(user);
    const pane = await textPane();
    expect(pane.tagName).toBe("PRE");
    expect(pane.textContent).toBe(body.slice(0, -1));
    const quoted = [...pane.querySelectorAll("[data-quoted]")];
    expect(quoted.map((q) => q.textContent)).toEqual(["> the first quoted line", "  >> a deeper one"]);
    for (const q of quoted) expect(q).toHaveClass("text-muted-foreground");
    expect(within(pane).getByText(/see below/)).not.toHaveClass("text-muted-foreground");
    expect(pane).toHaveClass("font-mono", "whitespace-pre-wrap", "text-foreground");
  });

  it("says No text body for a message without one and stays in text mode", async () => {
    const { user } = renderApp(1400, () => {
      setBody(null);
      mock.settings.set("reader_mode", "text");
    });
    await shellReady();
    await openFirst(user);
    const status = await within(reader()).findByRole("status");
    expect(status).toHaveTextContent("No text body");
    expect(frame()).toBeNull();
    expect(mock.settings.get("reader_mode")).toBe("text");
    expect(within(reader()).getByRole("button", { name: "Text" })).toHaveAttribute("aria-pressed", "true");
  });

  it("a text that does not load says why", async () => {
    const { user } = renderApp(1400, () => {
      mock.settings.set("reader_mode", "text");
      mock.failing.set("message_text", { kind: "timeout", message: "the daemon did not answer" });
    });
    await shellReady();
    await openFirst(user);
    expect(await within(reader()).findByRole("alert")).toHaveTextContent("The text did not load: the daemon did not answer");
  });

  it("follows the cursor to the next message", async () => {
    const { user } = renderApp(1400, () => mock.settings.set("reader_mode", "text"));
    await shellReady();
    await openFirst(user);
    expect((await textPane()).textContent).toContain("the quarterly ledger");
    await user.keyboard("j");
    await waitFor(async () => expect((await textPane()).textContent).toContain("Dachsanierung"));
  });

  it("z zooms the reader in text mode, and j scrolls the reader's container", async () => {
    const { user } = renderApp(1400, () => mock.settings.set("reader_mode", "text"));
    await shellReady();
    await openFirst(user);
    await textPane();
    const scroll = document.getElementById("mp-reader-scroll") as HTMLElement;
    act(() => scroll.focus());
    await user.keyboard("j");
    expect(scroll.scrollTop).toBeGreaterThan(0);
    await user.keyboard("z");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(within(reader()).getByLabelText(/^Message text: /)).toBeInTheDocument();
    await user.keyboard("z");
    expect(screen.getByRole("listbox")).toBeInTheDocument();
  });
});

describe("Settings and the palette", () => {
  it("Settings' Reader buttons switch and store the mode", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(screen.getByRole("button", { name: /^Settings$/ }));
    const view = await screen.findByRole("region", { name: "Settings" });
    const group = await within(view).findByRole("group", { name: "Reader" });
    expect(within(group).getByRole("button", { name: "HTML" })).toHaveAttribute("aria-pressed", "true");
    await user.click(within(group).getByRole("button", { name: "Text" }));
    expect(within(group).getByRole("button", { name: "Text" })).toHaveAttribute("aria-pressed", "true");
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("text"));
    await user.click(within(group).getByRole("button", { name: "HTML" }));
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("html"));
  });

  it("the palette has Toggle reader text mode on tt, and Reader: HTML and Reader: text with no key", async () => {
    const rows = GUI_ENTRIES.filter((e) => e.id === "toggle_reader_mode" || e.label.startsWith("Reader: "));
    expect(rows.map((e) => [e.section, e.label, e.id, e.keys])).toEqual([
      ["READER", "Toggle reader text mode", "toggle_reader_mode", ["tt"]],
      ["READER", "Reader: HTML", "reader_html", []],
      ["READER", "Reader: text", "reader_text", []],
    ]);
    // The TUI's `tt` row, the thread view, gives the key up.
    const thread = paletteEntries().find((e) => e.label === "Show conversation (thread)");
    expect(thread?.keys).toEqual([]);
    expect(thread?.badge).toBe("later");

    const { user } = renderApp();
    await shellReady();
    await openFirst(user);
    await user.keyboard(":");
    let dialog = await screen.findByRole("dialog", { name: "Command palette" });
    let row = within(dialog).getByText("Reader: text").closest("[data-testid='palette-item']");
    expect(row).not.toHaveAttribute("data-disabled", "true");
    await act(async () => {
      await user.click(row as HTMLElement);
    });
    await textPane();
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("text"));

    await user.keyboard(":");
    dialog = await screen.findByRole("dialog", { name: "Command palette" });
    row = within(dialog).getByText("Toggle reader text mode").closest("[data-testid='palette-item']");
    await act(async () => {
      await user.click(row as HTMLElement);
    });
    await waitFor(() => expect(frame()).not.toBeNull());
    await waitFor(() => expect(mock.settings.get("reader_mode")).toBe("html"));

    await user.keyboard(":");
    dialog = await screen.findByRole("dialog", { name: "Command palette" });
    row = within(dialog).getByText("Reader: HTML").closest("[data-testid='palette-item']");
    await act(async () => {
      await user.click(row as HTMLElement);
    });
    expect(frame()).not.toBeNull();
  });
});
