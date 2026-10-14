// The conversation list end to end (ticket 0142, step 5): the rows, the
// expansion on Enter, `l` and `h`, the actions on a collapsed row, and the
// re-read on any change of the account, against the fixture mock.

import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import App from "@/App";
import { initialState } from "@/app/state";
import { shellReady } from "@/test/render";
import { setWidth } from "@/test/setup";
import { CANNED_THREAD, emitEnvelope, mock, resetMock } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const header = (threadId: string) => document.querySelector<HTMLElement>(`[role="option"][data-thread-id="${threadId}"]`)!;
const offsite = () => header(CANNED_THREAD);
const members = () => [...document.querySelectorAll<HTMLElement>('[role="option"][data-member]')];
const cursorRow = () => document.querySelector<HTMLElement>('[role="option"][data-cursor="true"]');

/** The app with `work` listed by conversation, as step 6's setting will turn it on. */
function renderGrouped() {
  resetMock();
  setWidth(1400);
  const user = userEvent.setup();
  render(<App initial={{ ...initialState(), conversationMode: { work: true } }} />);
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
    expect(members().map((m) => m.getAttribute("aria-label")?.split(":")[0])).toEqual(["Inbox", "Sent", "Sent", "Drafts"]);
    expect(members().map((m) => m.getAttribute("data-member"))).toEqual(["row:1007", "row:1010", "row:1000", "draft:offsite-note"]);
    expect(callsOf("message_thread")).toEqual([{ account: "work", row_id: 1007, mailbox: "inbox" }]);
    expect(cursorRow()).toBe(offsite());
    expect(offsite()).toHaveAttribute("aria-expanded", "true");

    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.getByRole("complementary", { name: "Reader" })).toHaveAttribute("data-focused", "true"));
    expect(callsOf("message_html_meta")).toContainEqual({ account: "work", row_id: 1007 });
  });

  it("h collapses from a member back to its header, l expands again, and a member in Sent opens in place", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(offsite());
    await user.keyboard("l");
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.keyboard("jj");
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

  it("selecting the draft member previews the draft", async () => {
    const { user } = renderGrouped();
    await shellReady();
    await waitFor(() => expect(offsite()).not.toBeNull());
    await user.click(within(offsite()).getByRole("button", { name: "Expand conversation (l)" }));
    await waitFor(() => expect(members()).toHaveLength(4));
    await user.click(members()[3]);
    await waitFor(() => expect(callsOf("draft_preview")).toContainEqual({ account: "work", id: "offsite-note" }));
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
