import { describe, expect, it } from "vitest";
import { reducer, type Action } from "@/app/reducer";
import { calendarRows, cursorEvent, nowSortKey, statusBadge, visibleEvents } from "@/app/calendar";
import { initialState, isStale, listKey, type AppState } from "@/app/state";
import { hiddenNotice, OPEN_CALENDAR_FIRST } from "@/app/views";
import { fixtures, mailboxListing } from "@/test/tauri-mock";
import type { AgendaEvent, MessageListRow } from "@/protocol/types";
import type { MessageList } from "@/lib/gui-types";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

function event(row_id: number, start_sort: string, end_sort: string, patch: Partial<AgendaEvent> = {}): AgendaEvent {
  return {
    row_id,
    event: { sequence: 0, rsvp: "needs-action", recurrence: "", summary: `Event ${row_id}` },
    subject: `Invitation ${row_id}`,
    start_sort,
    end_sort,
    start_display: start_sort ? start_sort.replace("T", " ").slice(0, 16) : "",
    is_organizer: false,
    cancelled: false,
    ...patch,
  };
}

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

/** The Calendar view over `work`, its agenda loaded from the fixture. */
function inCalendar(): AppState {
  let s = run(booted(), { type: "switch_view", view: "calendar" });
  s = run(s, { type: "calendar_loaded", account: "work", gen: s.calendar.work.gen, events: fixtures.calendar.events.work });
  return s;
}

const invalidate = (resource: string): Action => ({
  type: "gui_event",
  event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision: 99, kind: "state.invalidate", payload: { resource, scope: { query: "counts" } } } },
});

describe("the past/upcoming rule", () => {
  const now = "2026-09-30T12:00:00";
  const past = event(1, "2026-09-29T08:00:00", "2026-09-29T09:00:00");
  const running = event(2, "2026-09-30T11:00:00", "2026-09-30T13:00:00");
  const endless = event(3, "2026-09-30T11:59:59", "");
  const later = event(4, "2026-10-01T08:00:00", "2026-10-01T09:00:00");
  const undated = event(5, "", "");
  const all = [past, running, endless, later, undated];

  it("hides past events by default, keeps a running one until its end and an undated one always", () => {
    expect(visibleEvents(all, false, now).map((e) => e.row_id)).toEqual([2, 4, 5]);
  });

  it("takes the start as the horizon when the end is unknown", () => {
    expect(visibleEvents([endless], false, "2026-09-30T11:59:59").map((e) => e.row_id)).toEqual([3]);
    expect(visibleEvents([endless], false, "2026-09-30T12:00:00")).toEqual([]);
  });

  it("shows every row with the past included", () => {
    expect(visibleEvents(all, true, now).map((e) => e.row_id)).toEqual([1, 2, 3, 4, 5]);
  });

  it("spells now as the sort keys do, UTC to the second", () => {
    expect(nowSortKey(new Date("2026-09-30T10:15:42.123+02:00"))).toBe("2026-09-30T08:15:42");
  });

  it("badges cancelled first, then organizer, then the user's reply", () => {
    expect(statusBadge(event(1, "", "", { cancelled: true, is_organizer: true }))).toBe("cancelled");
    expect(statusBadge(event(1, "", "", { is_organizer: true }))).toBe("organizer");
    expect(statusBadge(event(1, "", ""))).toBe("no reply");
    const tentative = event(1, "", "");
    tentative.event.rsvp = "tentative";
    expect(statusBadge(tentative)).toBe("tentative");
  });
});

describe("the Calendar view's model", () => {
  it("opens the selection account's agenda, upcoming only, its cursor on the first row", () => {
    let s = run(booted(), { type: "switch_view", view: "calendar" });
    expect(s.calendarView).toEqual({ account: "work", cursor: null, showPast: false, refreshing: false });
    expect(isStale(s.calendar.work)).toBe(true);
    expect(s.calendar.home).toBeUndefined();
    s = run(s, { type: "calendar_loaded", account: "work", gen: s.calendar.work.gen, events: fixtures.calendar.events.work });
    expect(calendarRows(s).map((e) => e.row_id)).toEqual([1008, 9103, 9102, 9104]);
    expect(cursorEvent(s)?.row_id).toBe(1008);
  });

  it("moves its cursor with move_selection, clamped, and leaves the mail selection alone", () => {
    let s = inCalendar();
    const mail = s.selection;
    s = run(s, { type: "move_selection", to: 1, relative: true });
    expect(s.calendarView?.cursor).toBe(9103);
    s = run(s, { type: "move_selection", to: "last", relative: false });
    expect(s.calendarView?.cursor).toBe(9104);
    s = run(s, { type: "move_selection", to: 5, relative: true });
    expect(s.calendarView?.cursor).toBe(9104);
    s = run(s, { type: "move_selection", to: "first", relative: false });
    expect(s.calendarView?.cursor).toBe(1008);
    expect(s.selection).toBe(mail);
  });

  it("t shows the past with the TUI's status line and puts the cursor back on top", () => {
    let s = run(inCalendar(), { type: "move_selection", to: 2, relative: true }, { type: "calendar_toggle_past" });
    expect(s.notice).toBe("Calendar: showing all events");
    expect(calendarRows(s)).toHaveLength(5);
    expect(cursorEvent(s)?.row_id).toBe(9101);
    s = run(s, { type: "calendar_toggle_past" });
    expect(s.notice).toBe("Calendar: showing upcoming events");
    expect(calendarRows(s)).toHaveLength(4);
  });

  it("r reads the agenda again and says how many events it shows once it lands", () => {
    let s = run(inCalendar(), { type: "calendar_refresh" });
    expect(isStale(s.calendar.work)).toBe(true);
    expect(s.calendarView?.refreshing).toBe(true);
    s = run(s, { type: "calendar_loaded", account: "work", gen: s.calendar.work.gen, events: fixtures.calendar.events.work });
    expect(s.notice).toBe("Calendar refreshed (4 events)");
    expect(s.calendarView?.refreshing).toBe(false);
  });

  it("r that fails with rows shown keeps them and says so in the notice line", () => {
    let s = run(inCalendar(), { type: "calendar_refresh" });
    s = run(s, {
      type: "calendar_failed",
      account: "work",
      gen: s.calendar.work.gen,
      error: { kind: "timeout", message: "the agenda took too long" },
    });
    expect(s.notice).toBe("Calendar refresh failed: the agenda took too long");
    expect(s.calendarView?.refreshing).toBe(false);
    expect(calendarRows(s)).toHaveLength(4);
  });

  it("goes stale with its own account's mail and syncs only", () => {
    let s = run(inCalendar(), { type: "switch_view", view: "mail" }, { type: "select_account", account: "home" }, { type: "switch_view", view: "calendar" });
    expect(s.calendarView?.account).toBe("home");
    s = run(s, { type: "calendar_loaded", account: "home", gen: s.calendar.home.gen, events: fixtures.calendar.events.home });
    expect(isStale(s.calendar.work)).toBe(false);
    expect(isStale(s.calendar.home)).toBe(false);

    s = run(s, invalidate("mailbox:work/inbox"));
    expect(isStale(s.calendar.work)).toBe(true);
    expect(isStale(s.calendar.home)).toBe(false);
    s = run(s, { type: "calendar_loaded", account: "work", gen: s.calendar.work.gen, events: [] });

    s = run(s, invalidate("message:home/inbox/abc"));
    expect(isStale(s.calendar.home)).toBe(true);
    expect(isStale(s.calendar.work)).toBe(false);
    s = run(s, { type: "calendar_loaded", account: "home", gen: s.calendar.home.gen, events: [] });

    s = run(s, invalidate("outbox:work"), invalidate("draft:work/abc"));
    expect(isStale(s.calendar.work)).toBe(false);

    s = run(s, {
      type: "gui_event",
      event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision: 100, kind: "sync.completed", payload: { account: "home", mode: "quick", error: null } } },
    });
    expect(isStale(s.calendar.home)).toBe(true);
    expect(isStale(s.calendar.work)).toBe(false);

    s = run(s, { type: "gui_event", event: { type: "rebootstrapped", cause: "resync", bootstrap: fixtures.bootstrap } });
    expect(isStale(s.calendar.work)).toBe(true);
  });

  it("follows the selection's account while it shows, and carries the scope over", () => {
    let s = run(inCalendar(), { type: "calendar_toggle_past" }, { type: "next_account" });
    expect(s.view).toBe("calendar");
    expect(s.calendarView).toMatchObject({ account: "home", cursor: null, showPast: true });
    expect(isStale(s.calendar.home)).toBe(true);
  });

  it("answers its own actions outside the Calendar view with a notice", () => {
    const s = booted();
    expect(hiddenNotice(s, "calendar_toggle_past")).toBe(OPEN_CALENDAR_FIRST);
    expect(hiddenNotice(inCalendar(), "calendar_toggle_past")).toBeNull();
    expect(hiddenNotice(inCalendar(), "calendar_open_source")).toBeNull();
  });
});
