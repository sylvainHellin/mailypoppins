import { describe, expect, it } from "vitest";
import { actionTargets, progressAction, reducer, type Action } from "@/app/reducer";
import { ACTIVITY_CAP } from "@/app/pending";
import { initialState, isStale, listKey, type AppState } from "@/app/state";
import { CLOSE_OUTBOX_FIRST } from "@/app/outbox";
import { BACK_TO_MAIL_FIRST, hiddenByView, hiddenNotice } from "@/app/views";
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
    // Already stale, and the fetch asked at `inflight` may have read before this change.
    expect(s.messages.gen).toBe(inflight + 1);
    s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: inflight, list: inbox("work", "inbox") });
    expect(isStale(s.messages)).toBe(true);
    s = run(s, { type: "messages_loaded", key: listKey("work", "inbox"), gen: s.messages.gen, list: inbox("work", "inbox") });
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

describe("mutations and pending state", () => {
  const key = listKey("work", "inbox");
  const row = (id: number) => ({ account: "work", row_id: id });
  const rows = (s: AppState) => (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows.map((r) => r.id);
  const box = (s: AppState, slug: string) => s.mailboxes.work.data!.mailboxes.find((m) => m.slug === slug)!;
  const pick = (s: AppState, id: number) => {
    const r = (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows.find((x) => x.id === id)!;
    return run(s, { type: "select_message", message: { row_id: r.id, message_id: r.message_id, selector: r.selector } });
  };
  const envelope = (kind: string, payload: unknown, revision = 500): Action => ({
    type: "gui_event",
    event: { type: "event", event: { instance_id: "fixture-instance-1", revision, kind, payload } },
  });
  const last = (s: AppState) => s.activity[s.activity.length - 1];
  const archive = (batch: number, ...ids: number[]): Action => ({ type: "mutation_apply", batch, kind: "archive", targets: ids.map(row) });

  it("archives at once, moves the counts and the cursor, and keeps the row gone once confirmed", () => {
    let s = pick(booted(), 1002);
    s = run(s, archive(1, 1002));
    expect(rows(s)).toEqual([1001, 1003, 1004, 1005, 1006, 1007, 1008]);
    expect(s.pending["work#1002"]).toMatchObject({ source: "inbox", leave: { batch: 1, kind: "archive", destination: "archive" } });
    expect(box(s, "inbox")).toMatchObject({ total: 7, unread: 3 });
    expect(box(s, "archive")).toMatchObject({ total: 4, unread: 1 });
    expect(s.selection.message?.row_id).toBe(1003);

    s = run(s, { type: "mutation_settled", batch: 1, kind: "archive", account: "work", done: [row(1002)], failed: [], moved_to: { mailbox: "archive", selector: "mp://work/archive/x" } });
    expect(s.pending).toEqual({});
    expect(rows(s)).not.toContain(1002);
    expect(box(s, "inbox").total).toBe(7);
    expect(s.activity.map((n) => [n.kind, n.text])).toEqual([["applied", "Archived 1 message"]]);
  });

  it("puts back each refused row of a batch where it was, with the daemon's reason", () => {
    let s = run(booted(), archive(1, 1002, 1003));
    s = run(s, {
      type: "mutation_settled",
      batch: 1,
      kind: "archive",
      account: "work",
      done: [row(1002)],
      failed: [{ target: row(1003), reason: "work holds no message with row id 1003" }],
    });
    expect(rows(s)).toEqual([1001, 1003, 1004, 1005, 1006, 1007, 1008]);
    expect(s.pending).toEqual({});
    expect(box(s, "inbox")).toMatchObject({ total: 7, unread: 3 });
    expect(isStale(s.mailboxes.work)).toBe(true);
    const failed = s.activity.find((n) => n.kind === "failed");
    expect(failed?.rows).toEqual([{ key: "work#1003", label: "This week in type: variable fonts", reason: "work holds no message with row id 1003" }]);
  });

  it("puts back every row of a batch whose command threw", () => {
    let s = run(booted(), archive(1, 1001, 1008));
    s = run(s, { type: "mutation_failed", batch: 1, kind: "archive", account: "work", targets: [row(1001), row(1008)], error: { kind: "timeout", message: "slow" } });
    expect(rows(s)).toEqual([1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008]);
    expect(box(s, "inbox")).toMatchObject({ total: 8, unread: 4 });
    expect(last(s)?.rows.map((r) => r.reason)).toEqual(["slow", "slow"]);
  });

  it("flags in place, and a refusal puts the old flag back", () => {
    let s = run(booted(), { type: "mutation_apply", batch: 3, kind: "flag", targets: [row(1001)], value: true });
    const flagged = (x: AppState) => (x.messages.data as Extract<MessageList, { kind: "messages" }>).rows.find((r) => r.id === 1001)!.flags.flagged;
    expect(flagged(s)).toBe(true);
    s = run(s, { type: "mutation_settled", batch: 3, kind: "flag", account: "work", done: [], failed: [{ target: row(1001), reason: "no" }] });
    expect(flagged(s)).toBe(false);
  });

  it("marks read in place and moves the unread count", () => {
    const s = run(booted(), { type: "mutation_apply", batch: 4, kind: "read", targets: [row(1002), row(1001)], value: true });
    expect(box(s, "inbox").unread).toBe(3);
  });

  it("a later batch owns a row it took over: the earlier batch's answer leaves it alone", () => {
    let s = run(booted(), { type: "mutation_apply", batch: 1, kind: "flag", targets: [row(1001)], value: true });
    s = run(s, { type: "mutation_apply", batch: 2, kind: "flag", targets: [row(1001)], value: false });
    s = run(s, { type: "mutation_settled", batch: 1, kind: "flag", account: "work", done: [row(1001)], failed: [] });
    expect(s.pending["work#1001"]).toMatchObject({ flag: { batch: 2, prev: false } });
  });

  it("a refused row whose axis a newer batch of the same kind took over re-reads the list and counts", () => {
    let s = run(booted(), { type: "mutation_apply", batch: 1, kind: "flag", targets: [row(1001)], value: true });
    s = run(s, { type: "mutation_apply", batch: 2, kind: "flag", targets: [row(1001)], value: false });
    s = run(s, { type: "mutation_settled", batch: 1, kind: "flag", account: "work", done: [], failed: [{ target: row(1001), reason: "no" }] });
    expect(s.pending["work#1001"]).toMatchObject({ flag: { batch: 2 } });
    expect(isStale(s.messages)).toBe(true);
    expect(isStale(s.mailboxes.work)).toBe(true);
  });

  const flagsOf = (s: AppState, id: number) =>
    (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows.find((x) => x.id === id)!.flags;

  it("flag then read, the flag refused: the flag is put back and the read stays", () => {
    let s = run(booted(), { type: "mutation_apply", batch: 1, kind: "flag", targets: [row(1002)], value: true });
    s = run(s, { type: "mutation_apply", batch: 2, kind: "read", targets: [row(1002)], value: true });
    expect(box(s, "inbox").unread).toBe(3);
    s = run(s, { type: "mutation_settled", batch: 1, kind: "flag", account: "work", done: [], failed: [{ target: row(1002), reason: "no" }] });
    expect(flagsOf(s, 1002)).toMatchObject({ flagged: false, seen: true });
    expect(box(s, "inbox").unread).toBe(3);
    expect(s.pending["work#1002"]).toMatchObject({ flag: null, read: { batch: 2, prev: false, value: true } });
    expect(isStale(s.messages)).toBe(true);

    s = run(s, { type: "mutation_settled", batch: 2, kind: "read", account: "work", done: [row(1002)], failed: [] });
    expect(s.pending).toEqual({});
    expect(flagsOf(s, 1002)).toMatchObject({ flagged: false, seen: true });
  });

  it("read then flag, the read refused: the unread count and the read state come back, the flag stays", () => {
    let s = run(booted(), { type: "mutation_apply", batch: 1, kind: "read", targets: [row(1002)], value: true });
    s = run(s, { type: "mutation_apply", batch: 2, kind: "flag", targets: [row(1002)], value: true });
    expect(box(s, "inbox").unread).toBe(3);
    s = run(s, { type: "mutation_failed", batch: 1, kind: "read", account: "work", targets: [row(1002)], error: { kind: "timeout", message: "slow" } });
    expect(box(s, "inbox").unread).toBe(4);
    expect(flagsOf(s, 1002)).toMatchObject({ flagged: true, seen: false });
    expect(s.pending["work#1002"]).toMatchObject({ read: null, flag: { batch: 2, value: true } });
  });

  it("two archives refused in answer order re-read the list, whose order replaces the guess", () => {
    let s = run(booted(), archive(1, 1002), archive(2, 1003));
    expect(rows(s)).toEqual([1001, 1004, 1005, 1006, 1007, 1008]);
    s = run(s, { type: "mutation_settled", batch: 1, kind: "archive", account: "work", done: [], failed: [{ target: row(1002), reason: "no" }] });
    s = run(s, { type: "mutation_settled", batch: 2, kind: "archive", account: "work", done: [], failed: [{ target: row(1003), reason: "no" }] });
    // Each saved index predates the other archive: the immediate guess swaps the two.
    expect(rows(s)).toEqual([1001, 1003, 1002, 1004, 1005, 1006, 1007, 1008]);
    expect(isStale(s.messages)).toBe(true);
    s = run(s, { type: "messages_loaded", key, gen: s.messages.gen, lgen: s.listGen[key], list: inbox("work", "inbox") });
    expect(rows(s)).toEqual([1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008]);
  });

  it("a list answer between the apply and the refusal re-reads the list after the restore", () => {
    let s = run(booted(), archive(1, 1003));
    // New mail lands at the top while the archive is out; the overlay keeps 1003 hidden.
    const fresh = inbox("work", "inbox") as Extract<MessageList, { kind: "messages" }>;
    const withNew: MessageList = { ...fresh, total: fresh.total + 1, rows: [{ ...fresh.rows[0], id: 1000, message_id: "<new@x>", selector: "mp://work/inbox/new" }, ...fresh.rows] };
    s = run(s, envelope("state.invalidate", { resource: "mailbox:work/inbox", scope: {} }));
    s = run(s, { type: "messages_loaded", key, gen: s.messages.gen, lgen: s.listGen[key], list: withNew });
    expect(rows(s)).toEqual([1000, 1001, 1002, 1004, 1005, 1006, 1007, 1008]);
    s = run(s, { type: "mutation_settled", batch: 1, kind: "archive", account: "work", done: [], failed: [{ target: row(1003), reason: "no" }] });
    // The saved index predates the new row: the guess lands one place too high.
    expect(rows(s)).toEqual([1000, 1001, 1003, 1002, 1004, 1005, 1006, 1007, 1008]);
    expect(isStale(s.messages)).toBe(true);
    s = run(s, { type: "messages_loaded", key, gen: s.messages.gen, lgen: s.listGen[key], list: withNew });
    expect(rows(s)).toEqual([1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008]);
  });

  it("a change of another kind restores its own axis only", () => {
    let s = run(booted(), { type: "mutation_apply", batch: 1, kind: "flag", targets: [row(1002)], value: true });
    s = run(s, { type: "mutation_apply", batch: 2, kind: "read", targets: [row(1002)], value: true });
    s = run(s, { type: "mutation_failed", batch: 2, kind: "read", account: "work", targets: [row(1002)], error: { kind: "timeout", message: "slow" } });
    const r = (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows.find((x) => x.id === 1002)!;
    expect(r.flags).toMatchObject({ flagged: true, seen: false });
    expect(box(s, "inbox").unread).toBe(4);
  });

  it("restores the rolled-back account's pending rows only, and re-reads its lists", () => {
    let s = run(booted(), archive(1, 1002));
    s = run(s, { type: "mutation_apply", batch: 2, kind: "archive", targets: [{ account: "home", row_id: 1015 }] });
    s = run(s, envelope("mutations.rolled_back", { account: "work", failed: 1 }));
    expect(rows(s)).toContain(1002);
    expect(Object.keys(s.pending)).toEqual(["home#1015"]);
    expect(isStale(s.messages)).toBe(true);
    expect(last(s)).toMatchObject({ kind: "rolled_back", account: "work", text: "work: 1 mutation(s) failed and were rolled back (see the log)" });
  });

  it("drops a list reload from before the mutation, takes a newer one, and hides rows still pending", () => {
    let s = booted();
    s = run(s, envelope("state.invalidate", { resource: "mailbox:work/inbox", scope: {} }));
    const staleAt = { gen: s.messages.gen, lgen: s.listGen[key] ?? 0 };
    s = run(s, archive(1, 1002));
    expect(s.listGen[key]).toBe(staleAt.lgen + 1);

    // Requested before the archive: it still has the row, and is dropped.
    s = run(s, { type: "messages_loaded", key, ...staleAt, list: inbox("work", "inbox") });
    expect(rows(s)).not.toContain(1002);
    expect(s.messages.gen).toBe(staleAt.gen + 1);
    expect(isStale(s.messages)).toBe(true);

    // The refetch may predate the daemon's commit: the pending row stays hidden.
    s = run(s, { type: "messages_loaded", key, gen: s.messages.gen, lgen: s.listGen[key], list: inbox("work", "inbox") });
    expect(isStale(s.messages)).toBe(false);
    expect(rows(s)).not.toContain(1002);
    expect((s.messages.data as Extract<MessageList, { kind: "messages" }>).total).toBe(7);

    // Of two reloads, the older answer never replaces the newer one.
    const newer = s.messages.loadedGen;
    const before = s;
    s = run(s, { type: "messages_loaded", key, gen: newer - 1, lgen: s.listGen[key], list: inbox("work", "inbox") });
    expect(s).toBe(before);
  });

  it("clears pending and reseeds the holds on a re-bootstrap", () => {
    let s = run(booted(), archive(1, 1002));
    expect(Object.keys(s.holds)).toEqual(["fixture-hold-seed"]);
    s = run(s, envelope("send.hold_fired", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }));
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
    expect(s.pending).toEqual({});
    expect(s.holds["fixture-hold-seed"]).toMatchObject({ state: "started", remaining_secs: 60 });
  });

  describe("send holds", () => {
    const hold = { operation_id: "op-h", account: "work", draft_id: "d", subject: "Hi", hold_secs: 10, remaining_secs: 10, fires_at: "2026-09-30T12:00:10Z", origin: "tui" };

    it("follows started, tick and cancelled, and ignores a tick after the end", () => {
      let s = run(booted(), envelope("send.hold_started", hold, 501));
      expect(s.holds["op-h"]).toMatchObject({ state: "started", remaining_secs: 10 });
      s = run(s, envelope("send.hold_tick", { ...hold, remaining_secs: 7 }, 502));
      expect(s.holds["op-h"]).toMatchObject({ state: "tick", remaining_secs: 7 });
      s = run(s, envelope("send.hold_cancelled", { ...hold, remaining_secs: 0 }, 503));
      expect(s.holds["op-h"]).toMatchObject({ state: "cancelled", remaining_secs: 0 });
      s = run(s, envelope("send.hold_tick", { ...hold, remaining_secs: 6 }, 504));
      expect(s.holds["op-h"].state).toBe("cancelled");
      expect(s.activity.filter((n) => n.kind === "hold_cancelled").map((n) => n.text)).toEqual(['Send of "Hi" cancelled']);
    });

    it("follows a hold to fired, and counts the notice of a cancel once whichever lands first", () => {
      let s = run(booted(), envelope("send.hold_started", hold, 501), envelope("send.hold_fired", { ...hold, remaining_secs: 0 }, 502));
      expect(s.holds["op-h"].state).toBe("fired");
      s = run(s, { type: "hold_cancel_requested", operation_id: "fixture-hold-seed" });
      expect(s.holds["fixture-hold-seed"].cancelling).toBe(true);
      s = run(s, { type: "hold_cancel_answered", operation_id: "fixture-hold-seed", cancelled: true });
      s = run(s, envelope("send.hold_cancelled", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }, 503));
      expect(s.holds["fixture-hold-seed"]).toMatchObject({ state: "cancelled", cancelling: false });
      expect(s.activity.filter((n) => n.kind === "hold_cancelled")).toHaveLength(1);
      s = run(s, { type: "dismiss_hold", operation_id: "op-h" });
      expect(s.holds["op-h"]).toBeUndefined();
    });

    it("reports a cancel the daemon refused", () => {
      let s = run(booted(), { type: "hold_cancel_requested", operation_id: "fixture-hold-seed" });
      s = run(s, { type: "hold_cancel_failed", operation_id: "fixture-hold-seed", error: { kind: "not_found", message: "no hold", code: -32602 } });
      expect(s.holds["fixture-hold-seed"].cancelling).toBe(false);
      expect(last(s)?.kind).toBe("hold_cancel_failed");
      s = run(s, { type: "dismiss_notice", id: last(s)!.id });
      expect(s.activity).toEqual([]);
    });
  });

  describe("the multi-select", () => {
    it("marks a range from the anchor, adds a second range, and acts in list order", () => {
      let s = booted();
      s = run(s, { type: "mark_toggle", key: "work#1002" });
      s = run(s, { type: "mark_range", key: "work#1005" });
      expect([...s.marked.keys].sort()).toEqual(["work#1002", "work#1003", "work#1004", "work#1005"]);
      expect(s.marked.anchor).toBe("work#1002");
      s = run(s, { type: "mark_range", key: "work#1001" });
      expect(s.marked.keys.size).toBe(5);
      s = run(s, { type: "mark_toggle", key: "work#1003" });
      expect(actionTargets(s)).toEqual([1001, 1002, 1004, 1005].map(row));
      s = run(s, { type: "mark_clear" });
      expect(s.marked.keys.size).toBe(0);
    });

    it("starts a range at the cursor with no anchor, and marks everything shown", () => {
      let s = pick(booted(), 1007);
      s = run(s, { type: "mark_range", key: "work#1008" });
      expect([...s.marked.keys]).toEqual(["work#1007", "work#1008"]);
      s = run(s, { type: "mark_all" });
      expect(s.marked.keys.size).toBe(8);
      expect(run(s, { type: "mark_set", keys: ["work#1001", "work#1002"], on: false }).marked.keys.size).toBe(6);
    });

    it("drops marks of rows that left, and those a reload no longer lists", () => {
      let s = run(booted(), { type: "mark_set", keys: ["work#1002", "work#1003", "work#1004"], on: true });
      s = run(s, archive(1, 1002));
      expect([...s.marked.keys]).toEqual(["work#1003", "work#1004"]);
      s = run(s, envelope("state.remove", { resource: "message:work/inbox/x" }));
      const reloaded = inbox("work", "inbox");
      if (reloaded.kind !== "messages") throw new Error("unreachable");
      reloaded.rows = reloaded.rows.filter((r) => r.id !== 1004);
      s = run(s, { type: "messages_loaded", key, gen: s.messages.gen, lgen: s.listGen[key], list: reloaded });
      expect([...s.marked.keys]).toEqual(["work#1003"]);
    });

    it("clears the marks when another mailbox is shown", () => {
      const s = run(booted(), { type: "mark_toggle", key: "work#1002" }, { type: "select_mailbox", account: "work", slug: "sent" });
      expect(s.marked.keys.size).toBe(0);
    });
  });

  describe("where the cursor goes when its row leaves", () => {
    it("goes to the next row that stays", () => {
      const s = run(pick(booted(), 1003), archive(1, 1003, 1004));
      expect(s.selection.message?.row_id).toBe(1005);
    });

    it("goes to the previous row when none follows", () => {
      const s = run(pick(booted(), 1008), archive(1, 1007, 1008));
      expect(s.selection.message?.row_id).toBe(1006);
    });

    it("goes nowhere when the list empties, and stays put when its row stays", () => {
      let s = run(pick(booted(), 1004), archive(1, 1001, 1002));
      expect(s.selection.message?.row_id).toBe(1004);
      s = run(s, archive(2, 1003, 1004, 1005, 1006, 1007, 1008));
      expect(s.selection.message).toBeNull();
      expect(s.reader.meta).toBeNull();
    });

    it("moves among search hits the same way", () => {
      let s = run(booted(), { type: "search_local", query: "x" });
      const hits = inbox("work", "inbox");
      if (hits.kind !== "messages") throw new Error("unreachable");
      s = run(s, { type: "search_local_loaded", seq: s.search!.seq, hits: hits.rows.slice(0, 3).map((r) => ({ ...r, mailbox: "inbox" })) });
      s = run(s, { type: "move_selection", to: 1, relative: false });
      expect(s.selection.message?.row_id).toBe(1002);
      s = run(s, archive(1, 1002));
      expect(s.search?.hits.map((h) => h.row_id)).toEqual([1001, 1003]);
      expect(s.selection.message?.row_id).toBe(1003);
    });
  });

  describe("syncs", () => {
    it("reports a failed sync, even when its end overtook the start answer", () => {
      let s = run(booted(), { type: "sync_requested" });
      s = run(s, envelope("operation.finished", { operation_id: "fixture-op-1", state: "failed", error: { code: -32000, message: "login refused" } }));
      expect(s.syncEarly).toHaveLength(1);
      s = run(s, { type: "sync_started", operation_id: "fixture-op-1", account: "home", mode: "quick" });
      expect(s.syncs).toEqual({});
      expect(last(s)).toMatchObject({ kind: "sync_failed", text: "Sync of home failed: login refused" });
    });

    it("logs a failed sync of this window once: the tick's line, and the notice without a second line", () => {
      let s = run(booted(), { type: "sync_requested" }, { type: "sync_started", operation_id: "op-1", account: "work", mode: "quick" });
      const before = s.activityLog.length;
      const tick = { account: "work", severity: "error", error: "login refused", saved: 0, new_inbox_mail: [] };
      s = run(s, envelope("sync.completed", tick, 501));
      s = run(s, envelope("operation.finished", { operation_id: "op-1", state: "failed", error: { code: -32000, message: "login refused" } }, 502));
      expect(last(s)).toMatchObject({ kind: "sync_failed", text: "Sync of work failed: login refused" });
      expect(s.activityLog.slice(before).map((e) => e.text)).toEqual(["Fetch failed (work): login refused"]);
      // Another account's failed tick leaves this window's sync of work alone.
      s = run(s, { type: "sync_requested" }, { type: "sync_started", operation_id: "op-2", account: "work", mode: "quick" });
      s = run(s, envelope("sync.completed", { ...tick, account: "home" }, 503));
      s = run(s, envelope("operation.finished", { operation_id: "op-2", state: "failed", error: { code: -32000, message: "timeout" } }, 504));
      expect(s.activityLog.slice(-2).map((e) => e.text)).toEqual(["Fetch failed (home): login refused", "Sync of work failed: timeout"]);
    });

    it("settles quietly on success and reports a dropped sync", () => {
      let s = run(booted(), { type: "sync_requested" }, { type: "sync_started", operation_id: "op-1", account: "work", mode: "full" });
      s = run(s, envelope("operation.finished", { operation_id: "op-1", state: "succeeded", result: {} }));
      expect(s.syncs).toEqual({});
      expect(s.activity).toEqual([]);
      s = run(s, { type: "sync_requested" }, { type: "sync_started", operation_id: "op-2", account: "work", mode: "quick" });
      s = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "op-2", kind: "sync", reason: "the daemon restarted" } });
      expect(last(s)?.text).toBe("Sync of work was dropped: the daemon restarted");
    });
  });

  describe("sends", () => {
    const hold = (remaining: number, op = "send-1") => ({
      operation_id: op,
      account: "work",
      draft_id: "angebot-antwort",
      subject: "Re: Angebot Dachsanierung",
      hold_secs: 20,
      remaining_secs: remaining,
      fires_at: "2026-09-30T12:00:20Z",
      origin: "gui",
    });
    const requested = (token = 1): Action => ({
      type: "send_requested",
      token,
      kind: "draft",
      account: "work",
      drafts: ["angebot-antwort"],
      subject: "Re: Angebot Dachsanierung",
    });
    const outcome = (delivered: boolean[]) => ({
      account: "work",
      selector: "mp://work/drafts/angebot-antwort",
      message_id: "<m@x>",
      status_line: "sent + saved",
      recipients: delivered.map((d, i) => ({
        address: `r${i}@example.com`,
        role: "To",
        delivered: d,
        error: d ? null : "550 no such mailbox",
      })),
      sent_copy: "filed",
      settle_error: null,
    });
    const finished = (state: string, extra: Record<string, unknown> = {}, op = "send-1", revision = 600) =>
      envelope("operation.finished", { operation_id: op, state, ...extra }, revision);

    it("started, ticked, fired and settled: the card counts down, then says Sent", () => {
      let s = run(booted(), requested());
      expect(s.sends).toHaveLength(1);
      // The hold's first event overtakes the command's answer.
      s = run(s, envelope("send.hold_started", hold(20), 501));
      s = run(s, { type: "send_started", token: 1, operation_id: "send-1", held: true });
      expect(s.sends[0]).toMatchObject({ operation_id: "send-1", held: true });
      s = run(s, envelope("send.hold_tick", hold(12), 502), envelope("send.hold_fired", hold(0), 503));
      expect(s.holds["send-1"].state).toBe("fired");
      expect(s.holds["send-1"].outcome).toBeUndefined();
      // Still sending until the operation settles.
      expect(s.sends).toHaveLength(1);
      s = run(s, finished("succeeded", { result: outcome([true]) }));
      expect(s.sends).toEqual([]);
      expect(s.holds["send-1"].outcome).toEqual({ tone: "sent", text: "Sent", sticky: false });
      expect(s.activity).toEqual([]);
    });

    it("started then cancelled: the card says Send cancelled and no notice repeats it", () => {
      let s = run(booted(), requested(), { type: "send_started", token: 1, operation_id: "send-1", held: true });
      s = run(s, envelope("send.hold_started", hold(20), 501), envelope("send.hold_cancelled", hold(0), 502));
      s = run(s, finished("cancelled", { error: { code: -32008, message: "operation_cancelled" } }));
      expect(s.sends).toEqual([]);
      expect(s.holds["send-1"]).toMatchObject({ state: "cancelled", outcome: { tone: "cancelled", text: "Send cancelled" } });
      expect(s.activity.filter((n) => n.kind !== "hold_cancelled")).toEqual([]);
    });

    it("settled with no hold (send_hold_secs = 0): a plain Sent notice", () => {
      let s = run(booted(), requested(), { type: "send_started", token: 1, operation_id: "send-1", held: false });
      s = run(s, finished("succeeded", { result: outcome([true, true]) }));
      expect(s.holds["send-1"]).toBeUndefined();
      expect(last(s)).toMatchObject({ kind: "applied", text: "Sent" });
    });

    it("an end that overtakes the answer is held for it", () => {
      let s = run(booted(), requested());
      s = run(s, finished("succeeded", { result: outcome([true]) }));
      expect(s.sendEarly).toHaveLength(1);
      s = run(s, { type: "send_started", token: 1, operation_id: "send-1", held: false });
      expect(s.sends).toEqual([]);
      expect(s.sendEarly).toEqual([]);
      expect(last(s)).toMatchObject({ kind: "applied", text: "Sent" });
    });

    it("a partial delivery reads Partly delivered and names the refused recipients, never a plain failure", () => {
      let s = run(booted(), requested(), { type: "send_started", token: 1, operation_id: "send-1", held: false });
      s = run(s, finished("succeeded", { result: outcome([true, false]) }));
      expect(last(s)).toMatchObject({ kind: "send_partial", text: "Partly delivered: r1@example.com (550 no such mailbox)" });
      // With a card, the card says it and stays until dismissed.
      s = run(s, requested(2), { type: "send_started", token: 2, operation_id: "send-2", held: true });
      s = run(s, envelope("send.hold_started", hold(20, "send-2"), 610), envelope("send.hold_fired", hold(0, "send-2"), 611));
      s = run(s, finished("succeeded", { result: outcome([true, false]) }, "send-2", 612));
      expect(s.holds["send-2"].outcome).toEqual({
        tone: "partial",
        text: "Partly delivered: r1@example.com (550 no such mailbox)",
        sticky: true,
      });
    });

    it("a failed send says Failed with the daemon's reason", () => {
      let s = run(booted(), requested(), { type: "send_started", token: 1, operation_id: "send-1", held: false });
      s = run(s, finished("failed", { error: { code: -32603, message: "421 closed" } }));
      expect(last(s)).toMatchObject({ kind: "send_failed", text: "Failed: 421 closed" });
    });

    it("a dropped send says it was interrupted and re-reads the Drafts list", () => {
      let s = run(booted(), { type: "select_mailbox", account: "work", slug: "drafts" });
      s = run(s, { type: "messages_loaded", key: listKey("work", "drafts"), gen: s.messages.gen, list: { kind: "drafts", account: "work", listing: fixtures.drafts.work } });
      expect(isStale(s.messages)).toBe(false);
      s = run(s, requested(), { type: "send_started", token: 1, operation_id: "send-1", held: true });
      s = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "send-1", kind: "send", reason: "the daemon restarted" } });
      expect(s.sends).toEqual([]);
      expect(last(s)).toMatchObject({ kind: "send_failed", text: "The send was interrupted; check the outbox" });
      expect(isStale(s.messages)).toBe(true);
    });

    it("a batch says Sent N, failed M with its failures as an alert", () => {
      let s = run(booted(), { type: "send_requested", token: 1, kind: "approved", account: "work", drafts: ["a", "b"], subject: null });
      s = run(s, { type: "send_started", token: 1, operation_id: "send-1", held: false });
      const result = {
        account: "work",
        results: [outcome([true]), { ...outcome([]), selector: "mp://work/drafts/b", status_line: "no recipient" }],
        sent: 1,
        failed: 1,
      };
      s = run(s, { type: "gui_event", event: { type: "operation_settled", operation_id: "send-1", kind: "send_approved", status: { operation_id: "send-1", method: "send.approved", state: "succeeded", scope: "durable", progress: null, result, error: null } } });
      expect(last(s)).toMatchObject({ kind: "send_failed", text: "Sent 1, failed 1", rows: [{ label: "b", reason: "Failed: no recipient" }] });
      s = run(s, { type: "send_requested", token: 2, kind: "approved", account: "work", drafts: [], subject: null });
      s = run(s, { type: "send_started", token: 2, operation_id: "send-2", held: false });
      s = run(s, finished("succeeded", { result: { account: "work", results: [], sent: 0, failed: 0 } }, "send-2", 700));
      expect(last(s)).toMatchObject({ kind: "applied", text: "No approved emails found" });
    });

    it("a refused start frees the draft and says why, with the file for one that does not parse", () => {
      let s = run(booted(), requested());
      const invalid = { account: "work", id: "angebot-antwort", path: "/x/angebot-antwort.md", diagnostics: [{ line: 3, message: "bad yaml" }] };
      s = run(s, { type: "send_failed", token: 1, error: { kind: "protocol", message: "bad yaml", code: -32010 }, invalid });
      expect(s.sends).toEqual([]);
      expect(last(s)).toMatchObject({ kind: "send_failed", text: "Send failed: the draft does not parse: line 3: bad yaml (/x/angebot-antwort.md)" });
    });

    it("a send outlives a re-bootstrap of the same daemon, and its ended card too", () => {
      let s = run(booted(), requested(), { type: "send_started", token: 1, operation_id: "send-1", held: true });
      s = run(s, envelope("send.hold_started", hold(20), 501), envelope("send.hold_fired", hold(0), 502));
      s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
      expect(s.sends).toHaveLength(1);
      expect(s.holds["send-1"]?.state).toBe("fired");
      s = run(s, { type: "gui_event", event: { type: "operation_settled", operation_id: "send-1", kind: "send", status: { operation_id: "send-1", method: "send.draft", state: "succeeded", scope: "durable", progress: null, result: outcome([true]), error: null } } });
      expect(s.holds["send-1"].outcome?.text).toBe("Sent");
    });
  });

  describe("the activity notices", () => {
    const failSync = (n: number): Action => ({ type: "sync_failed", account: `acct${n}`, error: { kind: "internal", message: "no route" } });

    it("keeps the newest ACTIVITY_CAP notices, failures included, and drops the oldest", () => {
      expect(ACTIVITY_CAP).toBe(20);
      const s = run(booted(), ...Array.from({ length: 25 }, (_, i) => failSync(i + 1)));
      expect(s.activity).toHaveLength(20);
      expect(s.activity[0].account).toBe("acct6");
      expect(last(s).account).toBe("acct25");
      expect(s.activitySeq).toBe(25);
    });

    it("dismisses one notice by id, or all of them", () => {
      let s = run(booted(), failSync(1), failSync(2), failSync(3));
      s = run(s, { type: "dismiss_notice", id: s.activity[1].id });
      expect(s.activity.map((n) => n.account)).toEqual(["acct1", "acct3"]);
      s = run(s, { type: "dismiss_all_notices" });
      expect(s.activity).toEqual([]);
      expect(run(s, { type: "dismiss_all_notices" })).toBe(s);
    });
  });

  describe("operation progress", () => {
    const progressEvent = (id: string, done: number, revision: number, message: string | null = null): Action => ({
      type: "gui_event",
      event: {
        type: "event",
        event: {
          instance_id: fixtures.bootstrap.instance_id,
          revision,
          kind: "operation.progress",
          payload: { operation_id: id, phase: "contacts", done, total: null, message },
        },
      },
    });
    const finished = (id: string, revision: number): Action => ({
      type: "gui_event",
      event: {
        type: "event",
        event: { instance_id: fixtures.bootstrap.instance_id, revision, kind: "operation.finished", payload: { operation_id: id, state: "succeeded", result: {}, error: null } },
      },
    });

    it("decodes a report into the typed action and keeps the last one by operation id", () => {
      expect(progressAction({ operation_id: "op-1", phase: "device_code", done: 0, total: null, message: "https://microsoft.com/devicelogin FXTR" })).toEqual({
        type: "operation_progress",
        operation_id: "op-1",
        progress: { phase: "device_code", done: 0, total: null, message: "https://microsoft.com/devicelogin FXTR" },
      });
      expect(progressAction({ phase: "x", done: 0 })).toBeNull();
      let s = run(booted(), progressEvent("op-1", 0, 900), progressEvent("op-1", 1, 901, "work"));
      expect(s.progress).toEqual({ "op-1": { phase: "contacts", done: 1, total: null, message: "work" } });
      s = run(s, { type: "operation_progress", operation_id: "op-2", progress: { phase: "draft", done: 0, total: 2, message: null } });
      expect(Object.keys(s.progress)).toEqual(["op-1", "op-2"]);
    });

    it("drops the report when its operation finishes, settles or is dropped", () => {
      let s = run(booted(), progressEvent("op-1", 0, 900), progressEvent("op-2", 0, 901), progressEvent("op-3", 0, 902));
      s = run(s, finished("op-1", 903));
      expect(Object.keys(s.progress)).toEqual(["op-2", "op-3"]);
      s = run(s, {
        type: "gui_event",
        event: { type: "operation_settled", operation_id: "op-2", kind: "sync", status: { operation_id: "op-2", method: "sync.quick", state: "succeeded", scope: "durable", progress: null, result: null, error: null } },
      });
      s = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "op-3", kind: "sync", reason: "daemon restarted" } });
      expect(s.progress).toEqual({});
    });

    it("keeps the reports over a re-bootstrap of the same daemon and drops them for another", () => {
      let s = run(booted(), progressEvent("op-1", 0, 900));
      s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
      expect(Object.keys(s.progress)).toEqual(["op-1"]);
      s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "instance_changed", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } } });
      expect(s.progress).toEqual({});
    });
  });

  describe("views", () => {
    const view = (v: "mail" | "contacts" | "calendar" | "settings"): Action => ({ type: "switch_view", view: v });
    const rows = (s: AppState) => (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows;

    it("switches to a view and back keeping the selection and the marks, with focus on the view's pane", () => {
      let s = booted();
      const row = rows(s)[1];
      s = run(s, { type: "select_message", message: { row_id: row.id, message_id: row.message_id, selector: row.selector }, focus: "reader" });
      s = run(s, { type: "mark_toggle", key: `work#${rows(s)[3].id}` });
      const seq = s.focusSeq;
      s = run(s, view("contacts"));
      expect(s.view).toBe("contacts");
      expect(s.focus).toBe("list");
      expect(s.focusSeq).toBeGreaterThan(seq);
      s = run(s, view("calendar"), view("mail"));
      expect(s.view).toBe("mail");
      expect(s.focus).toBe("list");
      expect(s.selection.message?.row_id).toBe(row.id);
      expect([...s.marked.keys]).toEqual([`work#${rows(s)[3].id}`]);
    });

    it("ends a search when it leaves Mail, and Space m inside Mail keeps it", () => {
      let s = run(booted(), { type: "search_local", query: "ledger" });
      expect(run(s, view("mail")).search?.query).toBe("ledger");
      s = run(s, view("contacts"));
      expect(s.search).toBeNull();
      expect(s.selection).toMatchObject({ account: "work", mailbox: "inbox" });
    });

    it("keeps the outbox view inside Mail across a round trip through Contacts", () => {
      let s = run(booted(), { type: "open_outbox", account: "home" }, view("contacts"));
      expect(s.outboxView?.account).toBe("home");
      s = run(s, view("mail"));
      expect(s.view).toBe("mail");
      expect(s.outboxView?.account).toBe("home");
    });

    it("hides the actions on the mailbox selection outside Mail, and names the view first", () => {
      let s = run(booted(), { type: "open_outbox", account: "work" });
      expect(hiddenNotice(s, "archive")).toBe(CLOSE_OUTBOX_FIRST);
      s = run(s, view("calendar"));
      for (const id of ["archive", "reply", "copy_selector", "mark_toggle", "open_message", "send", "open_attachment"] as const) {
        expect(hiddenByView(s, id)).toBe(true);
        expect(hiddenNotice(s, id)).toBe(BACK_TO_MAIL_FIRST);
      }
      for (const id of ["quick_sync", "cancel_hold", "toggle_help", "view_mail", "view_contacts", "open_settings", "new_draft", "dismiss_notice"] as const) {
        expect(hiddenNotice(s, id)).toBeNull();
      }
      expect(hiddenByView(run(s, view("mail"), { type: "close_outbox" }), "archive")).toBe(false);
    });

    it("comes back to Mail for a mailbox, a search, an outbox or Escape, and stays for another account", () => {
      const contacts = run(booted(), view("contacts"));
      expect(run(contacts, { type: "select_mailbox", account: "work", slug: "sent", focus: "list" }).view).toBe("mail");
      expect(run(contacts, { type: "search_local", query: "x" }).view).toBe("mail");
      expect(run(contacts, { type: "open_outbox", account: "work" }).view).toBe("mail");
      expect(run(contacts, { type: "clear_selection" })).toMatchObject({ view: "mail", focus: "list" });
      const other = run(contacts, { type: "next_account" });
      expect(other.view).toBe("contacts");
      expect(other.selection.account).toBe("home");
    });

    it("leaves the hidden mailbox list alone for a move, and Tab skips the reader", () => {
      let s = run(booted(), view("settings"));
      expect(run(s, { type: "move_selection", to: 1, relative: true })).toBe(s);
      s = run(s, { type: "cycle_focus", dir: 1 });
      expect(s.focus).toBe("sidebar");
      s = run(s, { type: "cycle_focus", dir: 1 });
      expect(s.focus).toBe("list");
      s = run(s, { type: "cycle_focus", dir: -1 }, { type: "cycle_focus", dir: -1 });
      expect(s.focus).toBe("list");
    });

    it("never goes back into the reader of a hidden Mail", () => {
      const s = run(booted(), { type: "focus", pane: "sidebar" }, { type: "focus", pane: "reader" }, view("contacts"), { type: "back" });
      expect(s.focus).toBe("sidebar");
    });
  });
});
