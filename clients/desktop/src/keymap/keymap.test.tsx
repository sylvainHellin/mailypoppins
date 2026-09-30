import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, emitMenu, fixtures, mock } from "@/test/tauri-mock";

function selectedSubject(): string | null {
  const row = document.querySelector('[role="option"][aria-selected="true"]');
  return row?.getAttribute("aria-label") ?? null;
}

describe("keyboard routing", () => {
  it("j and k move the selection in the list and the reader follows", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    expect(selectedSubject()).toMatch(/Quarterly ledger review/);
    await user.keyboard("j");
    expect(selectedSubject()).toMatch(/Angebot Dachsanierung/);
    await user.keyboard("k");
    expect(selectedSubject()).toMatch(/Quarterly ledger review/);
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByRole("heading", { name: "Quarterly ledger review" })).toBeInTheDocument();
    expect(within(reader).getByTitle("Message body: Quarterly ledger review")).toHaveAttribute(
      "src",
      "mpmsg://localhost/work/1001",
    );
  });

  it("Tab and Shift+Tab cycle focus through sidebar, list and reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const pane = () => document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane");
    expect(pane()).toBe("list");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("reader");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("sidebar");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("list");
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(pane()).toBe("sidebar");
  });

  it("the sidebar cursor moves with j and Enter opens the mailbox", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("gm");
    await user.keyboard("jj");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "Sent" })).toBeInTheDocument();
    expect(await screen.findByRole("listbox", { name: "Sent messages" })).toBeInTheDocument();
  });

  it("digits jump to a mailbox of the selected account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("4");
    expect(await screen.findByRole("listbox", { name: "Archive messages" })).toBeInTheDocument();
  });

  it(": and Ctrl+p open the palette, ? the key help", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("{Control>}p{/Control}");
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("?");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("ignores keys while a text field has focus, except Escape", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("/");
    const input = screen.getByRole("searchbox", { name: /Filter this list/ });
    expect(input).toHaveFocus();
    await user.keyboard("j:");
    expect(selectedSubject()).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(input).toHaveValue("j:");
    await user.keyboard("{Escape}");
    expect(input).not.toHaveFocus();
  });

  it("z zooms the focused list pane, hiding the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("z");
    expect(screen.queryByRole("complementary", { name: "Reader" })).toBeNull();
    await user.keyboard("z");
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
  });

  it("a native menu item runs the same action as its key", async () => {
    renderApp();
    await shellReady();
    emitMenu("key_help");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("a key for a later milestone says so instead of doing nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("cn");
    expect(await screen.findByText(/New draft arrives with compose \(M3\)/)).toBeInTheDocument();
  });
});

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const marked = () =>
  [...document.querySelectorAll('[role="option"][aria-selected="true"]')].map((e) => Number(e.getAttribute("data-row-id")));

/** The seeded hold fires, so `u` is toggle read again. */
function fireSeededHold() {
  act(() => emitEnvelope("send.hold_fired", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }));
}

describe("mutation keys (the TUI's)", () => {
  it("a asks first, and y archives the cursor row", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("a");
    const dialog = await screen.findByRole("dialog", { name: "Archive this email?" });
    expect(dialog).toHaveTextContent("Quarterly ledger review");
    expect(callsOf("message_archive")).toEqual([]);
    await user.keyboard("y");
    await waitFor(() => expect(callsOf("message_archive")).toEqual([{ account: "work", row_ids: [1001] }]));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("n cancels the confirmation and nothing is called", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("d");
    expect(await screen.findByRole("dialog", { name: "Delete this email?" })).toBeInTheDocument();
    await user.keyboard("n");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(callsOf("message_delete")).toEqual([]);
  });

  it("v marks and steps, and d with Enter deletes the marked rows in list order", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("vv");
    expect(marked()).toEqual([1001, 1002]);
    await user.keyboard("d");
    expect(await screen.findByRole("dialog", { name: "Delete 2 emails?" })).toBeInTheDocument();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(callsOf("message_delete")).toEqual([{ account: "work", row_ids: [1001, 1002] }]));
  });

  it("* flags the cursor row and unflags a flagged one", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j*");
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1001], flagged: true }]));
    await user.keyboard("jjj*");
    await waitFor(() => expect(callsOf("message_set_flag")[1]).toEqual({ account: "work", row_ids: [1004], flagged: false }));
  });

  it("over marks, * flags them all when any is unflagged, and the marks clear", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jjjvj");
    // 1003 is unflagged, 1004 flagged: flagging wins.
    await user.keyboard("{ArrowUp}{ArrowUp}");
    expect(marked()).toEqual([1003]);
    await user.keyboard("jvk");
    expect(marked()).toEqual([1003, 1004]);
    await user.keyboard("*");
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1003, 1004], flagged: true }]));
    expect(screen.queryByText(/marked/)).toBeNull();
  });

  it("u cancels a held send while one counts down, and toggles read otherwise", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("u");
    await waitFor(() => expect(callsOf("send_cancel_hold")).toEqual([{ operation_id: "fixture-hold-seed" }]));
    expect(callsOf("message_set_read")).toEqual([]);
    fireSeededHold();
    await user.keyboard("u");
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1001], read: false }]));
  });

  it("M opens the mailbox picker, typing filters it and Enter moves", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    await user.keyboard("M");
    const picker = await screen.findByRole("dialog", { name: "Move the message to" });
    await user.keyboard("arch");
    await waitFor(() =>
      expect([...picker.querySelectorAll("[data-testid='move-destination']")].map((e) => e.getAttribute("data-slug"))).toEqual(["archive"]),
    );
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(callsOf("message_move")).toEqual([{ account: "work", row_ids: [1002], destination: "archive" }]),
    );
  });

  it("Ctrl+a marks every row, and Escape clears the marks before the selection", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("{Control>}a{/Control}");
    expect(marked()).toHaveLength(8);
    await user.keyboard("{Escape}");
    expect(marked()).toEqual([1001]);
    await user.keyboard("{Escape}");
    expect(marked()).toEqual([]);
  });

  it("d on a draft discards it after the confirmation", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard("d");
    expect(await screen.findByRole("dialog", { name: "Delete this email?" })).toBeInTheDocument();
    await user.keyboard("y");
    await waitFor(() => expect(callsOf("draft_discard")).toEqual([{ account: "work", ids: ["angebot-antwort"] }]));
    expect(callsOf("message_delete")).toEqual([]);
  });

  it("ss and sS start a quick and a full sync of the selected account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ss");
    await user.keyboard("sS");
    await waitFor(() =>
      expect(callsOf("sync_trigger")).toEqual([
        { account: "work", mode: "quick" },
        { account: "work", mode: "full" },
      ]),
    );
  });

  it("the message keys do nothing from the sidebar, as the TUI's", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("gm");
    await user.keyboard("*");
    expect(callsOf("message_set_flag")).toEqual([]);
  });
});
