import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, fixtures, mock } from "@/test/tauri-mock";
import type { ServerSearchHit } from "@/protocol/types";
import { FILTER_INPUT_ID } from "@/app/actions";

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

  it("ff starts the server search for the typed query", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(input());
    await user.keyboard("handover");
    // Out of the field, then the TUI's "Search all mail" key.
    await user.keyboard("{Escape}");
    await user.keyboard("ff");
    await waitFor(() => expect(mock.calls.some((c) => c.cmd === "search_server_start")).toBe(true));
  });
});
