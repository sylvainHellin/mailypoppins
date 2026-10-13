import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, fixtures, mock } from "@/test/tauri-mock";
import type { ServerSearchHit } from "@/protocol/types";
import { FILTER_INPUT_ID } from "@/app/actions";
import { REROUTED_NOTICE } from "@/app/search";

const input = () => document.getElementById(FILTER_INPUT_ID) as HTMLInputElement;
const results = () => screen.findByRole("listbox", { name: /^Search results for/ });
const status = () => document.querySelector('[data-slot="search-status"]');

function selectedLabel(): string | null {
  return document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("aria-label") ?? null;
}

function serverHit(mailbox: string, rowId: number | null, subject: string): ServerSearchHit {
  return {
    account: "work",
    mailbox,
    message_id: `<hit-${subject.replace(/\W+/g, "-")}@fixture.example>`,
    row_id: rowId,
    selector: rowId === null ? null : `mp://work/${rowId}`,
    from: "Someone <someone@example.com>",
    to: "me@example.com",
    cc: null,
    reply_to: null,
    bcc: null,
    subject,
    date_display: "Mon, 3 Mar 2025 09:00:00 +0100",
    date_sort: "2025-03-03T08:00:00",
    flags: { seen: true, answered: false, forwarded: false, flagged: false },
    has_attachments: false,
    is_invite: false,
    body_text: "",
    html_body: null,
  };
}

let revision = 500;
function event(kind: string, payload: unknown, instance = "fixture-instance-1"): void {
  act(() => emit({ type: "event", event: { instance_id: instance, revision: ++revision, kind, payload } }));
}

async function searchLocal(user: ReturnType<typeof renderApp>["user"], query: string) {
  await user.click(input());
  await user.keyboard(`${query}{Enter}`);
  return results();
}

async function searchServer(user: ReturnType<typeof renderApp>["user"], query: string) {
  await user.click(input());
  await user.keyboard(`${query}{Shift>}{Enter}{/Shift}`);
  await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
  expect(screen.getByText("No results yet.")).toBeInTheDocument();
  return mock.calls.filter((c) => c.cmd === "search_server_start").pop();
}

describe("local search", () => {
  it("runs search_local on Enter and lists the hits with their mailbox", async () => {
    const { user } = renderApp();
    await shellReady();
    const list = await searchLocal(user, "ledger");
    expect(mock.calls.find((c) => c.cmd === "search_local")?.args).toEqual({ params: { account: "work", query: "ledger" } });
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(3));
    const badges = [...list.querySelectorAll('[data-slot="mailbox-badge"]')].map((b) => b.textContent);
    expect(badges).toEqual(["Inbox", "Sent", "Archive"]);
    const options = within(list).getAllByRole("option");
    expect(options.map((o) => o.getAttribute("aria-posinset"))).toEqual(["1", "2", "3"]);
    for (const o of options) expect(o).toHaveAttribute("aria-setsize", "3");
    expect(status()).toHaveAttribute("data-status", "done");
    expect(status()).toHaveTextContent("3 results in the store");
  });

  it("opens a hit from another mailbox in the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    const list = await searchLocal(user, "ledger template");
    await user.click(within(list).getByRole("option", { name: /Ledger template/ }));
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByTitle("Message body: Ledger template")).toHaveAttribute(
      "src",
      "mpmsg://localhost/work/1012",
    );
  });

  it("Escape returns to the mailbox list with the previous selection restored", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jj");
    const before = selectedLabel();
    expect(before).toMatch(/Angebot Dachsanierung/);

    const list = await searchLocal(user, "ledger");
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(3));
    await user.keyboard("j");
    expect(selectedLabel()).toMatch(/Quarterly ledger review/);
    await user.keyboard("j");
    expect(selectedLabel()).toMatch(/Re: Quarterly ledger review/);

    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    expect(selectedLabel()).toBe(before);
    expect(input().value).toBe("");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" })).toBeInTheDocument();
  });

  it("Escape in the field leaves the search too", async () => {
    const { user } = renderApp();
    await shellReady();
    await searchLocal(user, "ledger");
    await user.click(input());
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
  });
});

describe("server search", () => {
  it("streams hits as they arrive and settles on operation.finished", async () => {
    const { user } = renderApp();
    await shellReady();
    const start = await searchServer(user, "handover");
    expect(start?.args).toEqual({ params: { account: "work", query: "handover" } });
    expect(status()).toHaveTextContent("Searching the server… 0 results so far");

    event("message.server_hit", { operation_id: "op-1", hit: serverHit("Inbox", 1005, "Re: BIM model handover") });
    const list = await results();
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(1));
    // Another search's hit is not this one's.
    event("message.server_hit", { operation_id: "op-9", hit: serverHit("Inbox", 1004, "Planned power outage") });
    event("message.server_hit", { operation_id: "op-1", hit: serverHit("Archive", null, "Server-only match") });
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(2));
    expect(status()).toHaveTextContent("2 results so far");

    const serverOnly = within(list).getByRole("option", { name: /Server-only match/ });
    expect(serverOnly).toHaveAccessibleName(/on the server only/);
    expect(serverOnly.querySelector('[data-slot="mailbox-badge"]')).toHaveTextContent("Archive");

    event("operation.finished", {
      operation_id: "op-1",
      state: "succeeded",
      result: { account: "work", query: "handover", hits: 2, deduplicated: 0, unreachable: [] },
    });
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "done"));
    expect(status()).toHaveTextContent("2 results on the server");
    // Nothing about an operation follows its end.
    event("message.server_hit", { operation_id: "op-1", hit: serverHit("Inbox", 1004, "Late hit") });
    expect(within(list).getAllByRole("option")).toHaveLength(2);
  });

  it("the server leg of a local search keeps its hits and excludes them", async () => {
    const { user } = renderApp();
    await shellReady();
    const list = await searchLocal(user, "ledger");
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(3));
    await user.click(within(status() as HTMLElement).getByRole("button", { name: "Search server" }));
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
    const start = mock.calls.find((c) => c.cmd === "search_server_start");
    const ids = fixtures.messages.work;
    expect(start?.args).toEqual({
      params: {
        account: "work",
        query: "ledger",
        exclude_message_ids: [ids.inbox[0].message_id, ids.sent[0].message_id, ids.archive[0].message_id],
      },
    });
    expect(within(await results()).getAllByRole("option")).toHaveLength(3);
  });

  it("cancels through search_server_cancel", async () => {
    const { user } = renderApp();
    await shellReady();
    await searchServer(user, "handover");
    event("message.server_hit", { operation_id: "op-1", hit: serverHit("Inbox", 1005, "Re: BIM model handover") });
    const list = await results();
    await user.click(within(status() as HTMLElement).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "cancelled"));
    expect(mock.calls.filter((c) => c.cmd === "search_server_cancel").map((c) => c.args)).toEqual([
      { operation_id: "op-1" },
    ]);
    expect(within(list).getAllByRole("option")).toHaveLength(1);
  });

  it("cancels a running search the user walks away from", async () => {
    const { user } = renderApp();
    await shellReady();
    await searchServer(user, "handover");
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("listbox", { name: "Inbox messages" })).toBeInTheDocument();
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "search_server_cancel").map((c) => c.args)).toEqual([
        { operation_id: "op-1" },
      ]),
    );
  });

  it("shows a dropped search as an error", async () => {
    const { user } = renderApp();
    await shellReady();
    await searchServer(user, "handover");
    act(() => emit({ type: "operation_dropped", operation_id: "op-1", kind: "server_search", reason: "the daemon restarted" }));
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "dropped"));
    expect(within(status() as HTMLElement).getByRole("alert")).toHaveTextContent(
      "The server search was dropped: the daemon restarted",
    );
    expect(mock.calls.some((c) => c.cmd === "search_server_cancel")).toBe(false);
  });

  it("keeps its hits across a re-bootstrap and settles from operation_settled", async () => {
    const { user } = renderApp();
    await shellReady();
    await searchServer(user, "handover");
    event("message.server_hit", { operation_id: "op-1", hit: serverHit("Inbox", 1005, "Re: BIM model handover") });
    const list = await results();
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(1));

    act(() => emit({ type: "resync", instance_id: "fixture-instance-1", reason: "queue overflow" }));
    act(() => emit({ type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap }));
    expect(within(await results()).getAllByRole("option")).toHaveLength(1);
    expect(status()).toHaveAttribute("data-status", "running");

    act(() =>
      emit({
        type: "operation_settled",
        operation_id: "op-1",
        kind: "server_search",
        status: {
          operation_id: "op-1",
          method: "message.search_server",
          state: "succeeded",
          scope: "durable",
          progress: null,
          result: { account: "work", query: "handover", hits: 1, deduplicated: 0, unreachable: [{ mailbox: "Archive", error: "timeout" }] },
          error: null,
        },
      }),
    );
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "done"));
    expect(status()).toHaveTextContent("1 result on the server, 1 mailbox unreachable");
    expect(within(await results()).getAllByRole("option")).toHaveLength(1);
  });

  it("from: borrmann ends the search with the parser's lines, the TUI's words, and no spinner (PERSO-115)", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(input());
    await user.keyboard("from: borrmann{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "failed"));
    const lines = "invalid search query: from: needs a value\n  from: borrmann\n  ^";
    expect(document.querySelector('[data-slot="search-parse-error"]')?.textContent).toBe(lines);
    expect(within(status() as HTMLElement).getByRole("alert")).toHaveTextContent(/^The search failed:invalid search query: from: needs a value/);
    expect(status()?.querySelector(".animate-spin")).toBeNull();
    const cmds = mock.calls.map((c) => c.cmd).filter((c) => c.startsWith("search_"));
    expect(cmds).toEqual(["search_local", "search_query"]);
    expect(mock.calls.find((c) => c.cmd === "search_query")?.args).toMatchObject({ form: { keywords: "from: borrmann" } });
  });

  it("a search after Escape runs, where it used to spin with the seq the ended one had (PERSO-115)", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(input());
    await user.keyboard("ledger{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "done"));
    await user.keyboard("{Escape}");
    await waitFor(() => expect(status()).toBeNull());
    await user.click(input());
    await user.keyboard("from: borrmann{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "failed"));
    await user.keyboard("{Escape}");
    await user.click(input());
    await user.keyboard("ledger{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "done"));
    expect(mock.calls.filter((c) => c.cmd === "search_local")).toHaveLength(3);
  });

  it("a to: query typed in the field goes to the server, which can answer it (PERSO-115)", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(input());
    await user.keyboard("to:diyan{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
    expect(mock.calls.find((c) => c.cmd === "search_server_start")?.args).toEqual({
      params: { account: "work", query: "to:diyan" },
    });
    expect(await screen.findByText(REROUTED_NOTICE)).toBeInTheDocument();
  });

  it("a rerouted query keeps its in: mailbox, so the server searches only that one", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(input());
    await user.keyboard("to:x in:Archive{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
    expect(mock.calls.find((c) => c.cmd === "search_server_start")?.args).toEqual({
      params: { account: "work", query: "to:x in:Archive", mailboxes: ["Archive"] },
    });
  });

  it("a refusal reads whole: the status wraps instead of cutting the daemon's sentence", async () => {
    const { user } = renderApp();
    await shellReady();
    const message = "message.search: the daemon refused the call: the store of work is being rebuilt, try again in a minute (-32603)";
    mock.failing.set("search_local", { kind: "protocol", message, code: -32603 });
    await user.click(input());
    await user.keyboard("ledger{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "failed"));
    const alert = within(status() as HTMLElement).getByRole("alert");
    expect(alert).toHaveTextContent(`The search failed: ${message}`);
    expect(alert).not.toHaveClass("truncate");
    expect(alert).toHaveClass("whitespace-normal");
  });
});

describe("the Server Search form (PERSO-115)", () => {
  // The dialog moves the focus to From once it opens, as the TUI's form starts there.
  async function form(): Promise<HTMLElement> {
    const dialog = await screen.findByRole("dialog", { name: "Server Search" });
    await waitFor(() => expect(within(dialog).getByRole("textbox", { name: "From" })).toHaveFocus());
    return dialog;
  }
  const field = (dialog: HTMLElement, name: string) => within(dialog).getByRole("textbox", { name }) as HTMLInputElement;

  it("ff opens it with From focused, the TUI's fields, and the typed text as its keywords", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(input());
    await user.keyboard("handover");
    await user.keyboard("{Escape}");
    await user.keyboard("ff");
    const dialog = await form();
    expect(field(dialog, "From")).toHaveFocus();
    for (const name of ["To", "Subject", "Keywords", "After", "Before", "Advanced"]) {
      expect(field(dialog, name)).toBeInTheDocument();
    }
    expect(field(dialog, "Keywords")).toHaveValue("handover");
    expect(within(dialog).getByRole("button", { name: "Search in: Current Account" })).toBeInTheDocument();
    expect(within(dialog).getByRole("checkbox", { name: "has attachment" })).not.toBeChecked();
    expect(mock.calls.some((c) => c.cmd === "search_server_start")).toBe(false);
  });

  it("Tab moves through the fields, Enter runs a to: search on the server alone, and the results fill the list", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ff");
    const dialog = await form();
    await user.keyboard("{Tab}diyan{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Server Search" })).toBeNull());
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
    expect(mock.calls.find((c) => c.cmd === "search_query")?.args).toMatchObject({ form: { to: "diyan" } });
    expect(mock.calls.find((c) => c.cmd === "search_server_start")?.args).toEqual({
      params: { account: "work", query: 'to:"diyan"' },
    });
    // The store's index cannot answer `to:`, so no local pass ran.
    expect(mock.calls.some((c) => c.cmd === "search_local")).toBe(false);
    expect(dialog).not.toBeInTheDocument();
  });

  it("Space flips the scope to the current mailbox: the store answers first, then the server leg excludes its hits", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ff");
    const dialog = await form();
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(within(dialog).getByRole("button", { name: "Search in: Current Account" })).toHaveFocus();
    await user.keyboard(" ");
    expect(within(dialog).getByRole("button", { name: "Search in: Current Mailbox" })).toBeInTheDocument();
    await user.click(field(dialog, "Keywords"));
    await user.keyboard("ledger{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
    expect(mock.calls.find((c) => c.cmd === "search_local")?.args).toEqual({
      params: { account: "work", query: "ledger", mailbox: "Inbox" },
    });
    expect(mock.calls.find((c) => c.cmd === "search_server_start")?.args).toEqual({
      params: {
        account: "work",
        query: "ledger",
        mailboxes: ["Inbox"],
        exclude_message_ids: [fixtures.messages.work.inbox[0].message_id],
      },
    });
    expect(within(await results()).getAllByRole("option")).toHaveLength(1);
    expect(status()).toHaveTextContent("Searching the server… 1 result so far");
  });

  it("a non-blank Advanced line greys the structured fields, and a refusal keeps the form open", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ff");
    const dialog = await form();
    await user.keyboard("ada");
    await user.click(field(dialog, "Advanced"));
    await user.keyboard("subject:x");
    for (const name of ["From", "To", "Subject", "Keywords", "After", "Before"]) expect(field(dialog, name)).toBeDisabled();
    expect(within(dialog).getByRole("checkbox", { name: "has attachment" })).toBeDisabled();
    await user.clear(field(dialog, "Advanced"));
    expect(field(dialog, "From")).toBeEnabled();

    await user.click(field(dialog, "Before"));
    await user.keyboard("nope{Enter}");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("--before expects a date as YYYY-MM-DD, got 'nope'");
    expect(screen.getByRole("dialog", { name: "Server Search" })).toBeInTheDocument();
    expect(mock.calls.some((c) => c.cmd === "search_local" || c.cmd === "search_server_start")).toBe(false);
  });

  it("a blank form searches nothing, and Escape closes it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ff");
    const dialog = await form();
    await user.keyboard("{Enter}");
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Fill the form and press Enter to search");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Server Search" })).toBeNull());
    expect(mock.calls.some((c) => c.cmd === "search_local" || c.cmd === "search_server_start")).toBe(false);
  });

  it("opens again with the form the shown search ran", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ff");
    let dialog = await form();
    await user.keyboard("{Tab}diyan{Enter}");
    await waitFor(() => expect(status()).toHaveAttribute("data-status", "running"));
    await user.click(screen.getByRole("button", { name: "Server Search form" }));
    dialog = await form();
    expect(field(dialog, "To")).toHaveValue("diyan");
  });
});
