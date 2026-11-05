import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emit, mock, settleEventCreate } from "@/test/tauri-mock";
import { eventParams, eventProblem, writableCalendars } from "@/app/eventForm";
import type { CalendarInfo } from "@/protocol/types";
import { NO_REPEAT } from "@/app/recurrence";
import type { EventDialog } from "@/app/state";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const form = () => screen.findByRole("dialog", { name: /New event/ });
const alertOf = (dialog: HTMLElement) => dialog.querySelector('[data-slot="event-error"]');

async function openForm() {
  const r = renderApp();
  await shellReady();
  await r.user.keyboard(" a");
  await screen.findByRole("region", { name: "Calendar" });
  await r.user.click(screen.getByRole("button", { name: "New event" }));
  const dialog = await form();
  return { ...r, dialog };
}

function info(account: string, id: string, isDefault: boolean, features: string[], canEdit = true): CalendarInfo {
  return { account, id, name: `Cal ${id}`, color: null, is_default: isDefault, can_edit: canEdit, visible: true, backend: "graph", features };
}

describe("the event form's model", () => {
  it("offers the writable calendars of a writing backend, each account's default first", () => {
    const calendars = writableCalendars([
      info("home", "second", false, ["write", "online-meeting"]),
      info("home", "main", true, ["write", "online-meeting"]),
      info("home", "shared", false, ["write"], false),
      info("work", "imip", false, []),
    ]);
    expect(calendars.map((c) => c.key)).toEqual(["home/main", "home/second"]);
    expect(calendars[0].teams).toBe(true);
  });

  it("checks the fields as typed and sends them as typed, in the window's zone", () => {
    const calendars = writableCalendars([info("home", "main", true, ["write", "online-meeting"])]);
    const dialog: EventDialog = {
      calendars,
      transactionId: "mp-1",
      error: null,
      fields: {
        calendar: "home/main",
        title: " Kick-off ",
        allDay: false,
        start: "2099-10-14T14:00",
        end: "2099-10-14T15:00",
        location: "",
        to: "robin@example.com; kim@example.com",
        optional: "",
        rooms: "room-1@example.com",
        teams: true,
        reminder: "",
        noReminder: true,
        showAs: "busy",
        body: "",
        repeat: NO_REPEAT,
      },
    };
    expect(eventProblem(dialog.fields, calendars)).toBeNull();
    expect(eventProblem({ ...dialog.fields, title: " " }, calendars)).toBe("The event needs a title");
    expect(eventProblem({ ...dialog.fields, end: "2099-10-14T13:00" }, calendars)).toBe("The end comes before the start");
    expect(eventProblem({ ...dialog.fields, to: "robin" }, calendars)).toMatch(/not an email address/);
    expect(eventParams(dialog, "Europe/Berlin")).toEqual({
      account: "home",
      calendar: "main",
      transaction_id: "mp-1",
      event: {
        summary: "Kick-off",
        start: "2099-10-14T14:00",
        end: "2099-10-14T15:00",
        zone: "Europe/Berlin",
        all_day: false,
        attendees: [
          { email: "robin@example.com", role: "required" },
          { email: "kim@example.com", role: "required" },
          { email: "room-1@example.com", role: "resource" },
        ],
        online_meeting: true,
        reminder: null,
      },
    });
    const allDay = { ...dialog, transactionId: null, fields: { ...dialog.fields, allDay: true, start: "2099-10-14", end: "", noReminder: false } };
    const params = eventParams(allDay, "Europe/Berlin");
    expect(params.event).toMatchObject({ start: "2099-10-14", all_day: true });
    expect(params.event.end).toBeUndefined();
    expect(params.event.reminder).toBeUndefined();
    expect(params.transaction_id).toBeUndefined();
    expect(params.event.show_as).toBeUndefined();
    expect(eventParams({ ...allDay, fields: { ...allDay.fields, showAs: "oof" } }, "Europe/Berlin").event.show_as).toBe("oof");
  });
});

describe("the event form", () => {
  it("opens on n in the Calendar view on the Graph account's calendar, creates through event_create and closes; the settle says so", async () => {
    const r = renderApp();
    await shellReady();
    await r.user.keyboard(" a");
    await screen.findByRole("region", { name: "Calendar" });
    await r.user.keyboard("n");
    const dialog = await form();
    expect(within(dialog).getByLabelText("Calendar")).toHaveValue("home/AAMk-cal-home");
    // A fresh form is not wrong yet: the title's problem shows once the field is left.
    expect(alertOf(dialog)).toBeEmptyDOMElement();
    expect(within(dialog).getByRole("button", { name: /Create event/ })).toBeDisabled();
    await r.user.click(within(dialog).getByLabelText("Title"));
    await r.user.tab();
    expect(alertOf(dialog)).toHaveTextContent("The event needs a title");
    await r.user.type(within(dialog).getByLabelText("Title"), "Kick-off");
    expect(alertOf(dialog)).toBeEmptyDOMElement();
    await r.user.type(within(dialog).getByLabelText("Attendees"), "robin@example.com");
    await r.user.click(within(dialog).getByLabelText("Teams meeting"));
    await r.user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("event_create")).toHaveLength(1));
    const { params } = callsOf("event_create")[0] as { params: { account: string; calendar: string; transaction_id?: string; event: Record<string, unknown> } };
    expect(params.account).toBe("home");
    expect(params.calendar).toBe("AAMk-cal-home");
    expect(params.transaction_id).toBeUndefined();
    expect(params.event).toMatchObject({ summary: "Kick-off", online_meeting: true, all_day: false });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /New event/ })).toBeNull());
    act(() => void settleEventCreate());
    expect(await screen.findByText("Created Kick-off; invited robin@example.com")).toBeInTheDocument();
  });

  it("shows an untouched field's problem once a create is tried, and sends nothing", async () => {
    const { user, dialog } = await openForm();
    expect(alertOf(dialog)).toBeEmptyDOMElement();
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(alertOf(dialog)).toHaveTextContent("The event needs a title"));
    expect(callsOf("event_create")).toHaveLength(0);
  });

  it("shows a daemon refusal verbatim and stays open with what was typed", async () => {
    const { user, dialog } = await openForm();
    fireEvent.change(within(dialog).getByLabelText("Calendar"), { target: { value: "home/AAMk-cal-home" } });
    await user.type(within(dialog).getByLabelText("Title"), "Kick-off");
    mock.failing.set("event_create", { kind: "protocol", code: -32602, message: "The calendar Calendar of home is read-only" });
    await user.click(within(dialog).getByRole("button", { name: /Create event/ }));
    expect(await within(dialog).findByText("The calendar Calendar of home is read-only")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("Title")).toHaveValue("Kick-off");
    mock.failing.delete("event_create");
  });

  it("a failed create opens the form again with Retry, which sends the same transaction id", async () => {
    const { user, dialog } = await openForm();
    await user.type(within(dialog).getByLabelText("Title"), "Kick-off");
    await user.click(within(dialog).getByRole("button", { name: /Create event/ }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /New event/ })).toBeNull());
    act(() => void settleEventCreate({ fail: true }));
    const again = await screen.findByRole("dialog", { name: "New event (retry)" });
    expect(alertOf(again)).toHaveTextContent("The event was not created: Graph refused the new event: HTTP 503");
    expect(within(again).getByLabelText("Title")).toHaveValue("Kick-off");
    await user.click(within(again).getByRole("button", { name: /Retry/ }));
    await waitFor(() => expect(callsOf("event_create")).toHaveLength(2));
    const [first, second] = callsOf("event_create") as { params: { transaction_id?: string } }[];
    expect(first.params.transaction_id).toBeUndefined();
    expect(second.params.transaction_id).toBe("mp-fixture-1");
  });

  it("a create the daemon restarted under opens the form again with Retry", async () => {
    const { user, dialog } = await openForm();
    await user.type(within(dialog).getByLabelText("Title"), "Kick-off");
    await user.click(within(dialog).getByRole("button", { name: /Create event/ }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /New event/ })).toBeNull());
    act(() => emit({ type: "operation_dropped", operation_id: "fixture-create-1", kind: "event_create", reason: "the daemon restarted" }));
    const again = await screen.findByRole("dialog", { name: "New event (retry)" });
    expect(alertOf(again)).toHaveTextContent("The daemon restarted before the event was confirmed");
  });
});
