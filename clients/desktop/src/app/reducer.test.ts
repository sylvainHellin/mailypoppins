import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { initialState, isStale, listKey, type AppState } from "@/app/state";
import { openableHits } from "@/app/search";
import { fixtures, mailboxListing } from "@/test/tauri-mock";
import type { Bootstrap, MessageListRow } from "@/protocol/types";
import type { AccountInfo, MessageList } from "@/lib/gui-types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

function inbox(account: string, mailbox: string, idShift = 0): MessageList {
  const rows = (fixtures.messages[account][mailbox] as MessageListRow[]).map((r) => ({ ...r, id: r.id + idShift }));
  return { kind: "messages", account, mailbox, total: rows.length, rows };
}

function booted(): AppState {
  let s = run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
  s = run(s, { type: "mailboxes_loaded", account: "work", gen: 1, listing: mailboxListing("work") });
  s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list: inbox("work", "inbox") });
  return s;
}

function withoutAccount(b: Bootstrap, name: string): Bootstrap {
  const mailboxes = { ...b.snapshot.mailboxes };
  delete mailboxes[name];
  return {
    ...b,
    instance_id: "fixture-instance-2",
    snapshot: { ...b.snapshot, accounts: b.snapshot.accounts.filter((a) => a.name !== name), mailboxes },
  };
}

describe("the reducer", () => {
  it("selects the first account's inbox on the first bootstrap", () => {
    const s = booted();
    expect(s.selection).toMatchObject({ account: "work", mailbox: "inbox", message: null });
    expect(s.messages.data?.kind).toBe("messages");
  });

  it("restores the selection by message_id after a re-bootstrap, even when row_id moved", () => {
    let s = booted();
    const row = (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows[2];
    s = run(s, { type: "select_message", message: { row_id: row.id, message_id: row.message_id, selector: row.selector } });
    s = run(s, {
      type: "gui_event",
      event: { type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } },
    });
    expect(s.selection.account).toBe("work");
    expect(s.selection.mailbox).toBe("inbox");
    expect(s.selection.message?.verified).toBe(false);
    expect(isStale(s.messages)).toBe(true);

    // The daemon restarted: the same message now has another row_id.
    s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list: inbox("work", "inbox", 500) });
    expect(s.selection.message).toMatchObject({ message_id: row.message_id, row_id: row.id + 500, verified: true });
  });

  it("clears a message the reloaded list no longer has", () => {
    let s = booted();
    const row = (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows[0];
    s = run(s, { type: "select_message", message: { row_id: row.id, message_id: row.message_id, selector: row.selector } });
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
    const gone = inbox("work", "inbox");
    if (gone.kind !== "messages") throw new Error("unreachable");
    gone.rows = gone.rows.filter((r) => r.message_id !== row.message_id);
    s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list: gone });
    expect(s.selection.message).toBeNull();
    expect(s.reader.meta).toBeNull();
  });

  it("drops a removed account and its mailboxes, falling back to the remaining one", () => {
    let s = booted();
    s = run(s, { type: "mailboxes_loaded", account: "home", gen: 1, listing: mailboxListing("home") });
    s = run(s, { type: "select_mailbox", account: "home", slug: "newsletters" });
    expect(s.selection).toMatchObject({ account: "home", mailbox: "newsletters" });
    s = run(s, {
      type: "gui_event",
      event: { type: "rebootstrapped", cause: "instance_changed", bootstrap: withoutAccount(fixtures.bootstrap, "home") },
    });
    expect(s.selection).toMatchObject({ account: "work", mailbox: "inbox", message: null });
    expect(s.mailboxes.home).toBeUndefined();
    expect(s.sidebarCursor).toEqual({ account: "work", slug: "inbox" });
  });

  it("falls back to the inbox when the selected mailbox is gone from the snapshot", () => {
    let s = booted();
    s = run(s, { type: "select_mailbox", account: "work", slug: "archive" });
    const b: Bootstrap = {
      ...fixtures.bootstrap,
      snapshot: {
        ...fixtures.bootstrap.snapshot,
        mailboxes: { ...fixtures.bootstrap.snapshot.mailboxes, work: fixtures.bootstrap.snapshot.mailboxes.work.filter((m) => m.slug !== "archive") },
      },
    };
    s = run(s, { type: "mailboxes_loaded", account: "work", gen: s.mailboxes.work.gen, listing: { ...mailboxListing("work"), mailboxes: mailboxListing("work").mailboxes.filter((m) => m.slug !== "archive") } });
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: b } });
    expect(s.selection).toMatchObject({ account: "work", mailbox: "inbox" });
  });

  it("marks the list and the counts stale on an invalidation of the selected mailbox", () => {
    let s = booted();
    expect(isStale(s.messages)).toBe(false);
    s = run(s, {
      type: "gui_event",
      event: {
        type: "event",
        event: { instance_id: "fixture-instance-1", revision: 101, kind: "state.invalidate", payload: { resource: "mailbox:work/inbox", scope: { query: "counts" } } },
      },
    });
    expect(isStale(s.messages)).toBe(true);
    expect(isStale(s.mailboxes.work)).toBe(true);
  });

  it("keeps an answer stale when an event lands while it was in flight", () => {
    let s = booted();
    const gen = s.messages.gen;
    s = run(s, { type: "gui_event", event: { type: "event", event: { instance_id: "fixture-instance-1", revision: 101, kind: "state.invalidate", payload: { resource: "mailbox:work/inbox", scope: {} } } } });
    const inflight = s.messages.gen;
    s = run(s, { type: "gui_event", event: { type: "event", event: { instance_id: "fixture-instance-1", revision: 102, kind: "state.invalidate", payload: { resource: "mailbox:work/inbox", scope: {} } } } });
    expect(s.messages.gen).toBe(inflight); // already stale: one refetch covers both
    s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: inflight, list: inbox("work", "inbox") });
    expect(isStale(s.messages)).toBe(false);
    expect(gen).toBeLessThan(inflight);
  });

  it("ignores an event from another daemon instance", () => {
    const s = booted();
    const next = run(s, { type: "gui_event", event: { type: "event", event: { instance_id: "someone-else", revision: 900, kind: "state.invalidate", payload: { resource: "mailbox:work/inbox", scope: {} } } } });
    expect(next).toBe(s);
  });

  describe("the default account from list_accounts", () => {
    const accounts = (def: string): AccountInfo[] =>
      ["work", "home"].map((name) => ({
        name,
        default: name === def,
        backend: "imap" as const,
        store_state: "ready",
        runtime_state: "ready" as const,
        sync_health: "ok" as const,
        outbox: { queued: 0, failed: 0 },
      }));

    it("moves the bootstrap's pick to the default account", () => {
      const reversed = { ...fixtures.bootstrap, snapshot: { ...fixtures.bootstrap.snapshot, accounts: [...fixtures.bootstrap.snapshot.accounts].reverse() } };
      let s = run(initialState(), { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: reversed } });
      expect(s.selection.account).toBe("home");
      expect(s.selectionAuto).toBe(true);
      s = run(s, { type: "accounts_loaded", gen: s.accounts.gen, accounts: accounts("work") });
      expect(s.selection).toMatchObject({ account: "work", mailbox: "inbox" });
    });

    it("keeps a mailbox the user chose before list_accounts answered", () => {
      let s = booted();
      expect(s.selectionAuto).toBe(true);
      s = run(s, { type: "select_mailbox", account: "home", slug: "newsletters" });
      expect(s.selectionAuto).toBe(false);
      s = run(s, { type: "accounts_loaded", gen: s.accounts.gen, accounts: accounts("work") });
      expect(s.selection).toMatchObject({ account: "home", mailbox: "newsletters" });
    });

    it("keeps a choice made between a failed list_accounts and its retry", () => {
      let s = booted();
      s = run(s, { type: "accounts_failed", gen: s.accounts.gen, error: { kind: "timeout", message: "slow" } });
      s = run(s, { type: "next_account" });
      expect(s.selection.account).toBe("home");
      s = run(s, { type: "accounts_loaded", gen: s.accounts.gen, accounts: accounts("work") });
      expect(s.selection).toMatchObject({ account: "home", mailbox: "inbox" });
    });

    it("stays with the user's account across a later re-bootstrap", () => {
      let s = booted();
      s = run(s, { type: "select_account", account: "home" });
      s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
      expect(s.selectionAuto).toBe(false);
      s = run(s, { type: "accounts_loaded", gen: s.accounts.gen, accounts: accounts("work") });
      expect(s.selection.account).toBe("home");
    });
  });

  it("tracks sync health and account state from events", () => {
    let s = booted();
    s = run(s, {
      type: "accounts_loaded",
      gen: s.accounts.gen,
      accounts: [{ name: "work", default: true, backend: "imap", store_state: "ready", runtime_state: "ready", sync_health: "ok", outbox: { queued: 0, failed: 0 } }],
    });
    s = run(s, { type: "gui_event", event: { type: "event", event: { instance_id: "fixture-instance-1", revision: 101, kind: "sync.completed", payload: { account: "work", severity: "error", error: "login refused", saved: 0, new_inbox_mail: [] } } } });
    expect(s.accounts.data?.[0].sync_health).toBe("failed");
    s = run(s, { type: "gui_event", event: { type: "event", event: { instance_id: "fixture-instance-1", revision: 102, kind: "account.state_changed", payload: { account: "work", state: "blocked", reason: "x" } } } });
    expect(s.accounts.data?.[0].runtime_state).toBe("blocked");
  });

  it("drives the banners from disconnected, reconnected, resync and rebootstrapped", () => {
    let s = booted();
    s = run(s, { type: "gui_event", event: { type: "disconnected", reason: "socket closed" } });
    expect(s.disconnected).toBe("socket closed");
    s = run(s, { type: "gui_event", event: { type: "reconnected", instance_id: "fixture-instance-1" } });
    expect(s.disconnected).toBeNull();
    expect(s.resync).toBe("reconnected");
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "reconnected", bootstrap: fixtures.bootstrap } });
    expect(s.resync).toBeNull();
    s = run(s, { type: "gui_event", event: { type: "resync", instance_id: "fixture-instance-1", reason: "event_queue_overflow" } });
    expect(s.resync).toBe("event_queue_overflow");
  });

  it("cycles focus through sidebar, list and reader and walks back through history", () => {
    let s = booted();
    expect(s.focus).toBe("list");
    s = run(s, { type: "cycle_focus", dir: 1 });
    expect(s.focus).toBe("reader");
    s = run(s, { type: "cycle_focus", dir: 1 });
    expect(s.focus).toBe("sidebar");
    s = run(s, { type: "cycle_focus", dir: -1 });
    expect(s.focus).toBe("reader");
    s = run(s, { type: "back" });
    expect(s.focus).toBe("sidebar");
  });

  it("clamps the list width preference", () => {
    const s = run(booted(), { type: "set_list_width", px: 5000 });
    expect(s.prefs.listWidth).toBe(720);
  });
});

describe("the search reducer", () => {
  const hitPayload = (operation_id: string, message_id: string | null) => ({
    operation_id,
    hit: {
      account: "work", mailbox: "Inbox", message_id, row_id: null, selector: null, from: "a@example.com", to: "",
      cc: null, reply_to: null, bcc: null, subject: message_id, date_display: "", date_sort: "",
      flags: { seen: true, answered: false, forwarded: false, flagged: false }, has_attachments: false,
      is_invite: false, body_text: "", html_body: null,
    },
  });
  const envelope = (kind: string, payload: unknown, revision: number): Action => ({
    type: "gui_event",
    event: { type: "event", event: { instance_id: "fixture-instance-1", revision, kind, payload } },
  });

  it("holds hits that overtake the start answer and replays those of its operation", () => {
    let s = run(booted(), { type: "search_server", query: "x" });
    const seq = s.search!.seq;
    s = run(s, envelope("message.server_hit", hitPayload("op-7", "<a>"), 900), envelope("message.server_hit", hitPayload("op-7", "<b>"), 901));
    expect(s.search?.hits).toHaveLength(0);
    expect(s.search?.early).toHaveLength(2);
    s = run(s, { type: "search_server_started", seq, operation_id: "op-7" });
    expect(s.search).toMatchObject({ status: "running", operationId: "op-7", early: [] });
    expect(s.search?.hits.map((h) => h.message_id)).toEqual(["<a>", "<b>"]);
  });

  it("keeps every server hit without a Message-ID, each under its own key, and none of them openable", () => {
    let s = run(booted(), { type: "search_server", query: "x" });
    s = run(s, { type: "search_server_started", seq: s.search!.seq, operation_id: "op-9" });
    s = run(
      s,
      envelope("message.server_hit", hitPayload("op-9", null), 910),
      envelope("message.server_hit", hitPayload("op-9", null), 911),
      envelope("message.server_hit", hitPayload("op-9", "<c>"), 912),
      envelope("message.server_hit", hitPayload("op-9", "<c>"), 913),
    );
    const hits = s.search!.hits;
    expect(hits.map((h) => h.message_id)).toEqual([null, null, "<c>"]);
    expect(new Set(hits.map((h) => h.key)).size).toBe(3);
    expect(openableHits(s.search!)).toEqual([]);
  });

  it("drops a start answer for a superseded run", () => {
    let s = run(booted(), { type: "search_server", query: "x" });
    const stale = s.search!.seq;
    s = run(s, { type: "search_local", query: "y" });
    s = run(s, { type: "search_server_started", seq: stale, operation_id: "op-1" });
    expect(s.search).toMatchObject({ mode: "local", operationId: null, status: "searching" });
  });

  it("runs a local search again after a daemon restart, and restores the list selection by message_id", () => {
    let s = booted();
    const row = (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows[1];
    s = run(s, { type: "select_message", message: { row_id: row.id, message_id: row.message_id, selector: row.selector } });
    s = run(s, { type: "search_local", query: "ledger" });
    const seq = s.search!.seq;
    s = run(s, { type: "search_local_loaded", seq, hits: [] });
    s = run(s, {
      type: "gui_event",
      event: { type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } },
    });
    expect(s.search).toMatchObject({ status: "searching", seq: seq + 1 });
    s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list: inbox("work", "inbox", 500) });
    expect(s.search?.restore.selection.message).toMatchObject({ row_id: row.id + 500, verified: true });
    s = run(s, { type: "exit_search" });
    expect(s.search).toBeNull();
    expect(s.selection.message).toMatchObject({ message_id: row.message_id, row_id: row.id + 500 });
  });

  it("leaves the search when another mailbox is chosen", () => {
    let s = run(booted(), { type: "search_local", query: "ledger" });
    s = run(s, { type: "jump_mailbox", index: 2 });
    expect(s.search).toBeNull();
    expect(s.selection.mailbox).toBe("sent");
  });
});
