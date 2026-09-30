import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, fixtures, mock, MOCK_CONFIG_PATH, MOCK_LOG_PATH, NO_CONFIG } from "@/test/tauri-mock";

const seededHold = fixtures.bootstrap.snapshot.holds[0];
const inboxRow = fixtures.messages.work.inbox[0] as { selector: string; subject: string };

function stubClipboard() {
  const writeText = vi.fn(async (_: string) => {});
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  return writeText;
}

async function openLog() {
  const dialog = await screen.findByRole("dialog", { name: "Activity log" });
  return { dialog, log: within(dialog).getByRole("log", { name: "Activity log lines" }) };
}

const entries = (log: HTMLElement) =>
  [...log.querySelectorAll<HTMLElement>("[data-log-entry]")].map((e) => [e.dataset.level, e.lastElementChild?.textContent]);

/** A scroll box jsdom lays out: 1000 px of lines in a 200 px view. */
function layOut(el: HTMLElement) {
  let top = 0;
  Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => 1000 });
  Object.defineProperty(el, "clientHeight", { configurable: true, get: () => 200 });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => void (top = Math.max(0, Math.min(800, v))),
  });
}

describe("the activity log dialog", () => {
  it("s l lists what the window reported, oldest first, with each level, and Escape closes it", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("sync_trigger", { kind: "internal", message: "no route" });
    await user.keyboard("ss");
    await screen.findByRole("alert");
    act(() => emitEnvelope("config.invalid", { path: "/c/config.toml", line: 3, message: "expected a table" }));
    await user.keyboard("sl");
    const { log } = await openLog();
    expect(entries(log)).toEqual([
      ["info", "Quick sync of work…"],
      ["error", "Sync of work did not start: no route"],
      ["error", "The configuration was refused (/c/config.toml, line 3): expected a table"],
    ]);
    // Keys in the dialog stay in it.
    await user.keyboard("jkdu");
    expect(mock.calls.filter((c) => c.cmd === "message_text")).toEqual([]);
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Activity log" })).toBeNull());
  });

  it("filters on text and level: / goes to the field, Enter back to the lines, Escape clears, then closes", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("sync_trigger", { kind: "internal", message: "no route" });
    await user.keyboard("ss");
    await screen.findByRole("alert");
    await user.keyboard("sl");
    const { dialog, log } = await openLog();
    await user.keyboard("/");
    const field = within(dialog).getByLabelText("Filter");
    expect(field).toHaveFocus();
    await user.keyboard("error");
    expect(entries(log)).toEqual([["error", "Sync of work did not start: no route"]]);
    await user.keyboard("{Enter}");
    expect(log).toHaveFocus();
    expect(field).toHaveValue("error");
    await user.keyboard("/");
    await user.clear(field);
    await user.keyboard("nothing like it");
    expect(within(log).getByText("No line matches the filter")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(field).toHaveValue("");
    expect(entries(log)).toHaveLength(2);
    expect(screen.getByRole("dialog", { name: "Activity log" })).toBeInTheDocument();
    await user.keyboard("/{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Activity log" })).toBeNull());
  });

  it("opens at the end and scrolls with j/k, d/u, gg and G", async () => {
    const { user } = renderApp();
    await shellReady();
    // Lines taller than the view from the first render on, which jsdom does not lay out.
    const height = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get(this: HTMLElement) {
        return this.getAttribute("role") === "log" ? 1000 : 0;
      },
    });
    try {
      await user.keyboard("sl");
      const { log } = await openLog();
      expect(log.scrollTop).toBe(1000);
    } finally {
      if (height) Object.defineProperty(HTMLElement.prototype, "scrollHeight", height);
      else delete (HTMLElement.prototype as { scrollHeight?: number }).scrollHeight;
    }
    const { log } = await openLog();
    layOut(log);
    await user.keyboard("G");
    expect(log.scrollTop).toBe(800);
    await user.keyboard("k");
    expect(log.scrollTop).toBe(772);
    await user.keyboard("u");
    expect(log.scrollTop).toBe(672);
    await user.keyboard("gg");
    expect(log.scrollTop).toBe(0);
    await user.keyboard("jd");
    expect(log.scrollTop).toBe(128);
    await user.keyboard("{ArrowDown}{ArrowUp}");
    expect(log.scrollTop).toBe(128);
  });

  it("says so when nothing was reported, and the sidebar's Activity entry opens it too", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(screen.getByRole("button", { name: "Activity log, key s l" }));
    const { log } = await openLog();
    expect(within(log).getByText("Nothing reported yet")).toBeInTheDocument();
  });
});

describe("! and the activity area", () => {
  it("hides the notices and never the live hold card, and shows them again", async () => {
    const { user } = renderApp();
    await shellReady();
    const area = screen.getByRole("region", { name: "Activity" });
    await within(area).findByRole("group", { name: `Held send: ${seededHold.subject}` });
    mock.failing.set("sync_trigger", { kind: "internal", message: "no route" });
    await user.keyboard("ss");
    await within(area).findByRole("alert");
    await user.keyboard("!");
    expect(within(area).queryByRole("alert")).toBeNull();
    expect(within(area).getByRole("group", { name: `Held send: ${seededHold.subject}` })).toBeInTheDocument();
    expect(await screen.findByText("Notices hidden; ! shows them, s l lists them")).toBeInTheDocument();
    // `u` still cancels the held send, whose card still shows.
    await user.keyboard("u");
    expect(mock.calls.filter((c) => c.cmd === "send_cancel_hold")).toHaveLength(1);
    // From another view as well.
    await user.keyboard(" c");
    await screen.findByRole("listbox", { name: "Contacts" });
    await user.keyboard("!");
    expect(await within(area).findByRole("alert")).toHaveTextContent("Sync of work did not start: no route");
    expect(JSON.parse(localStorage.getItem("mailypoppins.desktop.prefs.v1") ?? "{}").activityHidden).toBe(false);
  });
});

describe("config.toml and the daemon log in the editor", () => {
  it("s c and s f open them from Mail and from Contacts, and s l works there too", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("sc");
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH]));
    expect(await screen.findByText(`Opened config.toml in code --wait '${MOCK_CONFIG_PATH}'`)).toBeInTheDocument();
    await user.keyboard("sf");
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH, MOCK_LOG_PATH]));
    await user.keyboard(" c");
    await screen.findByRole("listbox", { name: "Contacts" });
    await user.keyboard("sc");
    await user.keyboard("sf");
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH, MOCK_LOG_PATH, MOCK_CONFIG_PATH, MOCK_LOG_PATH]));
    await user.keyboard("sl");
    const { log } = await openLog();
    expect(entries(log).filter(([, t]) => t?.startsWith("Opened"))).toHaveLength(4);
  });

  it("says why when there is no config.toml or no log yet, and logs it as a warning", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.configState = "absent";
    mock.logExists = false;
    await user.keyboard("sc");
    expect(await screen.findByText(NO_CONFIG)).toBeInTheDocument();
    await user.keyboard("sf");
    expect(await screen.findByText(`No log file found at ${MOCK_LOG_PATH}`)).toBeInTheDocument();
    expect(mock.editorOpens).toEqual([]);
    await user.keyboard("sl");
    const { log } = await openLog();
    expect(entries(log)).toEqual([
      ["warning", NO_CONFIG],
      ["warning", `No log file found at ${MOCK_LOG_PATH}`],
    ]);
  });

  it("the palette's rows run them", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Open config.toml{Enter}");
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH]));
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Activity log overlay{Enter}");
    await openLog();
  });
});

describe("the reader's copies", () => {
  it("the Copy menu copies the sender's address, the mp:// link and the subject", async () => {
    const { user } = renderApp();
    await shellReady();
    const writeText = stubClipboard();
    await user.keyboard("j");
    const toolbar = await screen.findByRole("toolbar", { name: "Message actions" });
    const pick = async (item: string) => {
      await user.click(within(toolbar).getByRole("button", { name: "Copy" }));
      await user.click(await screen.findByRole("menuitem", { name: item }));
    };
    await pick("Copy sender address");
    await pick("Copy link (mp://)");
    await pick("Copy subject");
    expect(writeText.mock.calls.map((c) => c[0])).toEqual(["ivana@example.com", inboxRow.selector, inboxRow.subject]);
    expect(await screen.findByText("Copied the subject")).toBeInTheDocument();
    // `y` still copies the selector, the same link.
    await user.keyboard("y");
    expect(writeText).toHaveBeenLastCalledWith(inboxRow.selector);
  });

  it("the palette's copy rows act on the open message, and say so without one", async () => {
    const { user } = renderApp();
    await shellReady();
    const writeText = stubClipboard();
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Copy subject{Enter}");
    expect(await screen.findByText("Open a message first")).toBeInTheDocument();
    expect(writeText).not.toHaveBeenCalled();
    await user.keyboard("j");
    await screen.findByRole("toolbar", { name: "Message actions" });
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Copy sender address{Enter}");
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("ivana@example.com"));
  });
});
