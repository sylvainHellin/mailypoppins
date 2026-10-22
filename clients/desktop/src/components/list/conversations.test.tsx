// The conversation list end to end (ticket 0142, step 5): the rows, the
// expansion on Enter, `l` and `h`, the actions on a collapsed row, and the
// re-read on any change of the account, against the fixture mock.

import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import App from "@/App";
import { PREFS_KEY } from "@/app/prefs";
import { initialState } from "@/app/state";
import { shellReady } from "@/test/render";
import { setWidth } from "@/test/setup";
import { CANNED_THREAD, emitEnvelope, mock, resetMock } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const header = (threadId: string) => document.querySelector<HTMLElement>(`[role="option"][data-thread-id="${threadId}"]`)!;
const offsite = () => header(CANNED_THREAD);
const members = () => [...document.querySelectorAll<HTMLElement>('[role="option"][data-member]')];
const cursorRow = () => document.querySelector<HTMLElement>('[role="option"][data-cursor="true"]');

/** The app with every account listed by conversation, the default the stored prefs start from. */
function renderGrouped() {
  resetMock();
  setWidth(1400);
  const user = userEvent.setup();
  render(<App initial={initialState()} />);
  return { user };
}

describe("the conversation list", () => {
  it("lists one row per conversation, the canned thread as one row of three with its draft", async () => {
    renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    expect(callsOf("list_messages")).toContainEqual({ account: "work", mailbox: "inbox", conversations: true });
    expect(screen.getByText("8 conversations")).toBeInTheDocument();
    expect(offsite()).toHaveAccessibleName(/Marta Silva, Team offsite in October, .*3 messages, collapsed, has a draft/);
    expect(within(offsite()).getByText("(3)")).toBeInTheDocument();
    expect(offsite().querySelector('[data-slot="conversation-draft"]')).not.toBeNull();
    // A conversation of one reads as a message row: no count, no chevron.
    const lone = header("<angebot-dachsanierung-1002@fixture.example>");
    expect(lone).toHaveAccessibleName(/Robin Keller, Angebot Dachsanierung, .*unread/);
    expect(within(lone).queryByRole("button", { name: /Expand conversation/ })).toBeNull();
  });

  it("expands on Enter and keeps the cursor on the header, then reads on the next Enter", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("{Enter}");
    await waitFor(() => expect(members()).toHaveLength(4));
    // Newest first, like the list around them.
    expect(members().map((m) => m.getAttribute("aria-label")?.split(":")[0])).toEqual(["Drafts", "Sent", "Sent", "Inbox"]);
    expect(members().map((m) => m.getAttribute("data-member"))).toEqual(["draft:offsite-note", "row:1000", "row:1010", "row:1007"]);
    expect(callsOf("message_thread")).toEqual([{ account: "work", row_id: 1007, mailbox: "inbox" }]);
    expect(cursorRow()).toBe(offsite());
    expect(offsite()).toHaveAttribute("aria-expanded", "true");

    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.getByRole("complementary", { name: "Reader" })).toHaveAttribute("data-focused", "true"));
    expect(callsOf("message_html_meta")).toContainEqual({ account: "work", row_id: 1007 });
  });

  it("tells the user's own members apart: a tinted mailbox chip and Me, with the address it went out from", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("{Enter}");
    await waitFor(() => expect(members()).toHaveLength(4));
    const [draft, sentLater, sentEarlier, inbox] = members();
    // `own` is the daemon's, decided on the address: both Sent replies and the draft.
    expect(members().map((m) => m.hasAttribute("data-own"))).toEqual([true, true, true, false]);
    const chip = (m: HTMLElement) => m.querySelector<HTMLElement>('[data-slot="member-mailbox"]')!;
    expect(chip(sentEarlier)).toHaveClass("text-link", "bg-link/10", "border-framing/60");
    expect(chip(inbox)).toHaveClass("text-muted-foreground", "border-border");
    expect(chip(inbox)).not.toHaveClass("text-link");
    // "Me" on the name line, the bare address under it; others keep their header.
    expect(sentEarlier).toHaveAccessibleName(/^Sent: Me, /);
    expect(within(sentEarlier).getByText("Me")).toBeInTheDocument();
    expect(within(sentEarlier).getByText("me@example.com")).toBeInTheDocument();
    expect(inbox).toHaveAccessibleName(/^Inbox: Marta Silva, /);
    // A draft keeps its own wording, and its chip is tinted as the user's.
    expect(draft).toHaveAccessibleName(/^Drafts: Draft, /);
    expect(chip(draft)).toHaveClass("text-link");
    // On the cursor row the chip text takes the selection foreground, the tint stays.
    await user.click(sentLater);
    await waitFor(() => expect(cursorRow()).toBe(sentLater));
    expect(chip(sentLater)).toHaveClass("text-selection-foreground", "bg-link/10");
    expect(chip(sentLater)).not.toHaveClass("text-link");
  });

  it("draws an expanded conversation as one block: a tinted header, indented members on a lighter tint, a rule after the last", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    expect(offsite()).not.toHaveAttribute("data-open");
    expect(offsite()).not.toHaveClass("bg-muted");
    await user.click(offsite());
    await user.keyboard("{Enter}");
    await waitFor(() => expect(members()).toHaveLength(4));
    // The cursor sits on the header; move it off so the tint shows there.
    await user.click(members()[1]);
    expect(offsite()).toHaveAttribute("data-open", "true");
    expect(offsite()).toHaveClass("bg-muted", "shadow-[inset_2px_0_0_var(--color-framing)]");
    for (const m of members().filter((m) => m !== cursorRow())) {
      expect(m).toHaveClass("pl-12", "bg-muted/50", "shadow-[inset_2px_0_0_var(--color-framing)]");
    }
    // Only the last member closes the group, with the framing-tinted rule.
    expect(members().map((m) => m.hasAttribute("data-last"))).toEqual([false, false, false, true]);
    expect(members()[3]).toHaveClass("border-framing/40");
    expect(members()[2]).toHaveClass("border-border");
    // The row after the block stands alone on the pane's background.
    const rows = [...document.querySelectorAll<HTMLElement>('[role="option"]')];
    const after = rows[rows.indexOf(members()[3]) + 1];
    expect(after).toHaveAttribute("data-thread-id");
    expect(after).not.toHaveAttribute("data-open");
    expect(after).not.toHaveClass("bg-muted");
    expect(after.className).not.toContain("shadow-[inset");

    await user.keyboard("h");
    await waitFor(() => expect(members()).toHaveLength(0));
    expect(offsite()).not.toHaveAttribute("data-open");
  });

  it("h collapses from a member back to its header, l expands again, and a member in Sent opens in place", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.keyboard("jjj");
    expect(cursorRow()?.getAttribute("data-member")).toBe("row:1010");
    await waitFor(() => expect(callsOf("message_html_meta")).toContainEqual({ account: "work", row_id: 1010 }));
    // The sidebar keeps the inbox: the member opens by its row id where it lives.
    expect(screen.getByText("8 conversations")).toBeInTheDocument();

    await user.keyboard("h");
    await waitFor(() => expect(members()).toHaveLength(0));
    expect(cursorRow()).toBe(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
  });

  it("M on a member in Sent offers the inbox and leaves Sent out, wherever the list is", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.keyboard("jjj");
    expect(cursorRow()?.getAttribute("data-member")).toBe("row:1010");
    await user.keyboard("M");
    const picker = await screen.findByRole("dialog", { name: "Move the message to" });
    const slugs = () => [...picker.querySelectorAll("[data-testid='move-destination']")].map((e) => e.getAttribute("data-slug"));
    expect(slugs()).toEqual(["inbox", "archive"]);
    await user.keyboard("inbox{Enter}");
    await waitFor(() => expect(callsOf("message_move")).toEqual([{ account: "work", row_ids: [1010], destination: "inbox" }]));
  });

  it("selecting the draft member previews the draft", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(within(offsite()).getByRole("button", { name: "Expand conversation (l)" }));
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.click(members()[0]);
    await waitFor(() => expect(callsOf("draft_preview")).toContainEqual({ account: "work", id: "offsite-note" }));
  });

  it("e on the draft member opens the draft in the editor, as in Drafts", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.keyboard("j");
    expect(cursorRow()?.getAttribute("data-member")).toBe("draft:offsite-note");
    await user.keyboard("e");
    await waitFor(() => expect(mock.editorOpens).toEqual(["/fixture/work/drafts/offsite-note.md"]));
    expect(callsOf("draft_path")).toContainEqual({ account: "work", id: "offsite-note" });
    expect(callsOf("message_markdown_path")).toEqual([]);
  });

  it("the reader's Edit on the draft member opens the draft in the editor", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(within(offsite()).getByRole("button", { name: "Expand conversation (l)" }));
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.click(members()[0]);
    await user.click(await screen.findByRole("button", { name: /Edit in editor/ }));
    await waitFor(() => expect(mock.editorOpens).toEqual(["/fixture/work/drafts/offsite-note.md"]));
  });

  it("cA approves the draft member, its reader then offers Back to draft, and cD demotes it", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.keyboard("j");
    expect(cursorRow()?.getAttribute("data-member")).toBe("draft:offsite-note");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    await within(reader).findByRole("button", { name: /Approve/ });
    expect(within(reader).queryByRole("button", { name: /Attach file/ })).toBeNull();
    await user.keyboard("cA");
    await waitFor(() => expect(callsOf("draft_approve")).toEqual([{ account: "work", ids: ["offsite-note"] }]));
    await within(reader).findByRole("button", { name: /Back to draft/ });
    await user.keyboard("cD");
    await waitFor(() => expect(callsOf("draft_demote")).toEqual([{ account: "work", ids: ["offsite-note"] }]));
    await within(reader).findByRole("button", { name: /Approve/ });
  });

  it("x on the draft member confirms with the draft's own status and recipients, and ce edits them", async () => {
    const { user } = renderGrouped();
    const d = mock.drafts.work.drafts.find((x) => x.id === "offsite-note")!;
    Object.assign(d, { status: "approved", to: "Marta Silva <marta@fixture.example>", subject: "Re: Team offsite in October" });
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.keyboard("j");
    expect(cursorRow()?.getAttribute("data-member")).toBe("draft:offsite-note");
    await user.keyboard("x");
    const confirm = await screen.findByRole("dialog", { name: "Send this email?" });
    expect(confirm).toHaveTextContent("To: Marta Silva <marta@fixture.example> - Re: Team offsite in October");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Send this email?" })).toBeNull());

    await user.keyboard("ce");
    const dialog = await screen.findByRole("dialog", { name: "Edit recipients" });
    expect(within(dialog).getByDisplayValue("Marta Silva <marta@fixture.example>")).toBeInTheDocument();
  });

  it("y on a member copies its own selector, and on a draft member its file", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    try {
      await user.click(offsite());
      await user.keyboard("l");
      await waitFor(() => expect(members()).toHaveLength(4));
      await user.keyboard("j");
      expect(cursorRow()?.getAttribute("data-member")).toBe("draft:offsite-note");
      await user.keyboard("y");
      expect(writeText).toHaveBeenLastCalledWith("/fixture/work/drafts/offsite-note.md");
      await user.keyboard("jj");
      expect(cursorRow()?.getAttribute("data-member")).toBe("row:1010");
      await user.keyboard("y");
      const sent = mock.rows.work.sent.find((r) => r.id === 1010);
      expect(writeText).toHaveBeenLastCalledWith(`mp://work/sent/${sent?.message_id.replace(/^<|>$/g, "")}`);
      expect(writeText).toHaveBeenCalledTimes(2);
    } finally {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    }
  });

  it("archives a collapsed row's members in the inbox and nothing in Sent", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("a");
    const dialog = await screen.findByRole("dialog", { name: "Archive this email?" });
    await user.click(within(dialog).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(callsOf("message_archive")).toEqual([{ account: "work", row_ids: [1007] }]));
    await waitFor(() => expect(offsite()).toBeNull());
    expect(mock.rows.work.sent.map((r) => r.id)).toEqual(expect.arrayContaining([1000, 1010]));
  });

  it("marking a collapsed row read clears its unread and its bold", async () => {
    const { user } = renderGrouped();
    await shellReady();
    const angebot = () => header("<angebot-dachsanierung-1002@fixture.example>");
    await waitFor(() => expect(angebot()).not.toBeNull());
    // The fixture holds a send, so `u` would cancel it: the row's toggle marks read.
    await user.click(within(angebot()).getByRole("button", { name: "Unread" }));
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1002], read: true }]));
    await waitFor(() => expect(angebot()).not.toHaveAccessibleName(/unread/));
    expect(within(angebot()).getByRole("button", { name: "Unread" })).toHaveAttribute("aria-pressed", "false");
  });

  it("re-reads on a change in another mailbox, one read in flight and one owed for a burst", async () => {
    renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    const reads = () => callsOf("list_messages").filter((a) => a?.conversations === true).length;
    const before = reads();
    let open = () => {};
    mock.gates.set("list_messages", new Promise<void>((r) => (open = r)));
    act(() => emitEnvelope("state.invalidate", { resource: "mailbox:work/sent", scope: { query: "counts" } }));
    await waitFor(() => expect(reads()).toBe(before + 1));
    for (let i = 0; i < 5; i++) {
      act(() => emitEnvelope("state.invalidate", { resource: "mailbox:work/sent", scope: { query: "counts" } }));
      act(() => emitEnvelope("draft.changed", { account: "work", id: "offsite-note" }));
    }
    expect(reads()).toBe(before + 1);
    await act(async () => open());
    await waitFor(() => expect(reads()).toBe(before + 2));
    await new Promise((r) => setTimeout(r, 50));
    expect(reads()).toBe(before + 2);
  });

  it("keeps Drafts the flat draft listing", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    expect(screen.getByText("2 drafts")).toBeInTheDocument();
  });
});

describe("the conversation view setting", () => {
  const stored = () => JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}") as { flatListAccounts?: string[] };
  const lastRead = () => {
    const reads = callsOf("list_messages").filter((a) => a?.account === "work" && a?.mailbox === "inbox");
    return reads[reads.length - 1];
  };

  it("is on for an account the stored prefs do not name", async () => {
    localStorage.removeItem(PREFS_KEY);
    resetMock();
    setWidth(1400);
    render(<App />);
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    expect(lastRead()).toEqual({ account: "work", mailbox: "inbox", conversations: true });
  });

  it("switches with `tc`, and the choice survives a restart", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.keyboard("tc");
    await waitFor(() => expect(lastRead()).toEqual({ account: "work", mailbox: "inbox", conversations: false }));
    await waitFor(() => expect(screen.queryByText("8 conversations")).toBeNull());
    expect(offsite()).toBeNull();
    await waitFor(() => expect(stored().flatListAccounts).toEqual(["work"]));

    // A restart reads the stored prefs: `work` comes back flat.
    cleanup();
    resetMock();
    render(<App />);
    await shellReady();
    await waitFor(() => expect(lastRead()).toEqual({ account: "work", mailbox: "inbox", conversations: false }));

    await user.keyboard("tc");
    await waitFor(() => expect(offsite()).not.toBeNull());
    await waitFor(() => expect(stored().flatListAccounts).toEqual([]));
  });
});
