// The Server Search form's transitions (PERSO-115): opening it, running it,
// the local pass the server leg follows, and the header's query the store
// cannot answer going to the server.

import { describe, expect, it, vi } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { runAction } from "@/app/actions";
import { initialState, listKey, type AppState } from "@/app/state";
import {
  EMPTY_SEARCH_FORM,
  NO_MAILBOX_TARGET,
  REROUTED_NOTICE,
  advancedActive,
  arrangeAfter,
  dateMerge,
  datePosition,
  submitServerSearch,
  type FormRun,
} from "@/app/search";
import { fixtures, mailboxListing } from "@/test/tauri-mock";
import type { MessageListRow } from "@/protocol/types";
import type { LocalSearchHit, MessageList } from "@/lib/gui-types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

function booted(): AppState {
  let s = run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
  s = run(s, { type: "mailboxes_loaded", account: "work", gen: 1, listing: mailboxListing("work") });
  const rows = fixtures.messages.work.inbox as MessageListRow[];
  const list: MessageList = { kind: "messages", account: "work", mailbox: "inbox", total: rows.length, rows };
  return run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list });
}

const formRun = (over: Partial<FormRun> = {}): FormRun => ({
  account: "work",
  query: "ledger",
  local: true,
  mailbox: null,
  form: { fields: { ...EMPTY_SEARCH_FORM, keywords: "ledger" }, scope: "account" },
  ...over,
});

const localHit = (id: number, message_id: string): LocalSearchHit => {
  const row = (fixtures.messages.work.inbox as MessageListRow[])[0];
  return { ...row, id, message_id, mailbox: "inbox" };
};

describe("opening the Server Search form", () => {
  it("opens on the selected account with the mailbox's label and a blank form scoped to the account", () => {
    const s = run(booted(), { type: "open_server_search" });
    expect(s.overlay).toBe("server_search");
    expect(s.serverSearchDialog).toEqual({
      account: "work",
      mailbox: "Inbox",
      form: { fields: EMPTY_SEARCH_FORM, scope: "account" },
    });
  });

  it("takes the header's typed text as its keywords", () => {
    const s = run(booted(), { type: "filter", text: " to:diyan " }, { type: "open_server_search" });
    expect(s.serverSearchDialog?.form.fields.keywords).toBe("to:diyan");
  });

  it("has no mailbox to scope to on Drafts, which has no server folder", () => {
    let s = booted();
    s = { ...s, selection: { ...s.selection, mailbox: "drafts" } };
    expect(run(s, { type: "open_server_search" }).serverSearchDialog?.mailbox).toBeNull();
  });

  it("opens again with the form the shown search ran, and closes with Escape's overlay change", () => {
    const form = { fields: { ...EMPTY_SEARCH_FORM, to: "diyan" }, scope: "mailbox" as const };
    let s = run(booted(), { type: "search_run", run: formRun({ query: 'to:"diyan"', local: false, mailbox: "Inbox", form }) });
    s = run(s, { type: "open_server_search" });
    expect(s.serverSearchDialog?.form).toEqual(form);
    expect(s.serverSearchDialog?.mailbox).toBe("Inbox");
    s = run(s, { type: "overlay", overlay: null });
    expect(s).toMatchObject({ overlay: null, serverSearchDialog: null });
  });

  it("is what ff and the empty header's server button run", () => {
    const dispatch = vi.fn();
    runAction("open_server_search", booted(), dispatch);
    runAction("search_server", booted(), dispatch);
    expect(dispatch.mock.calls.map(([a]) => a)).toEqual([{ type: "open_server_search" }, { type: "open_server_search" }]);
  });

  it("knows an Advanced line takes over only once it holds something", () => {
    expect(advancedActive({ ...EMPTY_SEARCH_FORM, advanced: "  " })).toBe(false);
    expect(advancedActive({ ...EMPTY_SEARCH_FORM, advanced: "from:x" })).toBe(true);
  });
});

describe("running the Server Search form", () => {
  it("runs the store first, then the server leg over the same scope, keeping and excluding the local hits", () => {
    let s = run(booted(), { type: "search_run", run: formRun({ mailbox: "Inbox" }) });
    expect(s.overlay).toBeNull();
    expect(s.focus).toBe("list");
    expect(s.search).toMatchObject({ mode: "local", status: "searching", chain: true, mailbox: "Inbox", query: "ledger" });
    const seq = s.search!.seq;
    s = run(s, { type: "search_local_loaded", seq, hits: [localHit(1001, "<a@x>")], order: "newest" });
    expect(s.search).toMatchObject({ mode: "server", status: "searching", chain: false, mailbox: "Inbox", seq: seq + 1 });
    expect(s.search?.hits.map((h) => h.message_id)).toEqual(["<a@x>"]);
    expect(s.search?.form?.fields.keywords).toBe("ledger");
  });

  it("goes on to the server when the local pass is refused, with nothing to keep", () => {
    let s = run(booted(), { type: "search_run", run: formRun() });
    s = run(s, { type: "search_local_failed", seq: s.search!.seq, error: { kind: "protocol", message: "no", code: -32602 } });
    expect(s.search).toMatchObject({ mode: "server", status: "searching", hits: [], error: null });
  });

  it("sends a query only the server can answer straight to it", () => {
    const s = run(booted(), { type: "search_run", run: formRun({ query: 'to:"diyan"', local: false }) });
    expect(s.search).toMatchObject({ mode: "server", status: "searching", chain: false, query: 'to:"diyan"' });
  });

  it("a header search after it searches the whole account again, with no form", () => {
    let s = run(booted(), { type: "search_run", run: formRun({ mailbox: "Inbox" }) });
    s = run(s, { type: "search_local", query: "other" });
    expect(s.search).toMatchObject({ mailbox: null, form: null, chain: false });
  });
});

describe("a header query the store cannot answer (PERSO-115)", () => {
  it("goes to the server, and the notice line says why", () => {
    let s = run(booted(), { type: "search_local", query: "to:diyan" });
    const seq = s.search!.seq;
    s = run(s, { type: "search_local_rerouted", seq, mailbox: null });
    expect(s.search).toMatchObject({ mode: "server", status: "searching", query: "to:diyan", seq: seq + 1 });
    expect(s.notice).toBe(REROUTED_NOTICE);
  });

  it("keeps the query's in: mailbox, so the server searches only that one", () => {
    let s = run(booted(), { type: "search_local", query: "to:x in:Archive" });
    const seq = s.search!.seq;
    s = run(s, { type: "search_local_rerouted", seq, mailbox: "Archive" });
    expect(s.search).toMatchObject({ mode: "server", status: "searching", query: "to:x in:Archive", mailbox: "Archive" });
  });

  it("ignores the answer of a run since replaced", () => {
    let s = run(booted(), { type: "search_local", query: "to:diyan" });
    const stale = s.search!.seq;
    s = run(s, { type: "search_local", query: "ledger" });
    expect(run(s, { type: "search_local_rerouted", seq: stale, mailbox: null }).search).toMatchObject({ mode: "local", query: "ledger" });
  });
});

describe("submitting the form", () => {
  const dialog = { account: "work", mailbox: "Inbox" };

  it("says what to do with a blank form and what the parser refused, and runs nothing", async () => {
    const dispatch = vi.fn();
    expect(await submitServerSearch(dispatch, dialog, { fields: EMPTY_SEARCH_FORM, scope: "account" })).toBe(
      "Fill the form and press Enter to search",
    );
    expect(
      await submitServerSearch(dispatch, dialog, { fields: { ...EMPTY_SEARCH_FORM, after: "soon" }, scope: "account" }),
    ).toMatch(/YYYY-MM-DD/);
    expect(dispatch).not.toHaveBeenCalled();
  });

  it("refuses the mailbox scope with no mailbox to search, in the TUI's words", async () => {
    const dispatch = vi.fn();
    const form = { fields: { ...EMPTY_SEARCH_FORM, from: "ada" }, scope: "mailbox" as const };
    expect(await submitServerSearch(dispatch, { account: "work", mailbox: null }, form)).toBe(NO_MAILBOX_TARGET);
    expect(dispatch).not.toHaveBeenCalled();
  });

  it("runs the built query over the scope, and the query's in: wins over it", async () => {
    const dispatch = vi.fn();
    const form = { fields: { ...EMPTY_SEARCH_FORM, to: "diyan" }, scope: "mailbox" as const };
    expect(await submitServerSearch(dispatch, dialog, form)).toBeNull();
    const advanced = { fields: { ...EMPTY_SEARCH_FORM, advanced: "subject:x in:Archive" }, scope: "mailbox" as const };
    expect(await submitServerSearch(dispatch, dialog, advanced)).toBeNull();
    expect(dispatch.mock.calls.map(([a]) => a)).toEqual([
      { type: "search_run", run: { account: "work", query: 'to:"diyan"', local: false, mailbox: "Inbox", form } },
      { type: "search_run", run: { account: "work", query: "subject:x in:Archive", local: true, mailbox: "Archive", form: advanced } },
    ]);
  });
});

// The cases of `ServerSearchSettled::arrange_after` and
// `SearchOrder::date_position` in crates/mp-protocol, one for one.
describe("merging the two passes by date (PERSO-273)", () => {
  type Dated = readonly [string, string];
  const dateOf = (hit: Dated) => hit[1];
  const names = (hits: Dated[]) => hits.map(([name]) => name);

  it("newest first merges the server hits into the local ones by date", () => {
    const local: Dated[] = [
      ["local-may", "2026-05-01T09:00:00"],
      ["local-march", "2026-03-01T09:00:00"],
      ["local-undated", ""],
    ];
    const server = [
      [0, ["server-march", "2026-03-01T09:00:00"]],
      [1, ["server-today", "2026-07-02T08:00:00"]],
      [2, ["server-undated", ""]],
    ] as const;
    expect(names(arrangeAfter({ order: "newest", hitOrder: [1, 0, 2] }, "newest", local, server, dateOf))).toEqual([
      "server-today",
      "local-may",
      "local-march",
      "server-march",
      "local-undated",
      "server-undated",
    ]);
  });

  it("oldest first merges the server hits into the local ones by date", () => {
    const local: Dated[] = [
      ["local-march", "2026-03-01T09:00:00"],
      ["local-may", "2026-05-01T09:00:00"],
    ];
    const server = [
      [0, ["server-january", "2026-01-01T09:00:00"]],
      [1, ["server-today", "2026-07-02T08:00:00"]],
    ] as const;
    expect(names(arrangeAfter({ order: "oldest", hitOrder: [0, 1] }, "oldest", local, server, dateOf))).toEqual([
      "server-january",
      "local-march",
      "local-may",
      "server-today",
    ]);
  });

  it("a server run alone keeps its hit_order", () => {
    const server = [
      [0, ["older", "2026-03-01T09:00:00"]],
      [1, ["newer", "2026-05-01T09:00:00"]],
    ] as const;
    expect(names(arrangeAfter<Dated>({ order: "newest", hitOrder: [1, 0] }, null, [], server, dateOf))).toEqual(["newer", "older"]);
  });

  it("relevance or two different orders keep the local pass first", () => {
    const local: Dated[] = [["local-march", "2026-03-01T09:00:00"]];
    const server = [[0, ["server-today", "2026-07-02T08:00:00"]]] as const;
    for (const echoed of ["relevance", "oldest"] as const) {
      expect(names(arrangeAfter({ order: "newest", hitOrder: [0] }, echoed, local, server, dateOf))).toEqual([
        "local-march",
        "server-today",
      ]);
    }
    // An older daemon's settle echoes no order, which reads as relevance.
    expect(names(arrangeAfter({ order: "relevance", hitOrder: [] }, null, local, server, dateOf))).toEqual([
      "local-march",
      "server-today",
    ]);
    expect(dateMerge("relevance", null)).toBeNull();
    expect(dateMerge("newest", "relevance")).toBeNull();
    expect(dateMerge("oldest", "oldest")).toBe("oldest");
  });

  it("a streamed hit takes its date position", () => {
    const run: Dated[] = [
      ["may", "2026-05-01T09:00:00"],
      ["march", "2026-03-01T09:00:00"],
      ["undated", ""],
    ];
    expect(datePosition("newest", run, "2026-07-02T08:00:00", dateOf)).toBe(0);
    expect(datePosition("newest", run, "2026-03-01T09:00:00", dateOf)).toBe(2);
    expect(datePosition("newest", run, "2026-04-01T09:00:00", dateOf)).toBe(1);
    expect(datePosition("newest", run, "", dateOf)).toBe(3);
    const oldest = [...run].reverse().slice(1);
    expect(datePosition("oldest", oldest, "2026-04-01T09:00:00", dateOf)).toBe(1);
    expect(datePosition<Dated>("newest", [], "2026-04-01T09:00:00", dateOf)).toBe(0);
  });
});

describe("a search's seq", () => {
  it("never goes back after Escape, so the ended search's late answer does not land on the next one", () => {
    const first = run(booted(), { type: "search_local", query: "ledger" });
    expect(first.search?.seq).toBe(1);
    const next = run(first, { type: "exit_search" }, { type: "search_local", query: "from: borrmann" });
    expect(next.search?.seq).toBe(2);
    const late = run(next, { type: "search_local_loaded", seq: 1, hits: [localHit(1001, "<late@fixture.example>")], order: "newest" });
    expect(late.search).toEqual(next.search);
  });

  it("keeps counting across a daemon restart's re-run, so the next search gets a fresh number", () => {
    const first = run(booted(), { type: "search_local", query: "ledger" });
    const rerun = run(first, {
      type: "gui_event",
      event: { type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } },
    });
    expect(rerun.search).toMatchObject({ status: "searching", seq: 2 });
    const next = run(rerun, { type: "search_local", query: "borrmann" });
    expect(next.search?.seq).toBe(3);
  });
});
