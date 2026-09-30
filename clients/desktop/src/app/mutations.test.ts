import { beforeEach, describe, expect, it } from "vitest";
import * as cmd from "@/lib/commands";
import { createMutations } from "@/app/mutations";
import { reducer, type Action } from "@/app/reducer";
import { initialState, listKey, type AppState } from "@/app/state";
import { fixtures, mailboxListing, mock, resetMock } from "@/test/tauri-mock";
import type { MessageList } from "@/lib/gui-types";

/** The reducer behind a plain variable, with the fixture's work inbox shown. */
async function store(mailbox = "inbox") {
  let s = reducer(initialState(), { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap } });
  const dispatch = (a: Action) => {
    s = reducer(s, a);
  };
  dispatch({ type: "mailboxes_loaded", account: "work", gen: 1, listing: mailboxListing("work") });
  if (mailbox !== "inbox") dispatch({ type: "select_mailbox", account: "work", slug: mailbox });
  dispatch({ type: "messages_loaded", key: listKey("work", mailbox), gen: s.messages.gen, list: await cmd.listMessages("work", mailbox) });
  return { get: () => s, dispatch, m: createMutations(dispatch) };
}

const rowIds = (s: AppState) => (s.messages.data as Extract<MessageList, { kind: "messages" }>).rows.map((r) => r.id);
const row = (row_id: number, account = "work") => ({ account, row_id });
const calls = (name: string) => mock.calls.filter((c) => c.cmd === name).map((c) => c.args);

beforeEach(() => resetMock());

describe("the mutation dispatch", () => {
  it("applies before the command answers, then confirms from the batch", async () => {
    const { get, m } = await store();
    const done = m.archive([row(1002), row(1003)]);
    expect(rowIds(get())).toEqual([1001, 1004, 1005, 1006, 1007, 1008]);
    expect(Object.keys(get().pending)).toEqual(["work#1002", "work#1003"]);
    expect(await done).toEqual({ done: 2, failed: 0 });
    expect(calls("message_archive")).toEqual([{ account: "work", row_ids: [1002, 1003] }]);
    expect(get().pending).toEqual({});
    expect(rowIds(get())).toEqual([1001, 1004, 1005, 1006, 1007, 1008]);
    expect(mock.rows.work.archive.map((r) => r.id)).toEqual(expect.arrayContaining([1002, 1003]));
    expect(get().activity.map((n) => n.text)).toEqual(["Archived 2 messages"]);
  });

  it("puts back the row the daemon refused and keeps the one it took", async () => {
    const { get, m } = await store();
    // Another client deleted 1003 already: the daemon no longer holds it.
    mock.rows.work.inbox = mock.rows.work.inbox.filter((r) => r.id !== 1003);
    expect(await m.remove([row(1002), row(1003)])).toEqual({ done: 1, failed: 1 });
    expect(rowIds(get())).toEqual([1001, 1003, 1004, 1005, 1006, 1007, 1008]);
    const failed = get().activity.find((n) => n.kind === "failed");
    expect(failed?.text).toBe("Could not delete 1 message; it is back in the list");
    expect(failed?.rows[0].reason).toMatch(/holds no message with row id 1003/);
  });

  it("puts back every row when the command throws", async () => {
    const { get, m } = await store();
    mock.failing.set("message_move", { kind: "daemon_unavailable", message: "no daemon", socket: null, log: null });
    expect(await m.move([row(1001), row(1002)], "sent")).toEqual({ done: 0, failed: 2 });
    expect(rowIds(get())).toEqual([1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008]);
    expect(get().pending).toEqual({});
    expect(get().activity[0].rows.map((r) => r.reason)).toEqual(["no daemon", "no daemon"]);
  });

  it("names where a move went, and refuses a mailbox the account lacks", async () => {
    const { get, m } = await store();
    await m.move([row(1001)], "Sent");
    expect(calls("message_move")[0]).toEqual({ account: "work", row_ids: [1001], destination: "Sent" });
    expect(get().activity[0].text).toBe("Moved 1 message to Sent");
    expect(await m.move([row(1002)], "Nowhere")).toEqual({ done: 0, failed: 1 });
    expect(rowIds(get())).toContain(1002);
  });

  it("flags and marks read in place, and the answer confirms the state", async () => {
    const { get, m } = await store();
    await m.setFlag([row(1001)], true);
    await m.setRead([row(1002)], true);
    const r = (id: number) => (get().messages.data as Extract<MessageList, { kind: "messages" }>).rows.find((x) => x.id === id)!;
    expect(r(1001).flags.flagged).toBe(true);
    expect(r(1002).flags.seen).toBe(true);
    expect(get().pending).toEqual({});
    expect(calls("message_set_flag")).toEqual([{ account: "work", row_ids: [1001], flagged: true }]);
    expect(calls("message_set_read")).toEqual([{ account: "work", row_ids: [1002], read: true }]);
    expect(get().activity.map((n) => n.text)).toEqual(["Flagged 1 message", "Marked 1 message read"]);
  });

  it("sends one call per account, in the order given", async () => {
    const { m } = await store();
    await m.setRead([row(1002), row(1015, "home"), row(1001)], false);
    expect(calls("message_set_read")).toEqual([
      { account: "work", row_ids: [1002, 1001], read: false },
      { account: "home", row_ids: [1015], read: false },
    ]);
  });

  it("fails a whole batch for an unknown account", async () => {
    const { get, m } = await store();
    expect(await m.archive([row(1, "nobody")])).toEqual({ done: 0, failed: 1 });
    expect(get().activity[0].rows[0].reason).toMatch(/account_unknown/);
  });

  it("discards drafts, and puts back an approved one", async () => {
    const { get, m } = await store("drafts");
    mock.drafts.work.drafts[1].status = "approved";
    expect(await m.discardDrafts("work", ["angebot-antwort", "offsite-note"])).toEqual({ done: 1, failed: 1 });
    const list = get().messages.data as Extract<MessageList, { kind: "drafts" }>;
    expect(list.listing.drafts.map((d) => d.id)).toEqual(["offsite-note"]);
    expect(get().activity.map((n) => n.kind)).toEqual(["applied", "failed"]);
  });
});

describe("holds and syncs", () => {
  it("cancels a hold, and reports one the daemon no longer runs", async () => {
    const { get, m } = await store();
    await m.cancelHold("fixture-hold-seed");
    expect(get().holds["fixture-hold-seed"]).toMatchObject({ state: "cancelled", cancelling: false });
    expect(mock.holds).toEqual([]);
    await m.cancelHold("fixture-hold-seed");
    expect(get().activity.map((n) => n.kind)).toEqual(["hold_cancelled"]);
    await m.cancelHold("gone");
    expect(get().activity.map((n) => n.kind)).toEqual(["hold_cancelled", "hold_cancel_failed"]);
    expect((await cmd.sendHoldStatus()).holds).toEqual([]);
  });

  it("starts a sync and awaits its id, and reports one that did not start", async () => {
    const { get, m } = await store();
    await m.sync("work", "quick");
    expect(get().syncs).toEqual({ "fixture-op-1": { account: "work", mode: "quick" } });
    expect(get().syncStarting).toBe(0);
    await m.sync("nobody", "full");
    expect(get().activity[0]).toMatchObject({ kind: "sync_failed", account: "nobody" });
  });
});
