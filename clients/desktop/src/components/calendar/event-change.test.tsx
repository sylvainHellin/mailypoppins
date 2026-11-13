import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock, settleEventChange, settleEventCreate } from "@/test/tauri-mock";
import {
  changedFields,
  changedText,
  editDialogFor,
  editProblem,
  NOTHING_CHANGED,
  openEventScope,
  openEventSeriesForm,
  ORGANIZER_ONLY,
  proposalDialogFor,
  proposalRefusal,
  removeDialogFor,
  RESTARTED_REMOVE,
} from "@/app/eventChange";
import { NO_TITLE, openEventDialog } from "@/app/eventForm";
import type { CalendarEvent, CalendarInfo, CalendarRecurrence } from "@/protocol/types";
import { initialState, type EventChangeRun, type EventDialog } from "@/app/state";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const cursorEvent = () => document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-event-id");
const noticed = (text: string | RegExp) =>
  waitFor(() =>
    expect(
      [...document.querySelectorAll('[role="status"]')].some((e) => (typeof text === "string" ? e.textContent?.includes(text) : text.test(e.textContent ?? ""))),
    ).toBe(true),
  );

type User = ReturnType<typeof renderApp>["user"];

/** The Calendar view anchored on Wednesday 2099-10-14, the cursor on `event`. */
async function calendarOn(event: string, before?: () => void) {
  const r = renderApp(1400, before);
  await shellReady();
  await r.user.keyboard(" a");
  await screen.findByRole("region", { name: "Calendar" });
  await r.user.keyboard("gt");
  await waitFor(() => expect(document.activeElement).toHaveAttribute("aria-label", "Jump to date"));
  await r.user.keyboard("2099-10-14{Enter}");
  const list = await screen.findByRole("listbox", { name: "Agenda" });
  await within(list).findByRole("option", { name: /Design review/ });
  await moveTo(r.user, event);
  return r;
}

async function moveTo(user: User, event: string) {
  for (let i = 0; i < 6 && cursorEvent() !== event; i++) await user.keyboard("j");
  expect(cursorEvent()).toBe(event);
}

const calendars: CalendarInfo[] = [
  { account: "home", id: "cal", name: "Calendar", color: null, is_default: true, can_edit: true, visible: true, backend: "graph", features: ["write", "online-meeting"] },
  { account: "home", id: "ro", name: "Shared", color: null, is_default: false, can_edit: false, visible: true, backend: "graph", features: ["write"] },
];

function event(over: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    account: "home",
    calendar: "cal",
    id: "ev-1",
    uid: "u",
    kind: "single",
    summary: "Budget",
    time: { kind: "timed", start: "2099-10-14T08:00:00Z", end: "2099-10-14T09:00:00Z", zone: "Europe/Berlin" },
    location: "Room 1",
    attendees: [{ email: "ada@example.com", role: "required", status: "accepted" }],
    own_response: "organizer",
    reminder: 15,
    is_online_meeting: false,
    sensitivity: "normal",
    show_as: "busy",
    cancelled: false,
    is_organizer: true,
    allow_new_time_proposals: true,
    response_requested: true,
    ...over,
  };
}

describe("the event change model", () => {
  it("opens the form with the event's values and sends only what changed", () => {
    const dialog = editDialogFor(event(), calendars) as EventDialog;
    expect(dialog.fields).toMatchObject({ title: "Budget", start: "2099-10-14T10:00", end: "2099-10-14T11:00", location: "Room 1", to: "ada@example.com", reminder: "15" });
    expect(editProblem(dialog)).toBe(NOTHING_CHANGED);
    const moved: EventDialog = { ...dialog, fields: { ...dialog.fields, start: "2099-10-14T11:00", end: "2099-10-14T12:00", location: "" } };
    expect(editProblem(moved)).toBeNull();
    expect(changedFields(moved, "Europe/Berlin")).toEqual({
      start: "2099-10-14T11:00",
      end: "2099-10-14T12:00",
      all_day: false,
      zone: "Europe/Berlin",
      location: null,
    });
    const quiet: EventDialog = { ...dialog, fields: { ...dialog.fields, noReminder: true } };
    expect(changedFields(quiet, "Europe/Berlin")).toEqual({ reminder: null });
  });

  it("lets an attendee change the reminder and the show-as only", () => {
    const dialog = editDialogFor(event({ is_organizer: false, own_response: "accepted", organizer: { email: "kim@example.com" } }), calendars) as EventDialog;
    expect(dialog.edit?.organizer).toBe(false);
    expect(editProblem({ ...dialog, fields: { ...dialog.fields, reminder: "5" } })).toBeNull();
    const away: EventDialog = { ...dialog, fields: { ...dialog.fields, showAs: "oof" } };
    expect(editProblem(away)).toBeNull();
    expect(changedFields(away, "Europe/Berlin")).toEqual({ show_as: "oof" });
    expect(editProblem({ ...dialog, fields: { ...dialog.fields, title: "Mine now" } })).toBe(ORGANIZER_ONLY);
  });

  it("sends the show-as only when it changed", () => {
    const dialog = editDialogFor(event({ show_as: "tentative" }), calendars) as EventDialog;
    expect(dialog.fields.showAs).toBe("tentative");
    expect(changedFields({ ...dialog, fields: { ...dialog.fields, location: "Room 2" } }, "Europe/Berlin")).toEqual({ location: "Room 2" });
    expect(changedFields({ ...dialog, fields: { ...dialog.fields, showAs: "free" } }, "Europe/Berlin")).toEqual({ show_as: "free" });
  });

  it("saves an event without a title, and refuses clearing a title it had", () => {
    const untitled = editDialogFor(event({ summary: "", is_organizer: false, own_response: "accepted", organizer: { email: "kim@example.com" } }), calendars) as EventDialog;
    const reminded: EventDialog = { ...untitled, fields: { ...untitled.fields, reminder: "5" } };
    expect(editProblem(reminded)).toBeNull();
    expect(changedFields(reminded, "Europe/Berlin")).toEqual({ reminder: 5 });
    const own = editDialogFor(event(), calendars) as EventDialog;
    expect(editProblem({ ...own, fields: { ...own.fields, title: " ", location: "Room 2" } })).toBe(NO_TITLE);
  });

  it("cancels the organizer's meeting, deletes what tells nobody, and refuses the rest", () => {
    expect(removeDialogFor(event(), calendars)).toMatchObject({ cancel: true, attendees: 1 });
    expect(removeDialogFor(event({ attendees: [] }), calendars)).toMatchObject({ cancel: false });
    expect(removeDialogFor(event({ is_organizer: false, own_response: "accepted", cancelled: true, organizer: { email: "kim@example.com" } }), calendars)).toMatchObject({
      cancel: false,
    });
    expect(removeDialogFor(event({ is_organizer: false, own_response: "accepted", organizer: { email: "kim@example.com" } }), calendars)).toMatch(/someone else's meeting/);
    expect(removeDialogFor(event({ kind: "occurrence", series_master_id: "ev-series" }), calendars, "series")).toMatchObject({ cancel: true, scope: "series" });
    expect(removeDialogFor(event({ calendar: "ro" }), calendars)).toMatch(/takes no changes/);
    expect(
      editDialogFor(event({ id: "imip:7", is_organizer: false, own_response: "needs-action", organizer: { email: "kim@example.com" } }), calendars),
    ).toMatch(/can only be answered/);
  });
});

describe("an invitation mail the account organised (PERSO-204)", () => {
  const invitations: CalendarInfo[] = [
    ...calendars,
    { account: "home", id: "imip", name: "Invitations", color: null, is_default: false, can_edit: false, visible: true, backend: "none", features: [] },
  ];
  const sent = (over: Partial<CalendarEvent> = {}) => event({ id: "imip:7", calendar: "imip", reminder: undefined, ...over });

  it("opens the form and the cancel confirmation on it, which the daemon sends by mail", () => {
    const dialog = editDialogFor(sent(), invitations) as EventDialog;
    expect(dialog.edit).toMatchObject({ account: "home", event: "imip:7", organizer: true });
    const moved: EventDialog = { ...dialog, fields: { ...dialog.fields, location: "Room 2" } };
    expect(changedFields(moved, "Europe/Berlin")).toEqual({ location: "Room 2" });
    expect(removeDialogFor(sent(), invitations)).toMatchObject({ event: "imip:7", cancel: true, attendees: 1 });
  });

  it("cancels a repeating one rather than changing it, and says when it is cancelled already", () => {
    expect(editDialogFor(sent({ kind: "series-master" }), invitations)).toBe("mp cannot change a repeating invitation yet; Cancel meeting cancels Budget");
    expect(removeDialogFor(sent({ kind: "series-master" }), invitations)).toMatchObject({ cancel: true });
    const cancelled = "Budget is cancelled already; an invitation mail stays on the agenda";
    expect(editDialogFor(sent({ cancelled: true }), invitations)).toBe(cancelled);
    expect(removeDialogFor(sent({ cancelled: true }), invitations)).toBe(cancelled);
  });
});

describe("taking a proposed time (PERSO-255)", () => {
  it("opens the edit form at the proposal in this window's zone, which Save sends as the new time", () => {
    const slot = { start: "2099-10-15T12:00:00Z", end: "2099-10-15T13:30:00Z" };
    const dialog = proposalDialogFor(event(), calendars, slot);
    if (typeof dialog === "string") throw new Error(dialog);
    expect(dialog.fields.start).toBe("2099-10-15T14:00");
    expect(dialog.fields.end).toBe("2099-10-15T15:30");
    expect(changedFields(dialog, "Europe/Berlin")).toEqual({ start: "2099-10-15T14:00", end: "2099-10-15T15:30", all_day: false, zone: "Europe/Berlin" });
    const holiday = proposalDialogFor(event({ time: { kind: "all-day", start: "2099-10-14", end: "2099-10-15" } }), calendars, slot);
    if (typeof holiday === "string") throw new Error(holiday);
    expect(holiday.fields.allDay).toBe(false);
  });

  it("takes a proposal on an occurrence for it alone, with no chooser, and on a series master for the series", () => {
    const slot = { start: "2099-10-15T12:00:00Z", end: "2099-10-15T13:30:00Z" };
    const occurrence = event({ kind: "occurrence", series_master_id: "ev-series", recurrence: "Weekly on Wednesday" });
    expect(proposalRefusal(occurrence, calendars)).toBeNull();
    const one = proposalDialogFor(occurrence, calendars, slot);
    if (typeof one === "string") throw new Error(one);
    expect(one.edit?.scope).toBe("occurrence");
    expect(one.fields.repeat.series).toBe("Weekly on Wednesday");
    expect(changedFields(one, "Europe/Berlin")).toEqual({ start: "2099-10-15T14:00", end: "2099-10-15T15:30", all_day: false, zone: "Europe/Berlin" });
    const series = proposalDialogFor(event({ kind: "series-master", recurrence: "Weekly on Wednesday" }), calendars, slot);
    if (typeof series === "string") throw new Error(series);
    expect(series.edit?.scope).toBeNull();
    expect(changedFields(series, "Europe/Berlin")).toMatchObject({ start: "2099-10-15T14:00", end: "2099-10-15T15:30" });
  });

  it("refuses an attendee and a read-only calendar with the edit form's reasons", () => {
    expect(proposalRefusal(event(), calendars)).toBeNull();
    expect(proposalRefusal(event({ own_response: "accepted", is_organizer: false }), calendars)).toBe("Only the organizer takes a proposed time");
    expect(proposalRefusal(event({ calendar: "ro" }), calendars)).toBe("The calendar Shared takes no changes from mp");
  });

  it("Use this time on the organizer's card opens the form at Robin's proposal, and Save sends event_update", async () => {
    const r = await calendarOn("AAMkAD-robin");
    const card = screen.getByRole("region", { name: "Event" });
    expect(within(card).getByText(/Proposes Wed 14 Oct 2099, 14:00 to 14:30/)).toBeInTheDocument();
    await r.user.click(within(card).getByRole("button", { name: "Use this time, proposed by Robin Roe" }));
    const form = await screen.findByRole("dialog", { name: "Edit event" });
    await r.user.click(within(form).getByRole("button", { name: /Save changes/ }));
    await waitFor(() => expect(callsOf("event_update")).toHaveLength(1));
    expect(callsOf("event_update")).toEqual([
      {
        params: {
          account: "home",
          event: "AAMkAD-robin",
          fields: { start: "2099-10-14T14:00", end: "2099-10-14T14:30", all_day: false, zone: "Europe/Berlin" },
        },
      },
    ]);
    act(() => void settleEventChange());
    await noticed("Updated 1:1 with Robin");
  });

  it("an attendee's card offers no Use this time", async () => {
    await calendarOn("AAMkAD-design");
    const card = screen.getByRole("region", { name: "Event" });
    expect(within(card).getByText(/Proposes/)).toBeInTheDocument();
    expect(within(card).queryByRole("button", { name: /Use this time/ })).toBeNull();
  });
});

describe("changing an event in the Calendar view", () => {
  it("E opens the form on the organizer's meeting, Save sends event_update with what changed, and the settle says so", async () => {
    const r = await calendarOn("AAMkAD-robin");
    await r.user.keyboard("E");
    const dialog = await screen.findByRole("dialog", { name: "Edit event" });
    const title = within(dialog).getByLabelText("Title");
    expect(title).toHaveValue("1:1 with Robin");
    expect(within(dialog).getByLabelText("Calendar")).toBeDisabled();
    await r.user.clear(title);
    await r.user.type(title, "1:1, moved");
    await r.user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("event_update")).toHaveLength(1));
    expect(callsOf("event_update")[0]).toEqual({ params: { account: "home", event: "AAMkAD-robin", fields: { summary: "1:1, moved" } } });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Edit event" })).toBeNull());
    act(() => void settleEventChange());
    await noticed("Updated 1:1, moved");
  });

  it("a failed update opens the form again with what was typed", async () => {
    const r = await calendarOn("AAMkAD-robin");
    await r.user.click(within(screen.getByRole("region", { name: "Event" })).getByRole("button", { name: "Edit" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit event" });
    await r.user.type(within(dialog).getByLabelText("Location"), "Room 2");
    await r.user.click(within(dialog).getByRole("button", { name: /Save changes/ }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Edit event" })).toBeNull());
    act(() => void settleEventChange({ fail: true }));
    const again = await screen.findByRole("dialog", { name: "Edit event" });
    expect(again.querySelector('[data-slot="event-error"]')).toHaveTextContent("The change did not go through: Graph refused the update: HTTP 503");
    expect(within(again).getByLabelText("Location")).toHaveValue("Room 2");
  });

  it("a failed update while another overlay is open keeps what was typed for Edit", async () => {
    const r = await calendarOn("AAMkAD-robin");
    await r.user.keyboard("E");
    const dialog = await screen.findByRole("dialog", { name: "Edit event" });
    await r.user.type(within(dialog).getByLabelText("Location"), "Room 2");
    await r.user.click(within(dialog).getByRole("button", { name: /Save changes/ }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Edit event" })).toBeNull());
    await r.user.keyboard("?");
    await screen.findByRole("dialog", { name: "Keys" });
    act(() => void settleEventChange({ fail: true }));
    expect(await screen.findByText(/HTTP 503; Edit on 1:1 with Robin opens the form again with what you typed/)).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Edit event" })).toBeNull();
    await r.user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Keys" })).toBeNull());
    await r.user.keyboard("E");
    const again = await screen.findByRole("dialog", { name: "Edit event" });
    expect(within(again).getByLabelText("Location")).toHaveValue("Room 2");
    expect(again.querySelector('[data-slot="event-error"]')).toHaveTextContent("The change did not go through");
  });

  it("an attendee's form changes the reminder and the show-as only", async () => {
    const r = await calendarOn("AAMkAD-design");
    await r.user.keyboard("E");
    const dialog = await screen.findByRole("dialog", { name: "Edit event (your reminder and show-as)" });
    expect(within(dialog).getByLabelText("Title")).toBeDisabled();
    const reminder = within(dialog).getByLabelText("Reminder");
    await r.user.clear(reminder);
    await r.user.type(reminder, "5");
    const showAs = within(dialog).getByLabelText("Show as");
    expect(showAs).toBeEnabled();
    await r.user.selectOptions(showAs, "Out of office");
    await r.user.click(within(dialog).getByRole("button", { name: /Save changes/ }));
    await waitFor(() => expect(callsOf("event_update")).toHaveLength(1));
    expect(callsOf("event_update")[0]).toEqual({ params: { account: "home", event: "AAMkAD-design", fields: { reminder: 5, show_as: "oof" } } });
  });

  it("a failed cancel opens the confirmation again with the comment, and confirming sends it again", async () => {
    const r = await calendarOn("AAMkAD-robin");
    await r.user.keyboard("D");
    const dialog = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    await r.user.type(within(dialog).getByLabelText("Comment"), "Postponed{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Cancel the meeting?" })).toBeNull());
    act(() => void settleEventChange({ fail: true }));
    const again = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    expect(within(again).getByLabelText("Comment")).toHaveValue("Postponed");
    expect(again.querySelector('[data-slot="event-remove-error"]')).toHaveTextContent("Cancelling 1:1 with Robin failed: Graph refused the cancel: HTTP 503");
    await r.user.click(within(again).getByRole("button", { name: /Cancel meeting/ }));
    await waitFor(() => expect(callsOf("event_cancel")).toHaveLength(2));
    expect(callsOf("event_cancel")[1]).toEqual({ account: "home", event: "AAMkAD-robin", comment: "Postponed" });
  });

  it("a cancel whose daemon restarted after the server took it asks again, and confirming counts as done", async () => {
    const r = await calendarOn("AAMkAD-robin");
    await r.user.keyboard("D");
    const dialog = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    await r.user.type(within(dialog).getByLabelText("Comment"), "Postponed{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Cancel the meeting?" })).toBeNull());
    act(() => void settleEventChange({ dropped: true }));
    expect(mock.events.some((e) => e.id === "AAMkAD-robin")).toBe(false);
    const again = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    expect(within(again).getByLabelText("Comment")).toHaveValue("Postponed");
    expect(again.querySelector('[data-slot="event-remove-error"]')).toHaveTextContent(RESTARTED_REMOVE);
    await r.user.click(within(again).getByRole("button", { name: /Cancel meeting/ }));
    await waitFor(() => expect(callsOf("event_cancel")).toHaveLength(2));
    expect(mock.eventChanges[0]?.retried).toBeDefined();
    act(() => void settleEventChange());
    await noticed(/Cancelled 1:1 with Robin; told /);
  });

  it("a cancel that fails while another overlay is open keeps its confirmation for D", async () => {
    const r = await calendarOn("AAMkAD-robin");
    await r.user.keyboard("D");
    const dialog = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    await r.user.type(within(dialog).getByLabelText("Comment"), "Postponed{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Cancel the meeting?" })).toBeNull());
    await r.user.keyboard("?");
    await screen.findByRole("dialog", { name: "Keys" });
    act(() => void settleEventChange({ fail: true }));
    expect(await screen.findByText(/HTTP 503; Cancel meeting on 1:1 with Robin asks again/)).toBeInTheDocument();
    await r.user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Keys" })).toBeNull());
    await r.user.keyboard("D");
    const again = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    expect(within(again).getByLabelText("Comment")).toHaveValue("Postponed");
  });

  it("D asks to cancel the organizer's meeting with a comment and sends event_cancel; on someone else's meeting it says to decline", async () => {
    const r = await calendarOn("AAMkAD-design");
    await r.user.keyboard("D");
    await noticed("Design review is someone else's meeting: decline it (Respond) to tell the organizer");
    expect(screen.queryByRole("dialog", { name: /Cancel the meeting/ })).toBeNull();

    await moveTo(r.user, "AAMkAD-robin");
    await r.user.keyboard("D");
    const dialog = await screen.findByRole("dialog", { name: "Cancel the meeting?" });
    expect(dialog).toHaveTextContent("The server sends its attendee the cancellation.");
    await r.user.type(within(dialog).getByLabelText("Comment"), "Postponed{Enter}");
    await waitFor(() => expect(callsOf("event_cancel")).toHaveLength(1));
    expect(callsOf("event_cancel")[0]).toEqual({ account: "home", event: "AAMkAD-robin", comment: "Postponed" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Cancel the meeting?" })).toBeNull());
    act(() => void settleEventChange());
    await noticed(/Cancelled 1:1 with Robin; told /);
  });
});

describe("an occurrence of a series", () => {
  const weekly: CalendarRecurrence = { frequency: "weekly", interval: 1, weekdays: ["wednesday"], count: 4 };

  it("opens the form on the occurrence naming its series, or on the series with its rule, sent only when it changed", () => {
    const occurrence = event({ kind: "occurrence", series_master_id: "ev-series", recurrence: "Weekly on Wednesday" });
    const alone = editDialogFor(occurrence, calendars, "occurrence") as EventDialog;
    expect(alone.edit?.scope).toBe("occurrence");
    expect(alone.fields.repeat.series).toBe("Weekly on Wednesday");
    const moved: EventDialog = { ...alone, fields: { ...alone.fields, location: "Room 2" } };
    expect(changedFields(moved, "Europe/Berlin")).toEqual({ location: "Room 2" });

    const master = event({ id: "ev-series", kind: "series-master", recurrence: "Weekly on Wednesday (4 times)" });
    const series = editDialogFor(master, calendars, "series", weekly) as EventDialog;
    expect(series.edit?.scope).toBe("series");
    expect(series.fields.repeat).toMatchObject({ pattern: "weekly", on: ["wednesday"], ends: "count", count: "4" });
    expect(editProblem(series)).toBe(NOTHING_CHANGED);
    const renamed: EventDialog = { ...series, fields: { ...series.fields, title: "Budget, weekly" } };
    expect(changedFields(renamed, "Europe/Berlin")).toEqual({ summary: "Budget, weekly" });
    const shorter: EventDialog = { ...series, fields: { ...series.fields, repeat: { ...series.fields.repeat, count: "2" } } };
    expect(changedFields(shorter, "Europe/Berlin")).toEqual({ recurrence: { frequency: "weekly", interval: 1, weekdays: ["wednesday"], count: 2 } });

    const odd: CalendarRecurrence = { frequency: "yearly", interval: 1, weekdays: ["thursday"], week_index: "fourth", month: 11 };
    const kept = editDialogFor(master, calendars, "series", odd) as EventDialog;
    expect(kept.fields.repeat).toMatchObject({ pattern: null, kept: odd });
    expect(changedFields({ ...kept, fields: { ...kept.fields, title: "Thanksgiving" } }, "Europe/Berlin")).toEqual({ summary: "Thanksgiving" });
  });

  it("opens the series form event_get answered only while its chooser is still open", () => {
    const occurrence = event({ kind: "occurrence", series_master_id: "ev-series" });
    const chosen = { event: occurrence, remove: false };
    const asking = openEventScope(initialState(), chosen);
    const series = editDialogFor(event({ id: "ev-series", kind: "series-master" }), calendars, "series", weekly) as EventDialog;
    expect(openEventSeriesForm(asking, chosen, series)).toMatchObject({ overlay: "event", eventDialog: series, eventScopeDialog: null });
    // Escape closed the chooser before the reply came.
    const kept = { ...asking, overlay: null, eventScopeDialog: null };
    expect(openEventSeriesForm(kept, chosen, series)).toBe(kept);
    // `o` opened the occurrence's form before the reply came.
    const alone = openEventDialog(asking, editDialogFor(occurrence, calendars, "occurrence") as EventDialog);
    expect(openEventSeriesForm(alone, chosen, series)).toBe(alone);
  });

  it("E on an occurrence asks its scope; the whole series reads the master through event_get and saves it in the series scope", async () => {
    const r = await calendarOn("AAMkAD-weekly-20991020");
    await r.user.keyboard("E");
    const ask = await screen.findByRole("dialog", { name: "Edit a recurring event" });
    expect(ask).toHaveTextContent("Weekly planning, Weekly on Tuesday");
    await r.user.click(within(ask).getByRole("button", { name: /The whole series/ }));
    const dialog = await screen.findByRole("dialog", { name: "Edit event (your reminder and show-as)" });
    expect(callsOf("event_get")).toEqual([{ account: "home", event: "AAMkAD-weekly" }]);
    expect(dialog.querySelector('[data-slot="repeat-summary"]')).toHaveTextContent("Weekly on Tuesday");
    await r.user.click(within(dialog).getByLabelText("None"));
    await r.user.type(within(dialog).getByLabelText("Reminder"), "30");
    await r.user.click(within(dialog).getByRole("button", { name: /Save changes/ }));
    await waitFor(() => expect(callsOf("event_update")).toHaveLength(1));
    expect(callsOf("event_update")[0]).toEqual({ params: { account: "home", event: "AAMkAD-weekly", scope: "series", fields: { reminder: 30 } } });
    act(() => void settleEventChange());
    await waitFor(() => expect(mock.events.find((e) => e.id === "AAMkAD-weekly-20991013")?.reminder).toBe(30));
    expect(mock.events.find((e) => e.id === "AAMkAD-weekly-20991020")?.reminder).toBeUndefined();

    await r.user.keyboard("E");
    const again = await screen.findByRole("dialog", { name: "Edit a recurring event" });
    await r.user.keyboard("o");
    const alone = await screen.findByRole("dialog", { name: "Edit event (your reminder and show-as)" });
    expect(alone.querySelector('[data-slot="repeat-series"]')).toHaveTextContent("Weekly on Tuesday (this occurrence only)");
    expect(again).not.toBeInTheDocument();
    await r.user.click(within(alone).getByLabelText("None"));
    await r.user.type(within(alone).getByLabelText("Reminder"), "5");
    await r.user.click(within(alone).getByRole("button", { name: /Save changes/ }));
    await waitFor(() => expect(callsOf("event_update")).toHaveLength(2));
    expect(callsOf("event_update")[1]).toEqual({ params: { account: "home", event: "AAMkAD-weekly-20991020", scope: "occurrence", fields: { reminder: 5 } } });
  });

  it("a Start moved past the series' last day shows the rule's problem once Start is left", async () => {
    const r = await calendarOn("AAMkAD-weekly-20991020", () => {
      const ours = { is_organizer: true, own_response: "organizer" as const };
      mock.events = mock.events.map((e) => (e.series_master_id === "AAMkAD-weekly" ? { ...e, ...ours } : e));
      mock.series = mock.series.map((s) =>
        s.master.id === "AAMkAD-weekly" ? { master: { ...s.master, ...ours }, recurrence: { frequency: "weekly", interval: 1, weekdays: ["tuesday"], until: "2099-10-27" } } : s,
      );
    });
    await r.user.keyboard("E");
    const ask = await screen.findByRole("dialog", { name: "Edit a recurring event" });
    await r.user.click(within(ask).getByRole("button", { name: /The whole series/ }));
    const dialog = await screen.findByRole("dialog", { name: "Edit the whole series" });
    expect(within(dialog).getByLabelText("Ends")).toHaveValue("until");
    const end = within(dialog).getByLabelText("End");
    await r.user.clear(end);
    await r.user.type(end, "2099-11-03T09:00");
    const start = within(dialog).getByLabelText("Start");
    await r.user.clear(start);
    await r.user.type(start, "2099-11-03T08:30");
    await r.user.tab();
    expect(dialog.querySelector('[data-slot="event-error"]')).toHaveTextContent("The series ends before its first day, 2099-11-03");
    expect(within(dialog).getByRole("button", { name: /Save changes/ })).toBeDisabled();
  });

  it("D on someone else's occurrence says to decline it before asking any scope", async () => {
    const r = await calendarOn("AAMkAD-weekly-20991020");
    await r.user.keyboard("D");
    await noticed(/Weekly planning is someone else's meeting/);
    expect(screen.queryByRole("dialog", { name: /recurring event/ })).toBeNull();
  });

  it("the form makes a weekly series whose occurrences join the calendar, and D on one cancels the whole series", async () => {
    const r = renderApp(1400);
    await shellReady();
    await r.user.keyboard(" a");
    await screen.findByRole("region", { name: "Calendar" });
    await r.user.click(screen.getByRole("button", { name: "New event" }));
    const dialog = await screen.findByRole("dialog", { name: /New event/ });
    await r.user.type(within(dialog).getByLabelText("Title"), "Standup");
    const start = within(dialog).getByLabelText("Start");
    await r.user.clear(start);
    await r.user.type(start, "2099-10-14T09:00");
    const end = within(dialog).getByLabelText("End");
    await r.user.clear(end);
    await r.user.type(end, "2099-10-14T09:15");
    await r.user.type(within(dialog).getByLabelText("Attendees"), "robin@example.com");
    await r.user.selectOptions(within(dialog).getByLabelText("Repeats"), "Weekly on Wednesday");
    await r.user.selectOptions(within(dialog).getByLabelText("Ends"), "After");
    await r.user.type(within(dialog).getByLabelText("Occurrences"), "3");
    expect(dialog.querySelector('[data-slot="repeat-summary"]')).toHaveTextContent("Weekly on Wednesday (3 times)");
    await r.user.click(within(dialog).getByRole("button", { name: /Create event/ }));
    await waitFor(() => expect(callsOf("event_create")).toHaveLength(1));
    const params = (callsOf("event_create")[0] as { params: { event: { recurrence?: CalendarRecurrence } } }).params;
    expect(params.event.recurrence).toEqual({ frequency: "weekly", interval: 1, weekdays: [], count: 3 });
    act(() => void settleEventCreate());
    await waitFor(() => expect(mock.events.filter((e) => e.series_master_id?.startsWith("AAMk-created-")).length).toBe(3));
    const first = mock.events.find((e) => e.series_master_id?.startsWith("AAMk-created-")) as CalendarEvent;
    expect(first.recurrence).toBe("Weekly on Wednesday (3 times)");

    await r.user.keyboard("gt");
    await waitFor(() => expect(document.activeElement).toHaveAttribute("aria-label", "Jump to date"));
    await r.user.keyboard("2099-10-14{Enter}");
    await moveTo(r.user, first.id);
    await r.user.keyboard("D");
    const ask = await screen.findByRole("dialog", { name: "Cancel or delete a recurring event" });
    await r.user.click(within(ask).getByRole("button", { name: /The whole series/ }));
    const confirm = await screen.findByRole("dialog", { name: "Cancel the whole series?" });
    await r.user.click(within(confirm).getByRole("button", { name: /Cancel meeting/ }));
    await waitFor(() => expect(callsOf("event_cancel")).toHaveLength(1));
    expect(callsOf("event_cancel")[0]).toEqual({ account: "home", event: first.id, comment: null, scope: "series" });
    act(() => void settleEventChange());
    await noticed(/Cancelled the series Standup; told robin@example.com/);
    expect(mock.events.some((e) => e.series_master_id === first.series_master_id)).toBe(false);
  });
});

describe("a settled change of an invitation mail", () => {
  const run = (kind: EventChangeRun["kind"]): EventChangeRun => ({ token: 1, account: "home", summary: "Budget", kind, dialog: null, removal: null, operation_id: "op" });
  const refused = [{ address: "cy@example.com", role: "To", delivered: false, error: "550 no such user" }];

  it("names the attendees the server refused it for, as a partly delivered send does", () => {
    expect(changedText(run("update"), { account: "home", event: event(), refused })).toBe("Updated Budget; failed: cy@example.com (550 no such user)");
    expect(changedText(run("cancel"), { account: "home", event: "imip:4", notified: ["ada@example.com"], refused })).toBe(
      "Cancelled Budget; told ada@example.com; failed: cy@example.com (550 no such user)",
    );
    expect(changedText(run("update"), { account: "home", event: event() })).toBe("Updated Budget");
  });
});
