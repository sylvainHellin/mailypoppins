// The conversation list's model (ticket 0142, step 5): the visible sequence,
// the selection by thread, the generation guard, the expansions, the
// optimistic changes and the re-read on any change in the account.

import { describe, expect, it } from "vitest";
import { actionTargets, reducer, visibleItems, type Action } from "@/app/reducer";
import { overlayPending } from "@/app/pending";
import { conversationEntries, initialState, isStale, listKey, threadKey, type AppState } from "@/app/state";
import { CANNED_THREAD, conversationListing, fixtures, mailboxListing, resetMock, threadListing } from "@/test/tauri-mock";
import type { MessageList } from "@/lib/gui-types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);
type Conversations = Extract<MessageList, { kind: "conversations" }>;
const INBOX = listKey("work", "inbox");

/** The work inbox in conversation mode, its first answer landed. */
function grouped(): AppState {
  resetMock();
  let s = run(initialState(), { type: "conversation_mode", account: "work", on: true });
  s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap } });
  s = run(s, { type: "mailboxes_loaded", account: "work", gen: 1, listing: mailboxListing("work") });
  return run(s, { type: "messages_loaded", key: INBOX, gen: s.messages.gen, list: conversationListing("work", "inbox") });
}

const list = (s: AppState) => s.messages.data as Conversations;
const offsite = (s: AppState) => list(s).rows.find((r) => r.thread_id === CANNED_THREAD)!;

/** Expand the offsite thread and land its members. */
function expanded(s: AppState): AppState {
  s = run(s, { type: "conversation_expand", thread_id: CANNED_THREAD });
  const e = s.expanded[CANNED_THREAD];
  return run(s, { type: "thread_loaded", key: INBOX, thread_id: CANNED_THREAD, gen: e.gen, thread: threadListing("work", e.row_id, "inbox")! });
}

describe("the conversation list", () => {
  it("lists one row per conversation, the canned thread as one row of three with its draft and its later reply's date", () => {
    let s = grouped();
    expect(list(s).kind).toBe("conversations");
    expect(list(s).total).toBe(8);
    const row = offsite(s);
    expect(row).toMatchObject({ count: 3, has_draft: true, member_ids: [1007], date_sort: "2026-09-27T09:20:00", mailboxes: ["inbox", "sent"] });
    // The cursor goes on the top row, a header held by its thread.
    s = run(s, { type: "move_selection", to: "first", relative: false });
    expect(s.selection.conversation).toEqual({ thread_id: list(s).rows[0].thread_id, member: null });
    expect(s.selection.message?.row_id).toBe(list(s).rows[0].latest.id);
  });

  it("expands to four members oldest first under the header, and collapses back to it", () => {
    let s = grouped();
    s = expanded(s);
    const entries = conversationEntries(s.messages.data, s.expanded, "");
    const at = entries.findIndex((e) => e.kind === "header" && e.row.thread_id === CANNED_THREAD);
    const members = entries.slice(at + 1, at + 5).map((e) => (e.kind === "member" ? [e.member.mailbox, e.member.draft_id ?? e.member.id] : null));
    expect(members).toEqual([
      ["inbox", 1007],
      ["sent", 1010],
      ["sent", 1000],
      ["drafts", "offsite-note"],
    ]);
    // The cursor on the draft member, then a collapse puts it on the header.
    s = run(s, { type: "select_conversation", thread_id: CANNED_THREAD, member: "draft:offsite-note" });
    expect(s.selection).toMatchObject({ draft: "offsite-note", message: null });
    s = run(s, { type: "conversation_collapse", thread_id: CANNED_THREAD });
    expect(s.selection.conversation).toEqual({ thread_id: CANNED_THREAD, member: null });
    expect(s.selection.message?.row_id).toBe(1007);
    expect(s.selection.draft).toBeNull();
  });

  it("acts on a header's members here, a member alone, and the union of marked rows once each", () => {
    let s = expanded(grouped());
    s = run(s, { type: "select_conversation", thread_id: CANNED_THREAD, member: null });
    expect(actionTargets(s)).toEqual([{ account: "work", row_id: 1007 }]);
    s = run(s, { type: "select_conversation", thread_id: CANNED_THREAD, member: "row:1010" });
    expect(actionTargets(s)).toEqual([{ account: "work", row_id: 1010 }]);
    s = run(s, { type: "select_conversation", thread_id: CANNED_THREAD, member: "draft:offsite-note" });
    expect(actionTargets(s)).toEqual([{ account: "work", draft: "offsite-note" }]);

    const lone = list(s).rows[0];
    s = run(
      s,
      { type: "mark_toggle", key: threadKey("work", CANNED_THREAD) },
      { type: "mark_toggle", key: "work#1007" },
      { type: "mark_toggle", key: threadKey("work", lone.thread_id) },
    );
    expect(actionTargets(s)).toEqual([
      { account: "work", row_id: lone.latest.id },
      { account: "work", row_id: 1007 },
    ]);
    // Unmarking the rows a batch acted on unmarks the headers that stood for them.
    s = run(s, { type: "mark_set", keys: ["work#1007", `work#${lone.latest.id}`], on: false });
    expect(s.marked.keys.size).toBe(0);
  });

  it("drops an answer of the other mode and one older than the list's last change", () => {
    let s = grouped();
    const flat: MessageList = { kind: "messages", account: "work", mailbox: "inbox", total: 0, rows: [] };
    s = { ...s, messages: { ...s.messages, gen: s.messages.gen + 1 } };
    const after = run(s, { type: "messages_loaded", key: INBOX, gen: s.messages.gen, list: flat });
    expect(after.messages.data?.kind).toBe("conversations");
    // A conversation answer asked before an optimistic change is dropped.
    s = run(s, { type: "mutation_apply", batch: 1, kind: "read", targets: [{ account: "work", row_id: 1002 }], value: true });
    const stale = run(s, { type: "messages_loaded", key: INBOX, gen: s.messages.gen, lgen: 0, list: conversationListing("work", "inbox") });
    expect(stale.messages.loadedGen).toBe(s.messages.loadedGen);
  });

  it("switches an account's list to conversations and back, finding the selected message again", () => {
    resetMock();
    let s = run(initialState(), { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap } });
    s = run(s, { type: "mailboxes_loaded", account: "work", gen: 1, listing: mailboxListing("work") });
    const rows = (fixtures.messages.work.inbox as Conversations["rows"][number]["latest"][]).map((r) => ({ ...r }));
    s = run(s, { type: "messages_loaded", key: INBOX, gen: s.messages.gen, list: { kind: "messages", account: "work", mailbox: "inbox", total: rows.length, rows } });
    const r = rows.find((x) => x.id === 1007)!;
    s = run(s, { type: "select_message", message: { row_id: r.id, message_id: r.message_id, selector: r.selector } });
    s = run(s, { type: "conversation_mode", account: "work", on: true });
    expect(s.messages.data).toBeNull();
    expect(isStale(s.messages)).toBe(true);
    s = run(s, { type: "messages_loaded", key: INBOX, gen: s.messages.gen, list: conversationListing("work", "inbox") });
    expect(s.selection.conversation).toEqual({ thread_id: CANNED_THREAD, member: null });
    expect(s.selection.message).toMatchObject({ row_id: 1007, verified: true });
  });

  it("re-reads the conversation list on a change anywhere in the account, and the flat list only on its own mailbox", () => {
    const s = expanded(grouped());
    const event = (kind: string, payload: unknown): Action => ({
      type: "gui_event",
      event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision: 9000, kind, payload } },
    });
    const sent = event("state.invalidate", { resource: "mailbox:work/sent", scope: { query: "counts" } });
    expect(isStale(run(s, sent).messages)).toBe(true);
    const draft = event("draft.changed", { account: "work", id: "offsite-note" });
    expect(isStale(run(s, draft).messages)).toBe(true);
    const removed = event("state.remove", { resource: "draft:work/offsite-note" });
    expect(isStale(run(s, removed).messages)).toBe(true);
    const home = event("state.invalidate", { resource: "mailbox:home/inbox", scope: { query: "counts" } });
    expect(isStale(run(s, home).messages)).toBe(false);

    const flat = run(s, { type: "conversation_mode", account: "work", on: false });
    const loaded = run(flat, { type: "messages_loaded", key: INBOX, gen: flat.messages.gen, list: { kind: "messages", account: "work", mailbox: "inbox", total: 0, rows: [] } });
    expect(isStale(run(loaded, sent).messages)).toBe(false);

    // A reload reads every expansion again, from the row's latest.
    const reloaded = run(run(s, sent), { type: "messages_loaded", key: INBOX, gen: run(s, sent).messages.gen, list: conversationListing("work", "inbox") });
    expect(isStale(reloaded.expanded[CANNED_THREAD])).toBe(true);
    expect(reloaded.expanded[CANNED_THREAD].row_id).toBe(1007);
  });

  it("keeps a member cursor across a renumbering by its Message-ID, and falls back to the header when it is gone", () => {
    let s = expanded(grouped());
    s = run(s, { type: "select_conversation", thread_id: CANNED_THREAD, member: "row:1000" });
    // Another daemon instance renumbered the rows: the member keeps its Message-ID.
    const shifted = threadListing("work", 1007, "inbox")!;
    shifted.messages = shifted.messages.map((m) => (m.draft_id ? m : { ...m, id: m.id + 500 }));
    s = run(s, { type: "messages_loaded", key: INBOX, gen: s.messages.gen + 1, list: conversationListing("work", "inbox") });
    let e = s.expanded[CANNED_THREAD];
    s = run(s, { type: "thread_loaded", key: INBOX, thread_id: CANNED_THREAD, gen: e.gen, thread: shifted });
    expect(s.selection.conversation).toEqual({ thread_id: CANNED_THREAD, member: "row:1500" });
    expect(s.selection.message).toMatchObject({ row_id: 1500, verified: true });

    s = { ...s, expanded: { ...s.expanded, [CANNED_THREAD]: { ...s.expanded[CANNED_THREAD], gen: s.expanded[CANNED_THREAD].gen + 1 } } };
    e = s.expanded[CANNED_THREAD];
    const without = { ...shifted, messages: shifted.messages.filter((m) => m.id !== 1500) };
    s = run(s, { type: "thread_loaded", key: INBOX, thread_id: CANNED_THREAD, gen: e.gen, thread: without });
    expect(s.selection.conversation).toEqual({ thread_id: CANNED_THREAD, member: null });
  });

  it("applies a read and an archive to the row at once, and a reload keeps them until they settle", () => {
    let s = grouped();
    const angebot = list(s).rows.find((r) => r.latest.id === 1002)!;
    expect(angebot.unread).toBe(1);
    s = run(s, { type: "mutation_apply", batch: 1, kind: "read", targets: [{ account: "work", row_id: 1002 }], value: true });
    expect(list(s).rows.find((r) => r.latest.id === 1002)).toMatchObject({ unread: 0, latest: { flags: { seen: true } } });

    s = run(s, { type: "mutation_apply", batch: 2, kind: "archive", targets: [{ account: "work", row_id: 1007 }] });
    expect(list(s).rows.some((r) => r.thread_id === CANNED_THREAD)).toBe(false);
    expect(list(s).total).toBe(7);
    // The pending changes overlay a reload that does not have them yet.
    const fresh = overlayPending(s, conversationListing("work", "inbox")) as Conversations;
    expect(fresh.rows.some((r) => r.thread_id === CANNED_THREAD)).toBe(false);
    expect(fresh.rows.find((r) => r.latest.id === 1002)?.unread).toBe(0);
    // A refusal reads the conversation list again rather than guessing.
    s = run(s, {
      type: "mutation_settled",
      batch: 2,
      kind: "archive",
      account: "work",
      done: [],
      failed: [{ target: { account: "work", row_id: 1007 }, reason: "refused" }],
    });
    expect(isStale(s.messages)).toBe(true);
  });

  it("moves the cursor off a conversation that left the list", () => {
    let s = grouped();
    s = run(s, { type: "select_conversation", thread_id: CANNED_THREAD, member: null });
    const at = visibleItems(s).findIndex((it) => it.kind === "conversation" && it.thread_id === CANNED_THREAD);
    const next = visibleItems(s)[at + 1];
    s = run(s, { type: "mutation_apply", batch: 1, kind: "archive", targets: [{ account: "work", row_id: 1007 }] });
    expect(next.kind === "conversation" && s.selection.conversation?.thread_id === next.thread_id).toBe(true);
  });
});
