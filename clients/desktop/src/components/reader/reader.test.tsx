import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, emitEnvelope, mock } from "@/test/tauri-mock";
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

  it("in html mode fetches no plain-text body: the frame is the only path", async () => {
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

const argsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** Row 1001 carries a second part, so the picker and the checkboxes show. */
function twoParts() {
  mock.rows.work.inbox[0].attachments = [
    { name: "ledger-q3.pdf", size: 184223 },
    { name: "notes.txt", size: 812 },
  ];
}

describe("the reader's attachments", () => {
  it("opens a part with the system opener and saves one into the directory the dialog names", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const list = await within(reader()).findByRole("list", { name: "Attachments" });
    await user.click(within(list).getByRole("button", { name: "Open ledger-q3.pdf" }));
    await waitFor(() => expect(argsOf("attachment_open")).toEqual([{ account: "work", row_id: 1001, part: 0 }]));
    expect(await screen.findByText("Opened: ledger-q3.pdf")).toBeInTheDocument();

    await user.click(within(list).getByRole("button", { name: "Save ledger-q3.pdf" }));
    const dialog = await screen.findByRole("dialog", { name: "Save attachment" });
    const dir = within(dialog).getByRole("textbox", { name: "Directory" });
    await waitFor(() => expect(dir).toHaveFocus());
    expect(dir).toHaveValue("~/Downloads");
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(argsOf("attachment_save")).toEqual([{ account: "work", row_id: 1001, parts: [0], dest_dir: "~/Downloads" }]),
    );
    expect(await screen.findByText("Saved 1 file to ~/Downloads")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("Browse picks the directory natively, written with ~, and a closed or missing picker leaves the field", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("ts");
    const dialog = await screen.findByRole("dialog", { name: "Save attachment" });
    const dir = within(dialog).getByRole("textbox", { name: "Directory" });
    const browse = within(dialog).getByRole("button", { name: "Browse for a directory" });
    // Closed without a pick: nothing changes.
    await user.click(browse);
    await waitFor(() => expect(mock.pickerCalls).toHaveLength(1));
    expect(mock.pickerCalls[0]).toMatchObject({ directory: true, multiple: false, defaultPath: "/home/fixture/Downloads" });
    expect(dir).toHaveValue("~/Downloads");
    // A window with no picker says so, and the field stays to be typed.
    mock.pickerFailure = "plugin dialog not found";
    await user.click(browse);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("The file picker did not open (plugin dialog not found); type the path instead");
    expect(dir).toHaveValue("~/Downloads");
    mock.pickerFailure = null;
    mock.picked = "/home/fixture/Mail/Q3";
    await user.click(browse);
    await waitFor(() => expect(dir).toHaveValue("~/Mail/Q3"));
    expect(within(dialog).getByRole("alert")).toBeEmptyDOMElement();
    expect(within(dialog).getByRole("button", { name: "Save" })).toHaveFocus();
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(argsOf("attachment_save")).toEqual([{ account: "work", row_id: 1001, parts: [0], dest_dir: "~/Mail/Q3" }]),
    );
  });

  it("keeps the Save dialog open on a relative directory, and offers the last directory used next", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("ts");
    const dialog = await screen.findByRole("dialog", { name: "Save attachment" });
    const dir = within(dialog).getByRole("textbox", { name: "Directory" });
    await user.clear(dir);
    await user.type(dir, "Downloads{Enter}");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("is not an absolute path");
    expect(dir).toHaveAttribute("aria-invalid", "true");
    await user.clear(dir);
    await user.type(dir, "~/Mail/Q3{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await user.keyboard("ts");
    const again = await screen.findByRole("dialog", { name: "Save attachment" });
    const next = within(again).getByRole("textbox", { name: "Directory" });
    expect(next).toHaveValue("~/Mail/Q3");
    await waitFor(() => expect(next).toHaveFocus());
    await user.keyboard("{Enter}");
    await waitFor(() => expect(argsOf("attachment_save")).toHaveLength(3));
    expect(argsOf("attachment_save").map((a) => a?.dest_dir)).toEqual(["Downloads", "~/Mail/Q3", "~/Mail/Q3"]);
    await waitFor(() => expect(screen.getAllByText("Saved 1 file to ~/Mail/Q3")).toHaveLength(2));
  });

  it("to opens one attachment at once and picks among several; ts saves the checked parts", async () => {
    const { user } = renderApp(1400, twoParts);
    await shellReady();
    await user.keyboard("j");
    await within(reader()).findByRole("list", { name: "Attachments" });
    await user.keyboard("to");
    const picker = await screen.findByRole("dialog", { name: "Open attachment" });
    await waitFor(() => expect(within(picker).getByRole("button", { name: "Open ledger-q3.pdf" })).toHaveFocus());
    await user.click(within(picker).getByRole("button", { name: "Open notes.txt" }));
    await waitFor(() => expect(argsOf("attachment_open")).toEqual([{ account: "work", row_id: 1001, part: 1 }]));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    await user.keyboard("ts");
    const save = await screen.findByRole("dialog", { name: "Save attachments" });
    const boxes = within(save).getAllByRole("checkbox");
    expect(boxes.map((b) => (b as HTMLInputElement).checked)).toEqual([true, true]);
    await user.click(within(save).getByRole("checkbox", { name: /ledger-q3\.pdf/ }));
    await user.click(within(save).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(argsOf("attachment_save")).toEqual([{ account: "work", row_id: 1001, parts: [1], dest_dir: "~/Downloads" }]),
    );
  });

  it("says No attachments for a message without any", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    await user.keyboard("to");
    expect(await screen.findByText("No attachments")).toBeInTheDocument();
    expect(argsOf("attachment_open")).toEqual([]);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("tb and the toolbar open the daemon's rendition in the browser, and a message without markup says so", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const toolbar = await within(reader()).findByRole("toolbar", { name: "Message actions" });
    await user.click(within(toolbar).getByRole("button", { name: "Open in browser" }));
    await waitFor(() => expect(argsOf("html_open")).toEqual([{ account: "work", row_id: 1001 }]));
    expect(await screen.findByText("Opened in browser")).toBeInTheDocument();
    expect(mock.opened).toEqual(["/fixture/runtime/handles/h-1/message.html"]);
    // The click left focus in the reader, where j scrolls; J is the next message from any pane.
    await user.keyboard("J");
    await within(reader()).findByRole("heading", { name: "Angebot Dachsanierung" });
    await user.keyboard("tb");
    await waitFor(() => expect(argsOf("html_open")).toEqual([{ account: "work", row_id: 1001 }, { account: "work", row_id: 1002 }]));
    expect(await screen.findByText("No HTML version available")).toBeInTheDocument();
  });
});

/** A server-only hit, as `message.server_hit` carries it. */
function serverOnly(html: string | null = null) {
  return {
    account: "work",
    mailbox: "Archive",
    message_id: "<server-only@fixture.example>",
    row_id: null,
    selector: null,
    from: "Old Friend <old@example.com>",
    to: "me@example.com",
    cc: null,
    reply_to: null,
    bcc: null,
    subject: "The old thread",
    date_display: "Mon, 3 Mar 2025 09:00:00 +0100",
    date_sort: "2025-03-03T08:00:00",
    flags: { seen: true, answered: false, forwarded: false, flagged: false },
    has_attachments: true,
    is_invite: false,
    body_text: "The old thread.",
    html_body: html,
  };
}

async function onServerOnlyHit(user: ReturnType<typeof renderApp>["user"], html: string | null = null) {
  await user.click(screen.getByRole("searchbox", { name: /Filter this list/ }));
  await user.keyboard("thread{Shift>}{Enter}{/Shift}");
  await waitFor(() => expect(argsOf("search_server_start")).toHaveLength(1));
  act(() => emitEnvelope("message.server_hit", { operation_id: "op-1", hit: serverOnly(html) }));
  const hit = await screen.findByRole("option", { name: /The old thread/ });
  await user.click(hit);
  return within(reader()).findByText(/This message is on the server only/);
}

describe("a server-only hit in the reader", () => {
  it("F fetches it into the store, then the reader opens the row and a second F says it is there", async () => {
    const { user } = renderApp();
    await shellReady();
    await onServerOnlyHit(user);
    await user.keyboard("to");
    expect(await screen.findByText("This message is on the server only: fetch it first (F)")).toBeInTheDocument();
    await user.keyboard("F");
    await waitFor(() =>
      expect(argsOf("message_fetch")).toEqual([{ account: "work", mailbox: "Archive", message_id: "<server-only@fixture.example>" }]),
    );
    const activity = screen.getByRole("region", { name: "Activity" });
    await waitFor(() => expect(activity).toHaveTextContent("Fetched into the local store"));
    const hit = screen.getByRole("option", { name: /The old thread/ });
    await waitFor(() => expect(hit).not.toHaveAccessibleName(/on the server only/));
    expect(await within(reader()).findByRole("toolbar", { name: "Message actions" })).toBeInTheDocument();
    await waitFor(() => expect(argsOf("message_html_meta").slice(-1)[0]).toEqual({ account: "work", row_id: 1022 }));
    await user.keyboard("F");
    expect(await screen.findByText("Already in the local store")).toBeInTheDocument();
    expect(argsOf("message_fetch")).toHaveLength(1);
  });

  it("the Fetch button fetches, and a refusal lands in the activity area", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("message_fetch", { kind: "protocol", message: "The fetch failed: the server said NO", code: null });
    await onServerOnlyHit(user);
    await user.click(within(reader()).getByRole("button", { name: "Fetch" }));
    await waitFor(() => expect(argsOf("message_fetch")).toHaveLength(1));
    const activity = screen.getByRole("region", { name: "Activity" });
    await waitFor(() => expect(activity).toHaveTextContent("Fetch failed: The fetch failed: the server said NO"));
    expect(within(reader()).getByRole("button", { name: "Fetch" })).toBeEnabled();
  });

  it("opens the hit's own markup in the browser, and one with none has no button", async () => {
    const { user } = renderApp();
    await shellReady();
    await onServerOnlyHit(user, "<p>Old <b>thread</b></p>");
    await user.click(within(reader()).getByRole("button", { name: "Open in browser" }));
    await waitFor(() => expect(argsOf("hit_html_open")).toEqual([{ html: "<p>Old <b>thread</b></p>" }]));
    expect(argsOf("html_open")).toEqual([]);
  });
});
