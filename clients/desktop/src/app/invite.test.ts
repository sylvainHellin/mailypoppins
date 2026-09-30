import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { inviteAccount, inviteProblem, inviteResult, NO_RECIPIENT, NO_START, NO_SUBJECT } from "@/app/invite";
import { initialState, isStale, type AppState } from "@/app/state";
import { fixtures } from "@/test/tauri-mock";
import type { OperationStatus, SendOutcome } from "@/protocol/types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);
const last = (s: AppState) => s.activity[s.activity.length - 1];

function booted(): AppState {
  return run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

function finished(operation_id: string, state: string, result: unknown, error: string | null = null): Action {
  return {
    type: "gui_event",
    event: {
      type: "event",
      event: {
        instance_id: fixtures.bootstrap.instance_id,
        revision: 900,
        kind: "operation.finished",
        payload: { operation_id, state, result, error: error ? { code: -32603, message: error } : null },
      },
    },
  };
}

function outcome(delivered: boolean[]): SendOutcome {
  return {
    account: "work",
    selector: null,
    message_id: "<i@x>",
    status_line: "sent + saved",
    recipients: delivered.map((d, i) => ({ address: `r${i}@example.com`, role: "To", delivered: d, error: d ? null : "550 no such mailbox" })),
    sent_copy: "filed",
    settle_error: null,
  };
}

/** The Calendar view over `work` with its agenda read, and the form open. */
function formOpen(): AppState {
  let s = run(booted(), { type: "switch_view", view: "calendar" });
  s = run(s, { type: "calendar_loaded", account: "work", gen: s.calendar.work.gen, events: fixtures.calendar.events.work });
  return run(s, { type: "open_invite", account: "work" });
}

const requested = (token = 1): Action => ({ type: "invite_send_requested", token, account: "work", subject: "Kick-off" });

describe("the New invitation form's own checks", () => {
  it("asks for a subject, a start and one recipient, in that order", () => {
    const f = { subject: " ", start: "", to: "", cc: "" };
    expect(inviteProblem(f)).toBe(NO_SUBJECT);
    expect(inviteProblem({ ...f, subject: "Kick-off" })).toBe(NO_START);
    expect(inviteProblem({ ...f, subject: "Kick-off", start: "2099-12-01T10:00", to: " , ; " })).toBe(NO_RECIPIENT);
    expect(inviteProblem({ ...f, subject: "Kick-off", start: "2099-12-01T10:00", cc: "kim@example.com" })).toBeNull();
  });

  it("sends from the Calendar view's account, else the selection's", () => {
    const s = booted();
    expect(inviteAccount(s)).toBe(s.selection.account);
    const calendar = { ...formOpen(), calendarView: { account: "home", cursor: null, showPast: false, refreshing: false } };
    expect(inviteAccount(calendar)).toBe("home");
  });
});

describe("an invitation this window sent", () => {
  it("closes the form once started, then says so and reads the agenda and the outbox counts again", () => {
    let s = formOpen();
    expect(s.overlay).toBe("invite");
    s = run(s, requested());
    expect(s.overlay).toBe("invite");
    s = run(s, { type: "invite_send_started", token: 1, operation_id: "op-i" });
    expect(s.overlay).toBeNull();
    expect(s.inviteDialog).toBeNull();
    s = run(s, finished("op-i", "succeeded", outcome([true, true])));
    expect(s.inviteSends).toEqual([]);
    expect(last(s)).toMatchObject({ kind: "applied", account: "work", text: "Sent the invitation Kick-off" });
    expect(isStale(s.calendar.work)).toBe(true);
    expect(isStale(s.accounts)).toBe(true);
  });

  it("names a partial and an undelivered invitation, a failure and an interruption", () => {
    expect(inviteResult("Kick-off", outcome([true, false]))).toEqual({
      kind: "send_partial",
      text: "The invitation Kick-off reached 1 of 2 recipients; the outbox names who never got it",
    });
    expect(inviteResult("Kick-off", outcome([false])).kind).toBe("send_failed");
    let s = run(formOpen(), requested(1), { type: "invite_send_started", token: 1, operation_id: "op-f" });
    s = run(s, finished("op-f", "failed", null, "421 closed"));
    expect(last(s)).toMatchObject({ kind: "send_failed", text: "The invitation Kick-off failed: 421 closed" });
    s = run(s, requested(2), { type: "invite_send_started", token: 2, operation_id: "op-d" });
    s = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "op-d", kind: "send_invite", reason: "gone" } });
    expect(last(s).text).toBe("The invitation Kick-off was interrupted; check the outbox");
  });

  it("keeps an end that overtook the answer, and a refused start keeps the form open with no notice", () => {
    let s = run(formOpen(), requested());
    s = run(s, finished("op-e", "succeeded", outcome([true])));
    expect(s.inviteSendEarly).toHaveLength(1);
    s = run(s, { type: "invite_send_started", token: 1, operation_id: "op-e" });
    expect(last(s).text).toBe("Sent the invitation Kick-off");
    expect(s.inviteSendEarly).toEqual([]);
    const before = s.activity.length;
    s = run(s, { type: "open_invite", account: "work" }, requested(2), { type: "invite_send_failed", token: 2 });
    expect(s.overlay).toBe("invite");
    expect(s.inviteSends).toEqual([]);
    expect(s.activity).toHaveLength(before);
  });

  it("settles through the Rust layer's re-query as `send_invite`", () => {
    let s = run(formOpen(), requested(), { type: "invite_send_started", token: 1, operation_id: "op-q" });
    const status = { operation_id: "op-q", state: "succeeded", result: outcome([true]), error: null } as unknown as OperationStatus;
    s = run(s, { type: "gui_event", event: { type: "operation_settled", operation_id: "op-q", kind: "send_invite", status } });
    expect(last(s).text).toBe("Sent the invitation Kick-off");
  });
});
