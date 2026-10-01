import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, mock } from "@/test/tauri-mock";
import { GUI_ENTRIES } from "@/keymap/catalog";
import type { OutboxRow } from "@/protocol/types";

function row(id: number, patch: Partial<OutboxRow> = {}): OutboxRow {
  return {
    id,
    state: "failed",
    partial: false,
    never_submitted: false,
    message_id: `<row-${id}@example.com>`,
    target_mailbox: "Sent",
    updated: 1_790_000_000,
    last_error: "421 4.7.0 the server closed the connection",
    rejected: [],
    outstanding: ["robin@example.com"],
    ...patch,
  };
}

/** `work` with a failed row, a row owed its Sent copy and a partly delivered one. */
function seedWork() {
  mock.outbox.work = {
    ever_used: true,
    rows: [
      row(5),
      row(6, { state: "sent_pending_append", last_error: null, outstanding: [] }),
      row(7, {
        state: "done",
        partial: true,
        outstanding: [],
        rejected: [["nobody@refused.example", "550 5.1.1 no such mailbox"]],
        last_error: "partly delivered: nobody@refused.example refused",
      }),
    ],
  };
}

const view = (account: string) => screen.findByRole("region", { name: `Outbox of ${account}` });
const rowEl = (id: number) => document.querySelector(`[data-outbox-row="${id}"]`) as HTMLElement;
const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd);

describe("the outbox view", () => {
  it("opens from the sidebar line, which shows what waits, and lists the row with its notes", async () => {
    const { user } = renderApp();
    await shellReady();
    const line = screen.getByRole("button", { name: "Outbox of home: 1 queued, key g o" });
    expect(document.querySelector('[data-outbox="work"]')).toBeNull();
    await user.click(line);
    const region = await view("home");
    expect(within(region).getByRole("heading", { name: "Outbox: home" })).toBeInTheDocument();
    await waitFor(() => expect(within(region).getByRole("status")).toHaveTextContent("1 working, 0 failed, 0 partly delivered"));
    const r = rowEl(1);
    expect(r).toHaveAttribute("aria-label", "Row 1, Queued");
    expect(r).toHaveTextContent("<queued-0@home.fixture.example>");
    expect(r).toHaveTextContent("Never submitted; the next sync sends it");
    expect(r).toHaveTextContent("Still to deliver to friend@example.com");
    expect(within(r).queryByRole("button", { name: /Retry/ })).toBeNull();
    expect(within(r).getByRole("button", { name: "Discard row 1" })).toBeInTheDocument();
    expect(callsOf("outbox_list").map((c) => c.args)).toEqual([{ account: "home" }]);
  });

  it("go opens the selected account's outbox, Escape brings the mailbox back", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("go");
    const region = await view("work");
    expect(await within(region).findByText("Nothing has been queued yet.")).toBeInTheDocument();
    expect(screen.queryByRole("listbox", { name: "Inbox messages" })).toBeNull();
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Outbox of work" })).toBeNull();
  });

  it("shows each state with its chip, and a partial delivery never as a failure", async () => {
    const { user } = renderApp(1400, seedWork);
    await shellReady();
    await user.keyboard("go");
    await view("work");
    await waitFor(() => expect(rowEl(7)).not.toBeNull());
    const chip = (id: number) => rowEl(id).querySelector('[data-slot="outbox-state"]');
    expect(chip(5)).toHaveTextContent("Failed");
    expect(chip(6)).toHaveTextContent("Sent, copy owed");
    expect(chip(7)).toHaveTextContent("Partly delivered");
    expect(rowEl(5)).toHaveTextContent("Last error: 421 4.7.0 the server closed the connection");
    expect(rowEl(5)).toHaveTextContent("Still to deliver to robin@example.com");
    expect(rowEl(6)).toHaveTextContent("Sent copy owed to Sent");
    const partial = rowEl(7);
    expect(partial).toHaveAttribute("data-state", "partial");
    expect(partial).toHaveTextContent("Never delivered to nobody@refused.example (550 5.1.1 no such mailbox)");
    expect(partial).toHaveTextContent("Outcome: partly delivered: nobody@refused.example refused");
    expect(partial.textContent).not.toMatch(/fail|error/i);
    expect(within(rowEl(5)).getByRole("button", { name: "Retry row 5" })).toBeInTheDocument();
    expect(within(rowEl(6)).getByRole("button", { name: "Retry row 6" })).toBeInTheDocument();
    expect(within(partial).queryByRole("button", { name: /Retry/ })).toBeNull();
    // The sidebar line reads the listing, partial count included.
    expect(screen.getByRole("button", { name: "Outbox of work: 1 queued, 1 failed, 1 partly delivered, key g o" })).toBeInTheDocument();
  });

  it("R retries the cursor row after the warning, and the settle says how it ended", async () => {
    const { user } = renderApp(1400, seedWork);
    await shellReady();
    await user.keyboard("go");
    await view("work");
    await waitFor(() => expect(rowEl(5)).toHaveAttribute("data-cursor", "true"));
    await user.keyboard("R");
    const dialog = await screen.findByRole("dialog", { name: "Send row 5 again?" });
    expect(dialog).toHaveTextContent("<row-5@example.com>");
    expect(dialog).toHaveTextContent("may already have been delivered");
    await user.keyboard("y");
    await waitFor(() => expect(callsOf("outbox_retry").map((c) => c.args)).toEqual([{ account: "work", row_id: 5 }]));
    await waitFor(() => expect(rowEl(5)).toHaveAttribute("aria-busy", "true"));
    expect(rowEl(5)).toHaveTextContent("Retrying…");
    expect(document.querySelector('[data-slot="queue-depth"]')).toHaveTextContent("1 sending");
    mock.outbox.work.rows = mock.outbox.work.rows.filter((r) => r.id !== 5);
    act(() => emitEnvelope("state.invalidate", { resource: "outbox:work", scope: { query: "counts" } }));
    act(() => emitEnvelope("operation.finished", { operation_id: "fixture-op-retry-1", state: "succeeded", result: { row_id: 5, state: null, completed: 1 } }));
    const area = screen.getByRole("region", { name: "Activity" });
    await waitFor(() => expect(area).toHaveTextContent("Outbox row 5 sent; its Sent copy is filed"));
    await waitFor(() => expect(rowEl(5)).toBeNull());
  });

  it("R on a row the daemon will not retry names why and asks nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("go");
    await user.click(screen.getByRole("button", { name: /Outbox of home/ }));
    await waitFor(() => expect(rowEl(1)).not.toBeNull());
    await user.keyboard("R");
    expect(await screen.findByText(/row 1 is queued/)).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(callsOf("outbox_retry")).toEqual([]);
  });

  it("d discards the cursor row after the warning, and the sidebar line goes with it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(screen.getByRole("button", { name: /Outbox of home/ }));
    await waitFor(() => expect(rowEl(1)).not.toBeNull());
    await user.keyboard("d");
    const dialog = await screen.findByRole("dialog", { name: "Discard row 1?" });
    expect(dialog.querySelector('[data-slot="confirm-warning"]')).toHaveTextContent("It was never submitted; discarding it means it is never sent.");
    await user.keyboard("y");
    await waitFor(() => expect(callsOf("outbox_discard").map((c) => c.args)).toEqual([{ account: "home", row_id: 1 }]));
    expect(await within(await view("home")).findByText("The outbox is clear.")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("button", { name: /Outbox of home/ })).toBeNull());
    expect(screen.getByRole("region", { name: "Activity" })).toHaveTextContent("Discarded outbox row 1 (<queued-0@home.fixture.example>)");
  });

  it("a refused discard puts the row back and says why", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("outbox_discard", { kind: "not_found", message: "no outbox row 1", code: -32602 });
    await user.click(screen.getByRole("button", { name: /Outbox of home/ }));
    await waitFor(() => expect(rowEl(1)).not.toBeNull());
    await user.click(within(rowEl(1)).getByRole("button", { name: "Discard row 1" }));
    await screen.findByRole("dialog", { name: "Discard row 1?" });
    await user.keyboard("y");
    expect(await screen.findByRole("alert")).toHaveTextContent("The discard of outbox row 1 was refused: no outbox row 1");
    expect(rowEl(1)).not.toBeNull();
  });

  it("an invalidation refreshes the sidebar line without a bootstrap", async () => {
    renderApp();
    await shellReady();
    expect(document.querySelector('[data-outbox="work"]')).toBeNull();
    mock.outbox.work = { ever_used: true, rows: [row(9)] };
    act(() => emitEnvelope("state.invalidate", { resource: "outbox:work", scope: { query: "counts" } }));
    expect(await screen.findByRole("button", { name: "Outbox of work: 1 failed, key g o" })).toBeInTheDocument();
    expect(callsOf("bootstrap")).toEqual([]);
  });

  it("the palette opens the outbox and runs its row actions", async () => {
    const labels = GUI_ENTRIES.filter((e) => e.section === "OUTBOX").map((e) => [e.label, e.keys, e.id]);
    expect(labels).toEqual([
      ["Open outbox", ["go"], "open_outbox"],
      ["Retry outbox row", ["R"], "outbox_retry"],
      ["Discard outbox row", ["d"], "outbox_discard"],
    ]);
    const { user } = renderApp(1400, seedWork);
    await shellReady();
    await user.keyboard(":");
    await user.keyboard("Open outbox");
    await user.keyboard("{Enter}");
    await view("work");
    await waitFor(() => expect(rowEl(5)).not.toBeNull());
    await user.keyboard(":");
    await user.keyboard("Retry outbox row");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: "Send row 5 again?" })).toBeInTheDocument();
  });

  it("j and k move the view's cursor, and Enter opens nothing", async () => {
    const { user } = renderApp(1400, seedWork);
    await shellReady();
    await user.keyboard("go");
    await waitFor(() => expect(rowEl(5)).toHaveAttribute("data-cursor", "true"));
    await user.keyboard("j");
    expect(rowEl(6)).toHaveAttribute("data-cursor", "true");
    expect(document.activeElement).toBe(rowEl(6));
    await user.keyboard("j");
    await user.keyboard("k");
    expect(rowEl(6)).toHaveAttribute("data-cursor", "true");
    await user.keyboard("{Enter}");
    expect(await view("work")).toBeInTheDocument();
    // MESSAGE keys have no row here.
    await user.keyboard("a");
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("the outbox view hides the mailbox selection", () => {
  const acting = [
    "message_archive",
    "message_delete",
    "message_move",
    "message_set_flag",
    "message_set_read",
    "draft_discard",
    "draft_approve",
    "draft_demote",
    "send_draft",
    "send_approved",
    "attachment_open",
    "html_open",
    "message_fetch",
  ];
  const acted = () => mock.calls.filter((c) => acting.includes(c.cmd)).map((c) => c.cmd);
  const marked = () => document.querySelector('[data-slot="marked-count"]')?.textContent ?? "";

  it("drops every key on the hidden selection, from the list and from the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await within(screen.getByRole("complementary", { name: "Reader" })).findByRole("toolbar", { name: "Message actions" });
    await user.keyboard("go");
    const region = await view("work");
    // From the list pane: send, marks, the compose row keys.
    await user.keyboard("x");
    await user.keyboard("{Control>}a{/Control}");
    for (const combo of ["cr", "ca", "cf", "ce", "cA", "cD", "cX"]) await user.keyboard(combo);
    // From the reader, which still shows the message the list had selected.
    await user.keyboard("{Tab}");
    await waitFor(() => expect(document.activeElement?.closest("[data-pane]")).toHaveAttribute("data-pane", "reader"));
    for (const key of ["a", "d", "u", "*", "M", "e", "r", "y", "x", "v"]) await user.keyboard(key);
    for (const combo of ["cr", "cf", "ce", "to", "ts", "tb", "ta", "F"]) await user.keyboard(combo);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(acted()).toEqual([]);
    expect(screen.queryByText(/Close the outbox first/)).toBeNull();
    expect(region).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(marked()).toBe("");
  });

  it("Ctrl+a over the view marks nothing in the hidden mailbox list", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("go");
    await view("work");
    await user.keyboard("{Control>}a{/Control}");
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(marked()).toBe("");
  });

  it("Escape closes the view before it clears the marks the view hid", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jv");
    expect(marked()).toBe("1 marked");
    await user.keyboard("go");
    await view("work");
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(marked()).toBe("1 marked");
    await user.keyboard("{Escape}");
    expect(marked()).toBe("");
  });

  it("/ closes the view and focuses the mailbox list's filter", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("go");
    await view("work");
    await user.keyboard("/");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Outbox of work" })).toBeNull();
    await waitFor(() => expect(document.activeElement).toHaveAttribute("id", "mp-list-filter"));
  });

  it("cn opens the new-draft wizard for the view's account, which may not be the selected one", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(screen.getByRole("button", { name: /Outbox of home/ }));
    await view("home");
    await user.keyboard("cn");
    const wizard = await screen.findByRole("dialog", { name: "New draft" });
    await waitFor(() => expect(within(wizard).getByRole("combobox", { name: "To" })).toHaveFocus());
    // Completion off: the test types a recipient and is not about it.
    mock.failing.set("contact_search", new Error("off"));
    await user.keyboard("kim@example.com");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("draft_create")).toHaveLength(1));
    expect(callsOf("draft_create")[0].args).toMatchObject({ account: "home" });
  });

  it("the palette's actions on the selection say to close the view first, and its outbox rows still run", async () => {
    const { user } = renderApp(1400, seedWork);
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("go");
    await view("work");
    await waitFor(() => expect(rowEl(5)).not.toBeNull());
    for (const label of ["Archive", "Toggle flag/star", "Send all approved drafts", "Select all visible"]) {
      await user.keyboard(":");
      await user.keyboard(label);
      await user.keyboard("{Enter}");
      expect(await screen.findByText("Close the outbox first (Escape): this acts on the mailbox selection")).toBeInTheDocument();
      expect(screen.queryByRole("dialog")).toBeNull();
    }
    expect(acted()).toEqual([]);
    expect(await view("work")).toBeInTheDocument();
    await user.keyboard(":");
    await user.keyboard("Discard outbox row");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: "Discard row 5?" })).toBeInTheDocument();
  });

  it("the palette's Clear selection closes the view and keeps the marks", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jv");
    await user.keyboard("go");
    await view("work");
    await user.keyboard(":");
    await user.keyboard("Clear selection");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(marked()).toBe("1 marked");
  });
});
