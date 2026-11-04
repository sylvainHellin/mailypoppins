import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock, settleEventChange } from "@/test/tauri-mock";
import { changedFields, editDialogFor, editProblem, NOTHING_CHANGED, ORGANIZER_ONLY, removeDialogFor, RESTARTED_REMOVE } from "@/app/eventChange";
import { NO_TITLE } from "@/app/eventForm";
import type { CalendarEvent, CalendarInfo } from "@/protocol/types";
import type { EventDialog } from "@/app/state";

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
async function calendarOn(event: string) {
  const r = renderApp(1400);
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
    expect(removeDialogFor(event({ kind: "occurrence", series_master_id: "ev-series" }), calendars)).toMatch(/recurring series/);
    expect(removeDialogFor(event({ calendar: "ro" }), calendars)).toMatch(/takes no changes/);
    expect(editDialogFor(event({ id: "imip:7" }), calendars)).toMatch(/can only be answered/);
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
