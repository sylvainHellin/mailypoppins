// The notes on mail in the shell (PERSO-315): `n`'s prompt, the glyph beside
// the star, `fN`, the reader's band, `N`'s list and the hand-off to the agent
// in the panel beside the reader, against the fixture backend.

import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { closeNote, mock } from "@/test/tauri-mock";
import { GUI_ENTRIES, paletteEntries } from "@/keymap/catalog";

vi.mock("@xterm/xterm", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-fit", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-webgl", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-unicode11", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-clipboard", () => import("@/test/xterm-fake"));

const FIRST = "mp://work/inbox/quarterly-ledger-review-1001@fixture.example";
const SECOND = "mp://work/inbox/angebot-dachsanierung-1002@fixture.example";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** The shell on the Inbox, the cursor on its first row. */
async function onFirstRow(before?: () => void) {
  const { user } = renderApp(1400, before);
  await shellReady();
  await user.keyboard("j");
  await screen.findByRole("toolbar", { name: "Message actions" });
  return { user };
}

async function prompt(user: ReturnType<typeof renderApp>["user"]) {
  await user.keyboard("n");
  return screen.findByRole("dialog", { name: "Note" });
}

const inbox = () => screen.getByRole("listbox", { name: /Inbox/ });
const row = (id: number) => inbox().querySelector<HTMLElement>(`[data-row-id="${id}"]`)!;

describe("the note prompt", () => {
  it("n offers the daemon's presets; a letter that needs no text saves at once, and the row shows its glyph", async () => {
    const { user } = await onFirstRow();
    const dialog = await prompt(user);
    expect(within(dialog).getAllByRole("button").map((b) => b.getAttribute("data-preset")).filter((p) => p !== null)).toEqual(["r", "d", "s", "e", "p", "t", "f", ""]);
    await user.keyboard("s");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    expect(callsOf("note_set")).toEqual([{ params: { account: "work", selector: FIRST, preset: "s", text: "" } }]);
    expect(await screen.findByText("Note saved on Quarterly ledger review")).toBeInTheDocument();
    await waitFor(() => expect(within(row(1001)).getByRole("img", { name: "Note open" })).toBeInTheDocument());
    expect(row(1001)).toHaveAttribute("aria-label", expect.stringContaining("note open"));
  });

  it("a letter that needs a text waits for the line, Space is free text, and Escape writes nothing", async () => {
    const { user } = await onFirstRow();
    let dialog = await prompt(user);
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    dialog = await prompt(user);
    await user.keyboard("r");
    const field = await within(dialog).findByRole("textbox", { name: "Note text" });
    await waitFor(() => expect(field).toHaveFocus());
    expect(dialog).toHaveTextContent("Reply: Draft a reply in my voice from this gist.");
    await user.keyboard("yes, Tuesday works{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    expect(callsOf("note_set")).toEqual([{ params: { account: "work", selector: FIRST, preset: "r", text: "yes, Tuesday works" } }]);

    await user.keyboard("j");
    dialog = await prompt(user);
    await user.keyboard(" ");
    const free = await within(dialog).findByRole("textbox", { name: "Note text" });
    await waitFor(() => expect(free).toHaveFocus());
    await user.keyboard("{Enter}");
    expect(within(dialog).getByRole("alert")).toHaveTextContent("A note needs a preset or a text");
    await user.keyboard("ask Robin for the second quote{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    expect(callsOf("note_set").slice(-1)[0]).toEqual({ params: { account: "work", selector: SECOND, text: "ask Robin for the second quote" } });
  });

  it("on marked rows the note goes on each through note_set_many", async () => {
    const { user } = await onFirstRow();
    await user.keyboard("vv");
    const dialog = await prompt(user);
    expect(dialog).toHaveTextContent("2 emails, the same note on each");
    await user.keyboard("t");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    expect(callsOf("note_set_many")).toEqual([{ params: { account: "work", selectors: [FIRST, SECOND], preset: "t", text: "" } }]);
  });

  it("n on a noted mail edits its text; on a done one it shows the result, a letter instructs anew and Backspace clears", async () => {
    const { user } = await onFirstRow();
    let dialog = await prompt(user);
    await user.keyboard("r");
    await within(dialog).findByRole("textbox", { name: "Note text" });
    await user.keyboard("first draft{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());

    dialog = await prompt(user);
    const field = await within(dialog).findByRole("textbox", { name: "Note text" });
    expect(field).toHaveValue("first draft");
    expect(within(dialog).getByRole("button", { pressed: true })).toHaveAttribute("data-preset", "r");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());

    const id = mock.notes.notes[0].id;
    act(() => void closeNote(id, "Drafted the reply"));
    dialog = await prompt(user);
    expect(await within(dialog).findByText("Drafted the reply")).toBeInTheDocument();
    await user.keyboard("s");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    expect(callsOf("note_set").slice(-1)[0]).toEqual({ params: { account: "work", selector: FIRST, preset: "s", text: "" } });
    expect(mock.notes.notes[0]).toMatchObject({ id, state: "open", preset: "s" });

    act(() => void closeNote(id, "Three lines posted"));
    dialog = await prompt(user);
    await within(dialog).findByText("Three lines posted");
    await user.keyboard("{Backspace}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull());
    expect(callsOf("note_clear")).toEqual([{ id }]);
    expect(mock.notes.notes).toEqual([]);
  });
});

describe("noted mail in the list and the reader", () => {
  it("fN shows only the noted rows, and again every row", async () => {
    const { user } = await onFirstRow();
    await prompt(user);
    await user.keyboard("s");
    await waitFor(() => expect(within(row(1001)).getByRole("img", { name: "Note open" })).toBeInTheDocument());
    const before = within(inbox()).getAllByRole("option").length;
    expect(before).toBeGreaterThan(1);
    await user.keyboard("fN");
    await waitFor(() => expect(within(inbox()).getAllByRole("option").map((o) => o.getAttribute("data-row-id"))).toEqual(["1001"]));
    expect(screen.getByRole("button", { name: "Noted mail only" })).toHaveAttribute("aria-pressed", "true");
    await user.keyboard("fN");
    await waitFor(() => expect(within(inbox()).getAllByRole("option")).toHaveLength(before));
  });

  it("the reader shows the note's band, and its result once the agent closed it", async () => {
    const { user } = await onFirstRow();
    await prompt(user);
    await user.keyboard("e");
    const band = await screen.findByRole("region", { name: "Note" });
    await waitFor(() => expect(band).toHaveTextContent("Explain"));
    expect(band).toHaveTextContent("Note open");
    act(() => void closeNote(mock.notes.notes[0].id, "Ivana runs the ledger; last review was in June."));
    await waitFor(() => expect(screen.getByRole("region", { name: "Note" })).toHaveTextContent("Ivana runs the ledger; last review was in June."));
    expect(screen.getByRole("region", { name: "Note" })).toHaveTextContent("Note done");
  });
});

describe("the notes list and the hand-off", () => {
  async function withNotes() {
    const ctx = await onFirstRow();
    await prompt(ctx.user);
    await ctx.user.keyboard("s");
    await waitFor(() => expect(mock.notes.notes).toHaveLength(1));
    await ctx.user.keyboard("j");
    await prompt(ctx.user);
    await ctx.user.keyboard("p");
    await waitFor(() => expect(mock.notes.notes).toHaveLength(2));
    await ctx.user.keyboard("N");
    const dialog = await screen.findByRole("dialog", { name: "Notes" });
    const list = within(dialog).getByRole("listbox", { name: "Notes" });
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(2));
    return { ...ctx, dialog, list };
  }

  const cursorNote = (list: HTMLElement) => within(list).getByRole("option", { selected: true }).getAttribute("data-note");

  it("N lists every note newest first; d drops the cursor note, D clears the done ones, and a adds a note for a new email", async () => {
    const { user, list, dialog } = await withNotes();
    const [newest, oldest] = mock.notes.notes.map((n) => n.id).reverse();
    expect(within(list).getAllByRole("option").map((o) => o.getAttribute("data-note"))).toEqual([newest, oldest]);
    expect(cursorNote(list)).toBe(newest);
    await user.keyboard("j");
    expect(cursorNote(list)).toBe(oldest);
    act(() => void closeNote(newest, "Fixed two typos"));
    await waitFor(() => expect(within(list).getByText("Fixed two typos")).toBeInTheDocument());
    await user.keyboard("D");
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(1));
    expect(callsOf("note_clear_done")).toHaveLength(1);
    await user.keyboard("d");
    await waitFor(() => expect(within(list).queryAllByRole("option")).toHaveLength(0));
    expect(callsOf("note_clear")).toEqual([{ id: oldest }]);
    expect(dialog).toHaveTextContent("No notes");

    await user.keyboard("a");
    const note = await screen.findByRole("dialog", { name: "Note" });
    expect(note).toHaveTextContent("a new email from work");
    await user.keyboard(" ");
    await within(note).findByRole("textbox", { name: "Note text" });
    await user.keyboard("Invite the team to the offsite{Enter}");
    // Back to the list it came from, with the new note in it.
    const back = await screen.findByRole("dialog", { name: "Notes" });
    await waitFor(() => expect(within(back).getAllByRole("option")).toHaveLength(1));
    expect(callsOf("note_set").slice(-1)[0]).toEqual({ params: { account: "work", text: "Invite the team to the offsite" } });
  });

  it("e edits the cursor note, Enter opens its mail, and y copies the batch prompt", async () => {
    const { user, list } = await withNotes();
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      await user.keyboard("y");
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(1));
      expect(writeText.mock.calls[0]).toEqual([expect.stringMatching(/^I have notes on 2 emails\./)]);
      expect(await screen.findByText("Copied the notes' prompt")).toBeInTheDocument();
    } finally {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    }
    await user.keyboard("j");
    const oldest = cursorNote(list);
    await user.keyboard("e");
    const note = await screen.findByRole("dialog", { name: "Note" });
    await user.keyboard("{Escape}");
    await waitFor(() => expect(note).not.toBeInTheDocument());
    // Escape on a prompt from the list goes back to it.
    const back = await screen.findByRole("dialog", { name: "Notes" });
    const backList = within(back).getByRole("listbox", { name: "Notes" });
    await waitFor(() => expect(within(backList).getAllByRole("option")).toHaveLength(2));
    await user.keyboard("j");
    expect(cursorNote(backList)).toBe(oldest);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Notes" })).toBeNull());
    await waitFor(() => expect(row(1001)).toHaveAttribute("data-cursor", "true"));
  });

  it("x opens the agent in the panel beside the reader, and marks the notes handed off once it started", async () => {
    const { user } = await withNotes();
    const ids = mock.notes.notes.map((n) => n.id).reverse();
    await user.keyboard("x");
    const panel = await screen.findByRole("complementary", { name: "Agent" });
    await waitFor(() => expect(mock.terminal.of("agent")).toHaveLength(1));
    const [spawn] = mock.terminal.of("agent").map((c) => c.req);
    expect(spawn.text).toMatch(/^I have notes on 2 emails\./);
    expect(spawn.date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    await waitFor(() => expect(callsOf("note_handoff")).toEqual([{ ids }]));
    expect(mock.notes.notes.map((n) => n.state)).toEqual(["handed_off", "handed_off"]);
    expect(await screen.findByText("Handed 2 notes to the agent")).toBeInTheDocument();
    expect(within(panel).getByRole("status")).toHaveTextContent("Working on 2 notes");

    // The panel stays while another mail is read, hides and shows again.
    await user.keyboard("{Escape}");
    await user.keyboard("j");
    expect(screen.getByRole("complementary", { name: "Agent" })).toBeInTheDocument();
    await user.click(within(panel).getByRole("button", { name: "Hide the agent panel" }));
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "Agent" })).toBeNull());
    await user.click(screen.getByRole("button", { name: "Show the agent panel" }));
    const again = await screen.findByRole("complementary", { name: "Agent" });

    // A second hand-off while it runs is refused; the exit ends it, and Close frees it.
    await user.keyboard("N");
    await screen.findByRole("dialog", { name: "Notes" });
    await user.keyboard("x");
    expect(await screen.findByText(/An agent is already running in the panel/)).toBeInTheDocument();
    await user.keyboard("{Escape}");
    act(() => mock.terminal.exit(1, { code: 0, signal: null }));
    await waitFor(() => expect(within(again).getByRole("status")).toHaveTextContent("Done with 2 notes"));
    await user.click(within(again).getByRole("button", { name: "Close the agent panel" }));
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "Agent" })).toBeNull());
    expect(mock.terminal.of("kill").map((k) => k.session)).toContain(1);
  });

  it("an agent that does not start leaves the notes open and says why", async () => {
    const { user } = await withNotes();
    mock.terminal.spawnFailure = { kind: "setup", message: "`pi` from [agent] was not found on the login shell's PATH; set its full path in [agent.commands] of config.toml." };
    await user.keyboard("x");
    expect(await screen.findByText(/^The agent did not start: `pi` from \[agent\] was not found/)).toBeInTheDocument();
    expect(callsOf("note_handoff")).toEqual([]);
    expect(mock.notes.notes.every((n) => n.state === "open")).toBe(true);
  });

  it("open_in = clipboard copies the prompt and marks nothing; open_in = paseo runs paseo and marks the notes", async () => {
    const { user } = await withNotes();
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      mock.agent.route = "clipboard";
      await user.keyboard("x");
      await waitFor(() => expect(writeText).toHaveBeenCalledTimes(1));
      expect(writeText.mock.calls[0]).toEqual([expect.stringMatching(/^I have notes on 2 emails\./)]);
      expect(callsOf("note_handoff")).toEqual([]);
      expect(mock.terminal.of("agent")).toEqual([]);
    } finally {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    }
    mock.agent.route = "paseo";
    await user.keyboard("x");
    await waitFor(() => expect(mock.paseoRuns).toHaveLength(1));
    await waitFor(() => expect(callsOf("note_handoff")).toHaveLength(1));
    expect(await screen.findByText("Handed 2 notes to pi through paseo")).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Agent" })).toBeNull();
  });
});

describe("the notes' keys", () => {
  it("n, N, fN, y and x run from the palette under the TUI's descriptions and keys, and the list's own keys are in the help", () => {
    // keymap.json's rows, each once: the desktop adds only its agent panel.
    const rows = paletteEntries();
    const row = (id: string) => rows.find((e) => e.id === id);
    expect(row("note")).toMatchObject({ section: "MESSAGE (list, headers, body)", label: "Note for the agent (a preset, or Space for free text)", keys: ["n"] });
    expect(row("notes_list")).toMatchObject({ section: "GLOBAL", label: "Notes for the agent (every account)", keys: ["N"] });
    expect(row("toggle_noted_only")).toMatchObject({ section: "EMAIL LIST", label: "Show noted only (toggle)", keys: ["fN"] });
    expect(row("copy_notes_prompt")).toMatchObject({ section: "NOTES", label: "Copy the batch prompt of the open notes", keys: ["y"] });
    expect(row("hand_off_notes")).toMatchObject({ section: "NOTES", label: "Hand the open notes to the agent ([agent] open_in)", keys: ["x"] });
    const notesIds = ["note", "notes_list", "toggle_noted_only", "copy_notes_prompt", "hand_off_notes", "toggle_agent_panel"];
    expect(GUI_ENTRIES.filter((e) => e.section === "NOTES" || (e.id !== null && notesIds.includes(e.id)))).toEqual([
      { section: "NOTES", label: "Agent panel: show / hide", keys: [], id: "toggle_agent_panel", badge: null },
    ]);
    const listKeys = rows.filter((e) => e.section === "NOTES" && e.badge === "key").map((e) => e.keys[0]);
    expect(listKeys).toEqual(["j/k", "Enter", "a", "e", "d", "D", "Esc / q"]);
  });

  it("the palette runs n and N, and from another view n says to go back to Mail first", async () => {
    const { user } = await onFirstRow();
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Notes for the agent{Enter}");
    expect(await screen.findByRole("dialog", { name: "Notes" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Notes" })).toBeNull());
    await user.keyboard(" c");
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Note for the agent (a preset{Enter}");
    expect(await screen.findByText("Go back to Mail first (Escape): this acts on the mailbox selection")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Note" })).toBeNull();
  });
});
