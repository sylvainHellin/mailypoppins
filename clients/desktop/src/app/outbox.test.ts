import { beforeEach, describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { cursorRow, hiddenByOutbox, outboxRows, outboxSummary, queueDepth, queueText, retryDialog, retryResult, discardDialog } from "@/app/outbox";
import { initialState, isStale, type AppState } from "@/app/state";
import { fixtures, outboxListing, mock, resetMock } from "@/test/tauri-mock";
import { GUI_ENTRIES, paletteEntries, type ActionId } from "@/keymap/catalog";
import type { OperationStatus, OutboxListing, OutboxRow } from "@/protocol/types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

/** Every action the palette can run, once each. */
const paletteIds = (): ActionId[] => [...new Set([...paletteEntries(), ...GUI_ENTRIES].flatMap((e) => (e.id ? [e.id] : [])))];

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

function listing(account: string, rows: OutboxRow[]): OutboxListing {
  mock.outbox[account] = { ever_used: true, rows };
  return outboxListing(account);
}

function booted(): AppState {
  return run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

/** `work`'s outbox open and loaded with `rows`. */
function opened(rows: OutboxRow[]): AppState {
  const s = run(booted(), { type: "open_outbox", account: "work" });
  return run(s, { type: "outbox_loaded", account: "work", gen: s.outbox.work.gen, listing: listing("work", rows) });
}

function invalidate(resource: string, revision = 700): Action {
  return {
    type: "gui_event",
    event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind: "state.invalidate", payload: { resource, scope: { query: "counts" } } } },
  };
}

function finished(operation_id: string, state: string, result: unknown, error: string | null = null): Action {
  return {
    type: "gui_event",
    event: {
      type: "event",
      event: {
        instance_id: fixtures.bootstrap.instance_id,
        revision: 800,
        kind: "operation.finished",
        payload: { operation_id, state, result, error: error ? { code: -32603, message: error } : null },
      },
    },
  };
}

const last = (s: AppState) => s.activity[s.activity.length - 1];

beforeEach(() => resetMock());

describe("the outbox Loadable", () => {
  it("is created on the first open, loads, and the view focuses the list pane", () => {
    let s = run(booted(), { type: "focus", pane: "sidebar" }, { type: "open_outbox", account: "home" });
    expect(s.outboxView).toEqual({ account: "home", cursor: null });
    expect(s.focus).toBe("list");
    expect(isStale(s.outbox.home)).toBe(true);
    s = run(s, { type: "outbox_loaded", account: "home", gen: s.outbox.home.gen, listing: outboxListing("home") });
    expect(isStale(s.outbox.home)).toBe(false);
    expect(cursorRow(s)?.id).toBe(1);
    expect(outboxSummary(s, "home")).toEqual({ queued: 1, failed: 0, partial: 0 });
  });

  it("goes stale on an invalidation of its account, and is created by one when never read", () => {
    let s = opened([row(5)]);
    s = run(s, invalidate("outbox:work"));
    expect(isStale(s.outbox.work)).toBe(true);
    expect(s.outbox.home).toBeUndefined();
    s = run(s, invalidate("outbox:home", 701));
    expect(isStale(s.outbox.home)).toBe(true);
    expect(s.outbox.home.data).toBeNull();
  });

  it("goes stale on every bootstrap, and a failed read keeps what it had", () => {
    let s = opened([row(5)]);
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
    expect(isStale(s.outbox.work)).toBe(true);
    expect(s.outboxView?.account).toBe("work");
    s = run(s, { type: "outbox_failed", account: "work", gen: s.outbox.work.gen, error: { kind: "timeout", message: "slow" } });
    expect(s.outbox.work.error?.message).toBe("slow");
    expect(s.outbox.work.data?.rows).toHaveLength(1);
  });

  it("feeds the sidebar once loaded, where the bootstrap has no partial count", () => {
    let s = booted();
    expect(outboxSummary(s, "work")).toEqual({ queued: 0, failed: 0, partial: 0 });
    s = run(s, invalidate("outbox:work"));
    s = run(s, { type: "outbox_loaded", account: "work", gen: s.outbox.work.gen, listing: listing("work", [row(5), row(6, { state: "done", partial: true })]) });
    expect(outboxSummary(s, "work")).toEqual({ queued: 0, failed: 1, partial: 1 });
  });
});

describe("the outbox view", () => {
  it("moves its cursor with the list's moves, and the mailbox intents close it", () => {
    let s = opened([row(5), row(6), row(7)]);
    s = run(s, { type: "move_selection", to: 1, relative: true });
    expect(cursorRow(s)?.id).toBe(6);
    s = run(s, { type: "move_selection", to: "last", relative: false });
    expect(cursorRow(s)?.id).toBe(7);
    s = run(s, { type: "move_selection", to: 5, relative: true });
    expect(cursorRow(s)?.id).toBe(7);
    const selection = s.selection;
    expect(run(s, { type: "clear_selection" }).outboxView).toBeNull();
    expect(run(s, { type: "close_outbox" }).outboxView).toBeNull();
    const back = run(s, { type: "select_mailbox", account: "work", slug: "inbox" });
    expect(back.outboxView).toBeNull();
    expect(back.selection).toEqual(selection);
    expect(run(s, { type: "search_local", query: "x" }).outboxView).toBeNull();
  });
});

describe("retry and discard", () => {
  it("a retry is pending until its operation ends, then says how and reads the outbox again", () => {
    let s = opened([row(5)]);
    s = run(s, { type: "outbox_action_requested", token: 1, kind: "retry", account: "work", row_id: 5 });
    expect(queueDepth(s).sending).toBe(1);
    s = run(s, { type: "outbox_retry_started", token: 1, operation_id: "op-r" });
    s = run(s, { type: "outbox_loaded", account: "work", gen: s.outbox.work.gen, listing: s.outbox.work.data! });
    s = run(s, finished("op-r", "succeeded", { row_id: 5, state: null, completed: 1 }));
    expect(s.outboxActions).toEqual([]);
    expect(isStale(s.outbox.work)).toBe(true);
    expect(last(s)).toMatchObject({ kind: "applied", text: "Outbox row 5 sent; its Sent copy is filed" });
  });

  it("an end that overtakes the retry's answer is held for it", () => {
    let s = opened([row(5)]);
    s = run(s, { type: "outbox_action_requested", token: 1, kind: "retry", account: "work", row_id: 5 });
    s = run(s, finished("op-r", "succeeded", { row_id: 5, state: "failed", completed: 0 }));
    expect(s.outboxEarly).toHaveLength(1);
    s = run(s, { type: "outbox_retry_started", token: 1, operation_id: "op-r" });
    expect(s.outboxEarly).toEqual([]);
    expect(last(s)).toMatchObject({ kind: "send_failed", text: "Outbox row 5 failed again; the outbox says why" });
  });

  it("a settle or a drop after a restart ends the retry", () => {
    const status = (state: string): OperationStatus =>
      ({ operation_id: "op-r", method: "send.outbox_retry", state, scope: "durable", progress: null, result: { row_id: 5, state: "done", completed: 1 }, error: null }) as OperationStatus;
    let s = opened([row(5)]);
    s = run(s, { type: "outbox_action_requested", token: 1, kind: "retry", account: "work", row_id: 5 }, { type: "outbox_retry_started", token: 1, operation_id: "op-r" });
    const settled = run(s, { type: "gui_event", event: { type: "operation_settled", operation_id: "op-r", kind: "outbox_retry", status: status("succeeded") } });
    expect(last(settled)).toMatchObject({ kind: "send_partial", text: "Outbox row 5 partly delivered; the outbox names who never got it" });
    const dropped = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "op-r", kind: "outbox_retry", reason: "gone" } });
    expect(last(dropped)).toMatchObject({ kind: "send_failed", text: "The retry of outbox row 5 was interrupted; check the outbox" });
    const failed = run(s, finished("op-r", "failed", null, "no credential"));
    expect(last(failed).text).toBe("The retry of outbox row 5 failed: no credential");
  });

  it("a refused retry frees the row and says why", () => {
    let s = opened([row(5)]);
    s = run(s, { type: "outbox_action_requested", token: 1, kind: "retry", account: "work", row_id: 5 });
    s = run(s, { type: "outbox_action_failed", token: 1, error: { kind: "protocol", code: -32602, message: "outbox row 5 is pending_send" } });
    expect(s.outboxActions).toEqual([]);
    expect(last(s)).toMatchObject({ kind: "send_failed", text: "The retry of outbox row 5 was refused: outbox row 5 is pending_send" });
  });

  it("a discard hides the row at once and moves the cursor, and a refusal puts it back", () => {
    let s = opened([row(5), row(6), row(7, { state: "done", partial: true })]);
    s = run(s, { type: "outbox_select", row_id: 6 });
    s = run(s, { type: "outbox_action_requested", token: 2, kind: "discard", account: "work", row_id: 6 });
    expect(outboxRows(s, "work").map((r) => r.id)).toEqual([5, 7]);
    expect(cursorRow(s)?.id).toBe(7);
    expect(outboxSummary(s, "work")).toEqual({ queued: 0, failed: 1, partial: 1 });
    const refused = run(s, { type: "outbox_action_failed", token: 2, error: { kind: "not_found", message: "no outbox row 6", code: -32602 } });
    expect(outboxRows(refused, "work").map((r) => r.id)).toEqual([5, 6, 7]);
    expect(last(refused).text).toBe("The discard of outbox row 6 was refused: no outbox row 6");
    const done = run(s, { type: "outbox_discarded", token: 2, message_id: "<row-6@example.com>" });
    expect(done.outbox.work.data?.rows.map((r) => r.id)).toEqual([5, 7]);
    expect(done.outbox.work.data?.counts).toEqual({ open: 0, failed: 1, partial: 1 });
    expect(isStale(done.outbox.work)).toBe(true);
    expect(last(done)).toMatchObject({ kind: "applied", text: "Discarded outbox row 6 (<row-6@example.com>)" });
  });
});

describe("the confirmations and the words", () => {
  it("retries only what the daemon admits, and warns about a second delivery", () => {
    const failed = retryDialog("work", row(5));
    expect(failed).toMatchObject({ kind: "outbox_retry", title: "Send row 5 again?" });
    expect((failed as { warning: string }).warning).toMatch(/may already have been delivered/);
    expect(retryDialog("work", row(6, { state: "sent_pending_append" }))).toMatchObject({ title: "File the Sent copy of row 6?", warning: null });
    expect(retryDialog("work", row(7, { state: "pending_send" }))).toMatch(/row 7 is queued/);
    expect(retryDialog("work", row(8, { state: "done", partial: true }))).toMatch(/row 8 is partly delivered/);
  });

  it("names what a discard gives up, for each state", () => {
    expect(discardDialog("work", row(5)).kind).toBe("outbox_discard");
    const warning = (r: OutboxRow) => (discardDialog("work", r) as { warning: string }).warning;
    expect(warning(row(5))).toMatch(/without a verdict, so it may already have been delivered/);
    expect(warning(row(5, { state: "pending_send", never_submitted: true }))).toMatch(/never submitted/);
    expect(warning(row(5, { state: "sent_pending_append" }))).toMatch(/delivered; discarding it gives up its copy in Sent/);
    expect(warning(row(5, { state: "done", partial: true }))).toMatch(/went to some recipients/);
  });

  it("says a retry's outcome in each of the row's ends", () => {
    expect(retryResult({ row_id: 5, state: "sent_pending_append", completed: 0 }).text).toBe("Outbox row 5 sent; its Sent copy is still owed");
    expect(retryResult({ row_id: 5, state: "pending_send", completed: 0 }).text).toBe("Outbox row 5 is queued again; the next sync sends it");
    expect(retryResult({ row_id: 5, state: null, completed: 0 }).text).toBe("Outbox row 5 sent");
  });

  it("counts the queue depth from the outbox, the sends and the pending changes", () => {
    let s = booted();
    expect(queueDepth(s)).toEqual({ outbox: 1, sending: 0, changes: 0, total: 1 });
    expect(queueText(queueDepth(s))).toBe("1 waiting for the server: 1 in the outbox");
    s = run(s, { type: "send_requested", token: 1, kind: "draft", account: "work", drafts: ["d"], subject: null });
    s = run(s, { type: "mutation_apply", batch: 1, kind: "flag", targets: [{ account: "work", row_id: 1001 }], value: true });
    expect(queueText(queueDepth(s))).toBe("3 waiting for the server: 1 in the outbox, 1 sending, 1 change");
  });

  it("counts a retry of a row the listing already counts as open once, and a failed row's retry as sending", () => {
    let s = opened([row(5), row(6, { state: "sent_pending_append", last_error: null, outstanding: [] })]);
    const before = queueDepth(s);
    expect(before.sending).toBe(0);
    s = run(s, { type: "outbox_action_requested", token: 1, kind: "retry", account: "work", row_id: 6 });
    expect(queueDepth(s)).toEqual({ ...before, sending: 0, total: before.total });
    s = run(s, { type: "outbox_action_requested", token: 2, kind: "retry", account: "work", row_id: 5 });
    expect(queueDepth(s)).toEqual({ ...before, sending: 1, total: before.total + 1 });
  });
});

describe("what the outbox view hides", () => {
  it("hides every action on the mailbox selection and none of the others", () => {
    const shut = initialState();
    const open: AppState = { ...shut, outboxView: { account: "work", cursor: null } };
    const hidden = paletteIds().filter((id) => hiddenByOutbox(open, id));
    expect(hidden.sort()).toEqual(
      [
        "approve",
        "archive",
        "attach_file",
        "copy_link",
        "copy_selector",
        "copy_sender",
        "copy_subject",
        "delete",
        "demote",
        "edit_recipients",
        "fetch_hit",
        "forward",
        "mark_all",
        "mark_clear",
        "mark_range",
        "mark_toggle",
        "move",
        "open_attachment",
        "open_editor",
        "open_html",
        "reply",
        "reply_all",
        "rsvp",
        "save_attachment",
        "send",
        "send_all",
        "toggle_flag",
        "toggle_read",
      ].sort(),
    );
    // Enter's action, which has no palette row.
    expect(hiddenByOutbox(open, "open_message")).toBe(true);
    for (const id of [
      "new_draft",
      "focus_filter",
      "open_outbox",
      "outbox_retry",
      "outbox_discard",
      "clear_selection",
      "next_message",
      "toggle_activity",
      "activity_log",
      "open_config",
      "open_log",
    ] as const) {
      expect(hiddenByOutbox(open, id)).toBe(false);
    }
    expect(paletteIds().some((id) => hiddenByOutbox(shut, id))).toBe(false);
  });
});

