// The embedded editor in the shell (ticket 0130, U3): the route, the exit
// paths, the navigate-away and window-close questions, and the discard
// order. The PTY is the mock's FakeBridge (`mock.terminal`), reached through
// the real `tauriBridge`, and xterm is a stand-in.

import { act, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, mock, requestClose } from "@/test/tauri-mock";
import { terms } from "@/test/xterm-fake";

vi.mock("@xterm/xterm", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-fit", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-webgl", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-unicode11", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-clipboard", () => import("@/test/xterm-fake"));

type User = ReturnType<typeof renderApp>["user"];

const DRAFT = "/fixture/work/drafts/fixture-draft-1.md";
const nvim = () => mock.settings.set("editor", "nvim");
const banner = () => document.querySelector('[data-slot="editing-banner"]') as HTMLElement;
const line = () => banner().querySelector("[data-editing]") as HTMLElement | null;
const reader = () => screen.getByRole("complementary", { name: "Reader" });
const panes = () => [...document.querySelectorAll<HTMLElement>('[data-slot="terminal"]')];
const shownPane = () => panes().find((p) => !p.hidden) ?? null;
const cmds = (...names: string[]) => mock.calls.map((c) => c.cmd).filter((c) => names.includes(c));
const kills = () => mock.terminal.of("kill").map((k) => k.session);
/** A row's identity: a draft's id, or a message row's element id. */
const rowKey = (el: Element | null | undefined) => el?.getAttribute("data-draft-id") ?? el?.id;
const selectedRow = () => rowKey(document.querySelector('[role="option"][aria-selected="true"]'));
const rowOf = (key: string) =>
  document.querySelector<HTMLElement>(`[role="option"][data-draft-id="${key}"]`) ?? document.getElementById(key);

beforeEach(() => {
  terms.length = 0;
});

/** Reply to the first inbox message in the embedded editor, until its spawn answered. */
async function replyEmbedded(user: User) {
  await user.keyboard("j");
  const message = (await within(reader()).findByRole("article")).getAttribute("aria-label");
  await user.keyboard("r");
  await waitFor(() => expect(line()).toHaveAttribute("data-status", "editing"));
  return message;
}

/** Edit the cursor draft of the Drafts list in the embedded editor. */
async function editDraftEmbedded(user: User, moves: string) {
  await user.keyboard("2");
  await screen.findByRole("listbox", { name: "Drafts messages" });
  await user.keyboard(moves);
  await user.keyboard("e");
  await waitFor(() => expect(line()).toHaveAttribute("data-status", "editing"));
}

/** Click another inbox row than the selected one. */
async function clickOtherRow(user: User) {
  const list = screen.getByRole("listbox");
  const other = within(list)
    .getAllByRole("option")
    .find((o) => o.getAttribute("aria-selected") !== "true");
  await user.click(other!);
  return rowKey(other);
}

describe("the route", () => {
  it("a terminal editor opens the reply in the reader area, and editor_open is not called", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    expect(mock.terminal.of("spawn").map((c) => c.req)).toEqual([
      expect.objectContaining({ account: "work", id: "fixture-draft-1", path: DRAFT, theme: "dark" }),
    ]);
    expect(mock.editorOpens).toEqual([]);
    expect(line()).toHaveTextContent(`Editing fixture-draft-1.md in nvim '${DRAFT}'; each save updates the list`);
    expect(line()).toHaveAttribute("data-route", "embedded");
    expect(shownPane()).not.toBeNull();
    expect(within(reader()).queryByRole("article")).toBeNull();
  });

  it("follows the setting: a GUI editor keeps editor_open, a terminal editor's full path spawns", async () => {
    const { user } = renderApp(1400, () => mock.settings.set("editor", "zed {path}"));
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("r");
    await waitFor(() => expect(mock.editorOpens).toEqual([DRAFT]));
    expect(mock.terminal.of("spawn")).toEqual([]);
    expect(line()).toHaveAttribute("data-route", "external");

    mock.settings.set("editor", "/opt/homebrew/bin/nvim");
    await user.click(within(banner()).getByRole("button", { name: /Done editing/ }));
    await user.keyboard("r");
    await waitFor(() => expect(mock.terminal.of("spawn")).toHaveLength(1));
    expect(mock.editorOpens).toEqual([DRAFT]);
  });

  it("a refused spawn is a failure notice, and the banner offers Reopen", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    mock.terminal.spawnFailure = { kind: "setup", message: "`nvim` from the editor setting was not found; set the editor setting to its full path." };
    await user.keyboard("j");
    await user.keyboard("r");
    await waitFor(() => expect(line()).toHaveAttribute("data-status", "failed"));
    expect(line()).toHaveTextContent("fixture-draft-1.md did not open in the editor");
    expect(await screen.findByText(/The editor did not open fixture-draft-1\.md: .*full path/)).toBeInTheDocument();
    await user.click(within(banner()).getByRole("button", { name: /Reopen in editor/ }));
    await waitFor(() => expect(line()).toHaveAttribute("data-status", "editing"));
    expect(mock.terminal.of("spawn").map((c) => c.req.path)).toEqual([DRAFT, DRAFT]);
  });
});

describe("the exit", () => {
  it("0 frees the PTY and shows the draft's summary, whose Close brings the message back", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    const message = await replyEmbedded(user);
    act(() => mock.terminal.exit(1, { code: 0, signal: null }));
    await waitFor(() => expect(kills()).toContain(1));
    await waitFor(() => expect(banner()).toBeEmptyDOMElement());
    const summary = await within(reader()).findByRole("article", { name: /^Draft: / });
    expect(summary).toHaveTextContent(DRAFT);
    expect(mock.calls.some((c) => c.cmd === "draft_preview" && c.args?.id === "fixture-draft-1")).toBe(true);
    expect(panes()).toEqual([]);
    await user.click(within(summary).getByRole("button", { name: "Close" }));
    expect(await within(reader()).findByRole("article", { name: message! })).toBeInTheDocument();
  });

  it("0 on a draft that is gone shows the previous message", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    const message = await replyEmbedded(user);
    mock.failing.set("draft_preview", { kind: "not_found", message: "no draft matches", code: -32602 });
    act(() => mock.terminal.exit(1, { code: 0, signal: null }));
    expect(await within(reader()).findByRole("article", { name: message! })).toBeInTheDocument();
  });

  it("a nonzero code frees the PTY, keeps the draft and the pane, and Reopen spawns again on the same path", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    act(() => mock.terminal.exit(1, { code: 1, signal: null }));
    await waitFor(() => expect(line()).toHaveAttribute("data-status", "exited"));
    expect(line()).toHaveTextContent("The editor of fixture-draft-1.md exited with status 1; the draft keeps what was saved");
    expect(kills()).toEqual([1]);
    expect(shownPane()).not.toBeNull();
    expect(cmds("draft_discard")).toEqual([]);

    await user.click(within(banner()).getByRole("button", { name: "Reopen in editor: fixture-draft-1.md" }));
    await waitFor(() => expect(line()).toHaveAttribute("data-status", "editing"));
    expect(mock.terminal.of("spawn").map((c) => c.req.path)).toEqual([DRAFT, DRAFT]);
    expect(panes()).toHaveLength(1);
    expect(mock.editorOpens).toEqual([]);
  });

  it("a signal is a crash, with the signal in the banner", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    act(() => mock.terminal.exit(1, { code: null, signal: 9 }));
    await waitFor(() => expect(line()).toHaveAttribute("data-status", "crashed"));
    expect(line()).toHaveTextContent("The editor of fixture-draft-1.md was ended by signal 9; the draft keeps what was saved");
    expect(kills()).toEqual([1]);
    await user.click(within(banner()).getByRole("button", { name: /Done editing/ }));
    expect(banner()).toBeEmptyDOMElement();
  });
});

describe("navigating away from a running editor", () => {
  async function asked(user: User) {
    await replyEmbedded(user);
    const before = selectedRow();
    const target = await clickOtherRow(user);
    const dialog = await screen.findByRole("dialog", { name: "Leave the editor?" });
    expect(selectedRow()).toBe(before);
    return { dialog, target };
  }

  it("Keep editing in the background is the default; the editor keeps running and Show brings it back", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    const { dialog, target } = await asked(user);
    const keep = within(dialog).getByRole("button", { name: "Keep editing in the background" });
    await waitFor(() => expect(keep).toHaveFocus());
    await user.click(keep);
    await waitFor(() => expect(selectedRow()).toBe(target));
    expect(await within(reader()).findByRole("article")).toBeInTheDocument();
    expect(kills()).toEqual([]);
    expect(panes()).toHaveLength(1);
    expect(shownPane()).toBeNull();
    expect(line()).toHaveAttribute("data-status", "background");
    expect(line()).toHaveTextContent(`fixture-draft-1.md is open in nvim '${DRAFT}' in the background`);

    await user.click(within(banner()).getByRole("button", { name: "Show the editor: fixture-draft-1.md" }));
    await waitFor(() => expect(shownPane()).not.toBeNull());
    expect(line()).toHaveAttribute("data-status", "editing");
    expect(mock.terminal.of("spawn")).toHaveLength(1);
  });

  it("Close the editor kills the child, keeps the draft, then navigates", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    const { dialog, target } = await asked(user);
    await user.click(within(dialog).getByRole("button", { name: "Close the editor" }));
    await waitFor(() => expect(selectedRow()).toBe(target));
    expect(kills()).toContain(1);
    expect(banner()).toBeEmptyDOMElement();
    expect(cmds("draft_discard")).toEqual([]);
    expect(panes()).toEqual([]);
  });

  it("Stay cancels the navigation", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    const { dialog } = await asked(user);
    const before = selectedRow();
    await user.click(within(dialog).getByRole("button", { name: "Stay" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(selectedRow()).toBe(before);
    expect(shownPane()).not.toBeNull();
    expect(kills()).toEqual([]);
  });

  it("asks before a mailbox, a view or a search too", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    // A mailbox (a digit), a view (Space a, the Calendar) and a search (/ then Enter in the filter).
    for (const keys of ["2", " a", "/ledger{Enter}"]) {
      await user.keyboard(keys);
      const dialog = await screen.findByRole("dialog", { name: "Leave the editor?" });
      await user.click(within(dialog).getByRole("button", { name: "Stay" }));
      await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
      expect(shownPane()).not.toBeNull();
    }
  });

  it("selecting the draft of a background editor brings it back without asking", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await editDraftEmbedded(user, "j");
    const first = selectedRow();
    // The editor has the keys; the list is clicked.
    const other = await clickOtherRow(user);
    const dialog = await screen.findByRole("dialog", { name: "Leave the editor?" });
    await user.click(within(dialog).getByRole("button", { name: "Keep editing in the background" }));
    await waitFor(() => expect(selectedRow()).toBe(other));
    expect(shownPane()).toBeNull();
    expect(await within(reader()).findByRole("article", { name: /^Draft: / })).toBeInTheDocument();
    await user.click(rowOf(first!)!);
    await waitFor(() => expect(shownPane()).not.toBeNull());
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(mock.terminal.of("spawn")).toHaveLength(1);
  });
});

describe("a running editor is never replaced", () => {
  it("e on its draft after the setting turned to a GUI editor shows the running editor and launches nothing", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await editDraftEmbedded(user, "j");
    const draft = selectedRow()!;
    mock.settings.set("editor", "zed {path}");
    // The list takes the keys back; the editor keeps the reader area.
    await user.click(rowOf(draft)!);
    await user.keyboard("e");
    await waitFor(() => expect(mock.calls.filter((c) => c.cmd === "draft_path")).toHaveLength(2));
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(mock.editorOpens).toEqual([]);
    expect(kills()).toEqual([]);
    expect(panes()).toHaveLength(1);
    expect(shownPane()).not.toBeNull();
    expect(line()).toHaveAttribute("data-route", "embedded");
    expect(line()).toHaveAttribute("data-status", "editing");
    expect(mock.terminal.of("spawn")).toHaveLength(1);
  });

  it("the same e from the background brings the editor back", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await editDraftEmbedded(user, "j");
    const draft = selectedRow()!;
    await clickOtherRow(user);
    const dialog = await screen.findByRole("dialog", { name: "Leave the editor?" });
    await user.click(within(dialog).getByRole("button", { name: "Keep editing in the background" }));
    await waitFor(() => expect(shownPane()).toBeNull());
    mock.settings.set("editor", "zed {path}");
    // Selecting the draft shows its editor; the list then takes the keys again.
    await user.click(rowOf(draft)!);
    await waitFor(() => expect(shownPane()).not.toBeNull());
    await user.click(rowOf(draft)!);
    await user.keyboard("e");
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(mock.editorOpens).toEqual([]);
    expect(kills()).toEqual([]);
    expect(shownPane()).not.toBeNull();
  });
});

describe("a removed draft", () => {
  it("keeps a running editor mounted and kills nothing, as when a save broke the frontmatter", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await editDraftEmbedded(user, "j");
    const draft = selectedRow()!;
    act(() => emitEnvelope("state.remove", { resource: `draft:work/${draft}` }));
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(kills()).toEqual([]);
    expect(panes()).toHaveLength(1);
    expect(shownPane()).not.toBeNull();
    expect(line()).toHaveAttribute("data-status", "editing");
    expect(terms.filter((t) => t.disposed)).toEqual([]);
  });
});

describe("discard", () => {
  it("d on a draft open in the editor confirms, kills the editor, then discards", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await editDraftEmbedded(user, "j");
    await user.keyboard("d");
    const dialog = await screen.findByRole("dialog", { name: "Delete this email?" });
    expect(kills()).toEqual([]);
    await user.click(within(dialog).getByRole("button", { name: /Delete/ }));
    await waitFor(() => expect(cmds("draft_discard")).toHaveLength(1));
    expect(cmds("terminal_kill", "draft_discard")[0]).toBe("terminal_kill");
    await waitFor(() => expect(banner()).toBeEmptyDOMElement());
    expect(panes()).toEqual([]);
  });
});

describe("closing the window", () => {
  it("closes at once with no editor running", async () => {
    renderApp(1400, nvim);
    await shellReady();
    await waitFor(() => expect(mock.closeHandler).not.toBeNull());
    await act(() => requestClose());
    expect(mock.windowDestroyed).toBe(true);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("asks with Close the editor as the default; Stay keeps the window and the editor", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    await act(() => requestClose());
    expect(mock.windowDestroyed).toBe(false);
    const dialog = await screen.findByRole("dialog", { name: "Close the window?" });
    expect(dialog).toHaveTextContent("fixture-draft-1.md is still open in the editor");
    expect(within(dialog).queryByRole("button", { name: "Keep editing in the background" })).toBeNull();
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Close the editor" })).toHaveFocus());
    await user.click(within(dialog).getByRole("button", { name: "Stay" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(mock.windowDestroyed).toBe(false);
    expect(kills()).toEqual([]);
  });

  it("Close the editor kills every editor, then destroys the window", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    await act(() => requestClose());
    const dialog = await screen.findByRole("dialog", { name: "Close the window?" });
    await user.click(within(dialog).getByRole("button", { name: "Close the editor" }));
    await waitFor(() => expect(mock.windowDestroyed).toBe(true));
    expect(kills()).toContain(1);
  });

  it("an editor that already ended does not hold the close", async () => {
    const { user } = renderApp(1400, nvim);
    await shellReady();
    await replyEmbedded(user);
    act(() => mock.terminal.exit(1, { code: 1, signal: null }));
    await waitFor(() => expect(line()).toHaveAttribute("data-status", "exited"));
    await act(() => requestClose());
    expect(mock.windowDestroyed).toBe(true);
  });
});
