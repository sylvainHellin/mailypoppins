import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import {
  agendaRsvpRefusal,
  CANCELLED,
  NOT_REQUEST,
  ORGANIZER,
  rsvpRefusal,
  rsvpText,
  SUPERSEDED,
} from "@/app/rsvp";
import { initialState, isStale, readerKey, type AppState, type RsvpRun } from "@/app/state";
import { fixtures, GRAPH_RSVP_REFUSAL } from "@/test/tauri-mock";
import type { AgendaEvent, EventFrontmatter, OperationStatus } from "@/protocol/types";
import type { RsvpSettled } from "@/lib/gui-types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);
const last = (s: AppState) => s.activity[s.activity.length - 1];

const request = (patch: Partial<EventFrontmatter> = {}): EventFrontmatter => ({
  method: "REQUEST",
  sequence: 0,
  summary: "Steering committee",
  rsvp: "needs-action",
  recurrence: "",
  ...patch,
});

function booted(): AppState {
  return run(initialState(), {
    type: "gui_event",
    event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap },
  });
}

function envelope(kind: string, payload: unknown, revision = 900): Action {
  return { type: "gui_event", event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind, payload } } };
}

function settled(response = "accept", delivered = true): RsvpSettled {
  return {
    account: "work",
    selector: "mp://work/inbox/x",
    response,
    subject: "Accepted: Steering committee",
    organizer: "chair@example.com",
    message_id: "<r@x>",
    delivered,
  };
}

/** `work`'s agenda and row 1008's card, both read. */
function withAgendaAndCard(): AppState {
  let s = run(booted(), { type: "switch_view", view: "calendar" });
  s = run(s, { type: "calendar_loaded", account: "work", gen: s.calendar.work.gen, events: fixtures.calendar.events.work });
  s = { ...s, invites: { [readerKey("work", 1008)]: { data: request(), gen: 1, loadedGen: 0, error: null } } };
  return run(s, { type: "invite_loaded", key: readerKey("work", 1008), gen: 1, event: request() });
}

const requested = (token = 1): Action => ({ type: "rsvp_requested", token, account: "work", row_id: 1008, response: "accept", summary: "Steering committee" });

describe("why an invitation cannot be answered", () => {
  it("names the reasons in the TUI's order and words: not a REQUEST, cancelled, superseded, the user's own, then Graph", () => {
    const graph = GRAPH_RSVP_REFUSAL;
    expect(rsvpRefusal(request({ method: "CANCEL", cancelled: true, superseded: true }), true, graph)).toBe(NOT_REQUEST);
    expect(rsvpRefusal(null, false, null)).toBe(NOT_REQUEST);
    expect(rsvpRefusal(request({ cancelled: true, superseded: true }), true, graph)).toBe(CANCELLED);
    expect(rsvpRefusal(request({ superseded: true }), true, graph)).toBe(SUPERSEDED);
    expect(rsvpRefusal(request(), true, graph)).toBe(ORGANIZER);
    expect(rsvpRefusal(request(), false, graph)).toBe(graph);
    expect(rsvpRefusal(request({ method: "request" }), false, null)).toBeNull();
    expect(NOT_REQUEST).toBe("Only received invitations (REQUEST) can be RSVP'd");
    expect(ORGANIZER).toBe("You are the organizer of this invite; nothing to RSVP");
    for (const text of [CANCELLED, SUPERSEDED, ORGANIZER]) expect(text).not.toMatch(/\u2014/);
  });

  it("reads an agenda row in the TUI's agenda order: cancelled, organizer, not a REQUEST", () => {
    const row = (patch: Partial<AgendaEvent>, event: Partial<EventFrontmatter> = {}): AgendaEvent => ({
      row_id: 1,
      event: request(event),
      subject: "x",
      start_sort: "",
      end_sort: "",
      start_display: "",
      is_organizer: false,
      cancelled: false,
      ...patch,
    });
    expect(agendaRsvpRefusal(row({ cancelled: true, is_organizer: true }, { method: "CANCEL" }), null)).toBe(CANCELLED);
    expect(agendaRsvpRefusal(row({ is_organizer: true }, { method: "CANCEL" }), null)).toBe(ORGANIZER);
    expect(agendaRsvpRefusal(row({}, { method: "PUBLISH" }), null)).toBe(NOT_REQUEST);
    expect(agendaRsvpRefusal(row({}), "graph")).toBe("graph");
    expect(agendaRsvpRefusal(row({}), null)).toBeNull();
  });
});

describe("an RSVP this window started", () => {
  it("settles with the TUI-style line, and a reply no one took yet says it waits in the outbox", () => {
    const r: RsvpRun = { token: 1, account: "work", row_id: 1008, response: "tentative", summary: "Steering committee", operation_id: "op" };
    expect(rsvpText(r, settled("tentative"))).toBe("Replied tentative to Steering committee");
    expect(rsvpText(r, settled("tentative", false))).toBe("Replied tentative to Steering committee; queued in the outbox");
  });

  it("closes the choice, awaits its id, then says so and reads the agenda and the account's cards again", () => {
    let s = run(withAgendaAndCard(), { type: "open_rsvp", dialog: { account: "work", row_id: 1008, summary: "Steering committee" } });
    expect(s.overlay).toBe("rsvp");
    s = run(s, requested(), { type: "rsvp_started", token: 1, operation_id: "op-r" });
    expect(s.overlay).toBeNull();
    expect(s.rsvpDialog).toBeNull();
    expect(s.rsvps.map((r) => r.operation_id)).toEqual(["op-r"]);
    expect(isStale(s.calendar.work)).toBe(false);
    s = run(s, envelope("operation.finished", { operation_id: "op-r", state: "succeeded", result: settled() }));
    expect(s.rsvps).toEqual([]);
    expect(last(s)).toMatchObject({ kind: "applied", account: "work", text: "Replied accept to Steering committee" });
    expect(isStale(s.calendar.work)).toBe(true);
    expect(isStale(s.invites[readerKey("work", 1008)])).toBe(true);
  });

  it("keeps an end that overtook the start's answer, and settles on the id", () => {
    let s = run(withAgendaAndCard(), requested());
    s = run(s, envelope("operation.finished", { operation_id: "op-early", state: "succeeded", result: settled("accept", false) }));
    expect(s.rsvpEarly).toHaveLength(1);
    s = run(s, { type: "rsvp_started", token: 1, operation_id: "op-early" });
    expect(s.rsvps).toEqual([]);
    expect(s.rsvpEarly).toEqual([]);
    expect(last(s).text).toBe("Replied accept to Steering committee; queued in the outbox");
    expect(isStale(s.accounts)).toBe(true);
  });

  it("says a failed, a dropped and a refused RSVP in their own words", () => {
    let s = run(withAgendaAndCard(), requested(1), { type: "rsvp_started", token: 1, operation_id: "op-f" });
    s = run(s, envelope("operation.finished", { operation_id: "op-f", state: "failed", error: { code: -32603, message: "421 closed" } }));
    expect(last(s)).toMatchObject({ kind: "send_failed", text: "RSVP failed: 421 closed" });
    s = run(s, requested(2), { type: "rsvp_started", token: 2, operation_id: "op-d" });
    s = run(s, { type: "gui_event", event: { type: "operation_dropped", operation_id: "op-d", kind: "rsvp", reason: "gone" } });
    expect(last(s).text).toBe("The RSVP to Steering committee was interrupted; check the outbox");
    s = run(s, requested(3), { type: "rsvp_failed", token: 3, error: { kind: "protocol", message: "no such row", code: -32602 } });
    expect(last(s).text).toBe("RSVP failed: no such row");
    expect(s.rsvps).toEqual([]);
  });

  it("settles through the Rust layer's re-query as `rsvp`", () => {
    let s = run(withAgendaAndCard(), requested(), { type: "rsvp_started", token: 1, operation_id: "op-q" });
    const status: OperationStatus = {
      operation_id: "op-q",
      method: "calendar.rsvp",
      state: "succeeded",
      scope: "durable",
      progress: null,
      result: settled("decline"),
      error: null,
    } as unknown as OperationStatus;
    s = run(s, { type: "gui_event", event: { type: "operation_settled", operation_id: "op-q", kind: "rsvp", status } });
    expect(last(s).text).toBe("Replied decline to Steering committee");
  });
});

describe("the cards and the refusals", () => {
  it("a card goes stale with its account's mail and sync, never with another account's", () => {
    let s = withAgendaAndCard();
    const key = readerKey("work", 1008);
    s = run(s, envelope("state.invalidate", { resource: "mailbox:home/inbox", scope: { query: "counts" } }));
    expect(isStale(s.invites[key])).toBe(false);
    s = run(s, envelope("state.invalidate", { resource: "mailbox:work/inbox", scope: { query: "counts" } }));
    expect(isStale(s.invites[key])).toBe(true);
  });

  it("another daemon instance forgets the cards and the Graph refusals", () => {
    let s = run(withAgendaAndCard(), { type: "invite_refusal_loaded", account: "home", refusal: GRAPH_RSVP_REFUSAL });
    expect(s.inviteRefusals).toEqual({ home: GRAPH_RSVP_REFUSAL });
    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "reconnected", bootstrap: { ...fixtures.bootstrap, instance_id: "fixture-instance-2" } } });
    expect(s.invites).toEqual({});
    expect(s.inviteRefusals).toEqual({});
  });
});
