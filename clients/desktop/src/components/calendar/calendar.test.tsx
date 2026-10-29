import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, IMIP_SILENT_REFUSAL, INVITE_ROW, mock, settleRsvp, simulateInvite } from "@/test/tauri-mock";
import { ORGANIZER } from "@/app/rsvp";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const lastView = () => callsOf("calendar_view").slice(-1)[0];
const region = () => screen.getByRole("region", { name: "Calendar" });
const card = () => screen.getByRole("region", { name: "Event" });
const cursorEvent = () => document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-event-id");
const optionNames = (list: HTMLElement) => within(list).getAllByRole("option").map((o) => o.getAttribute("aria-label"));
const idsOn = (list: HTMLElement, day: string) =>
  [...list.querySelectorAll<HTMLElement>(`[role="option"][data-day="${day}"]`)].map((o) => o.getAttribute("data-event-id"));
/** A transient notice of the status line says `text`. */
const noticed = (text: string) =>
  waitFor(() => expect([...document.querySelectorAll('[role="status"]')].some((e) => e.textContent?.includes(text))).toBe(true));

type User = ReturnType<typeof renderApp>["user"];

/** `gt`, a date, Enter: the view anchored on that day. */
async function jumpTo(user: User, day: string) {
  await user.keyboard("gt");
  await waitFor(() => expect(document.activeElement).toHaveAttribute("aria-label", "Jump to date"));
  await user.keyboard(`${day}{Enter}`);
}

/** The Calendar view on the fixture's week, the agenda from Wednesday 2099-10-14. */
async function openCalendar(before?: () => void) {
  const r = renderApp(1400, before);
  await shellReady();
  await r.user.keyboard(" a");
  await screen.findByRole("region", { name: "Calendar" });
  await jumpTo(r.user, "2099-10-14");
  const list = await screen.findByRole("listbox", { name: "Agenda" });
  await within(list).findByRole("option", { name: /Design review/ });
  return { ...r, list };
}

describe("the Calendar view", () => {
  it("lists every account's events from calendar_view by day, all-day first, each with its account and calendar", async () => {
    const { list } = await openCalendar();
    expect(lastView()).toEqual({ start: "2099-10-14T00:00:00+02:00", end: "2099-10-29T00:00:00+01:00", zone: "Europe/Berlin" });
    expect(within(list).getAllByRole("group").map((g) => g.getAttribute("aria-label"))).toEqual(["Wed 14 Oct 2099", "Fri 16 Oct 2099", "Tue 20 Oct 2099"]);
    expect(optionNames(list)).toEqual([
      "09:00-10:30, Design review, home/Calendar, no reply",
      "10:00-11:00, Steering committee, work/Invitations, no reply",
      "10:00-10:30, 1:1 with Robin, home/Calendar, organizer",
      "all day, Company holiday, home/Holidays",
      "08:30-09:00, Weekly planning, home/Calendar, accepted",
    ]);
    expect(region().querySelector('[data-slot="calendar-scope"]')).toHaveTextContent("Wed 14 Oct 2099 to Wed 28 Oct 2099: 5 events");
    expect(cursorEvent()).toBe("AAMkAD-design");
    const c = card();
    expect(within(c).getByRole("heading", { name: "Design review" })).toBeInTheDocument();
    expect(c).toHaveTextContent("WhenWed 14 Oct 2099, 09:00 to 10:30");
    expect(c).toHaveTextContent("WhereTeams");
    expect(c).toHaveTextContent("Calendarhome/Calendar");
    expect(c).toHaveTextContent("OrganizerKim Lee <kim@example.com>");
    expect(c).toHaveTextContent("Your responseNo response yet");
    expect(c).toHaveTextContent("Walk through the new reader layout before the release.");
    expect(within(c).getByRole("button", { name: "Join the meeting" })).toBeInTheDocument();
    expect(within(c).queryByRole("button", { name: "Open mail" })).toBeNull();
    const attendees = within(c).getByRole("list", { name: "Attendees" });
    expect(within(attendees).getAllByRole("listitem").map((li) => li.textContent)).toEqual([
      "Kim Lee <kim@example.com>Accepted",
      "Me <me@example.com>No response yet",
      "Robin Roe <robin@example.com> (optional)Tentative",
    ]);
  });

  it("j and k move the cursor and the card follows; the occurrence of a series says so", async () => {
    const { user } = await openCalendar();
    await user.keyboard("j");
    expect(cursorEvent()).toBe(`imip:${INVITE_ROW}`);
    expect(within(card()).getByRole("button", { name: "Open mail" })).toBeInTheDocument();
    expect(card()).toHaveTextContent("Calendarwork/Invitations");
    await user.keyboard("G");
    expect(cursorEvent()).toBe("AAMkAD-weekly-20991020");
    expect(card()).toHaveTextContent("RepeatsFREQ=WEEKLY;BYDAY=TU");
    await user.keyboard("k");
    expect(cursorEvent()).toBe("AAMkAD-holiday");
    expect(card()).toHaveTextContent("WhenFri 16 Oct 2099, all day");
    expect(card()).not.toHaveTextContent("Your response");
    await user.keyboard("gg");
    expect(cursorEvent()).toBe("AAMkAD-design");
  });

  it("the week grid shows Monday to Sunday, the all-day band on top and overlapping events side by side", async () => {
    const { user } = await openCalendar();
    await user.keyboard("w");
    expect(await screen.findByText("Calendar: week layout")).toBeInTheDocument();
    const week = await screen.findByRole("listbox", { name: "Week" });
    expect(lastView()).toMatchObject({ start: "2099-10-12T00:00:00+02:00", end: "2099-10-19T00:00:00+02:00" });
    const columns = within(week)
      .getAllByRole("group")
      .filter((g) => !g.hasAttribute("data-band"))
      .map((g) => g.getAttribute("aria-label"));
    expect(columns).toEqual(["Mon 12 Oct 2099", "Tue 13 Oct 2099", "Wed 14 Oct 2099", "Thu 15 Oct 2099", "Fri 16 Oct 2099", "Sat 17 Oct 2099", "Sun 18 Oct 2099"]);
    await waitFor(() => expect(idsOn(week, "2099-10-14")).toEqual(["AAMkAD-design", `imip:${INVITE_ROW}`, "AAMkAD-robin"]));
    expect(idsOn(week, "2099-10-13")).toEqual(["AAMkAD-weekly-20991013"]);
    const band = within(week).getByRole("group", { name: "Fri 16 Oct 2099, all day" });
    expect(within(band).getByRole("option", { name: /Company holiday/ })).toBeInTheDocument();
    // The three of Wednesday overlap: three columns, each a third wide.
    const wednesday = within(week).getByRole("group", { name: "Wed 14 Oct 2099" });
    const blocks = within(wednesday).getAllByRole("option");
    expect(blocks.map((b) => b.getAttribute("data-column"))).toEqual(["1/3", "2/3", "3/3"]);
    expect(blocks[0].style.top).toBe(`${9 * 44}px`);
    // The cursor stays on the event it was on.
    expect(cursorEvent()).toBe("AAMkAD-design");
    expect(document.querySelector('[aria-current="date"]')).toBeNull();
    await user.keyboard("t");
    await waitFor(() => expect(document.querySelector('[aria-current="date"]')).toHaveAttribute("data-today", "true"));
  });

  it("the day grid shows one day, and h and l move it by a day", async () => {
    const { user } = await openCalendar();
    await user.keyboard("d");
    const day = await screen.findByRole("listbox", { name: "Day" });
    expect(region().querySelector('[data-slot="calendar-scope"]')).toHaveTextContent("Wed 14 Oct 2099: 3 events");
    expect(idsOn(day, "2099-10-14")).toEqual(["AAMkAD-design", `imip:${INVITE_ROW}`, "AAMkAD-robin"]);
    await user.keyboard("ll");
    await waitFor(() => expect(region().querySelector('[data-slot="calendar-scope"]')).toHaveTextContent("Fri 16 Oct 2099: 1 event"));
    const band = within(screen.getByRole("listbox", { name: "Day" })).getByRole("group", { name: "Fri 16 Oct 2099, all day" });
    expect(within(band).getByRole("option", { name: /Company holiday/ })).toBeInTheDocument();
    await user.keyboard("h");
    await waitFor(() => expect(region().querySelector('[data-slot="calendar-scope"]')).toHaveTextContent("Thu 15 Oct 2099: 0 events"));
  });

  it("l and h move the agenda by a week, reading the new days", async () => {
    const { user, list } = await openCalendar();
    const reads = callsOf("calendar_view").length;
    await user.keyboard("]");
    await waitFor(() => expect(callsOf("calendar_view")).toHaveLength(reads + 1));
    expect(lastView()).toMatchObject({ start: "2099-10-21T00:00:00+02:00" });
    await waitFor(() => expect(region().querySelector('[data-slot="calendar-scope"]')).toHaveTextContent("Wed 21 Oct 2099 to Wed 4 Nov 2099: 0 events"));
    await user.keyboard("h");
    await waitFor(() => expect(within(list).getAllByRole("option")).toHaveLength(5));
  });

  it("hiding a calendar takes its events out, and showing it brings them back", async () => {
    const { user, list } = await openCalendar();
    await user.keyboard("c");
    const panel = await screen.findByRole("region", { name: "Calendars" });
    const holidays = await within(panel).findByRole("checkbox", { name: "home/Holidays" });
    expect(holidays).toBeChecked();
    expect(within(panel).getByRole("checkbox", { name: "work/Invitations" })).toBeChecked();
    await user.click(holidays);
    expect(callsOf("calendar_set_visibility")).toEqual([{ account: "home", calendar: "AAMk-cal-holidays", visible: false }]);
    await waitFor(() => expect(within(list).queryByRole("option", { name: /Company holiday/ })).toBeNull());
    await waitFor(() => expect(within(panel).getByRole("checkbox", { name: "home/Holidays" })).not.toBeChecked());
    expect(await screen.findByText("Hiding home/Holidays")).toBeInTheDocument();
    await user.click(within(panel).getByRole("checkbox", { name: "home/Holidays" }));
    await waitFor(() => expect(within(list).getByRole("option", { name: /Company holiday/ })).toBeInTheDocument());
  });

  it("Space and Enter toggle the focused calendar, not the view's bindings", async () => {
    const { user } = await openCalendar();
    await user.keyboard("c");
    const panel = await screen.findByRole("region", { name: "Calendars" });
    const holidays = await within(panel).findByRole("checkbox", { name: "home/Holidays" });
    holidays.focus();
    await user.keyboard(" ");
    expect(callsOf("calendar_set_visibility")).toEqual([{ account: "home", calendar: "AAMk-cal-holidays", visible: false }]);
    await waitFor(() => expect(within(panel).getByRole("checkbox", { name: "home/Holidays" })).not.toBeChecked());
    await user.keyboard("{Enter}");
    await waitFor(() => expect(callsOf("calendar_set_visibility")).toHaveLength(2));
    expect(callsOf("calendar_set_visibility")[1]).toEqual({ account: "home", calendar: "AAMk-cal-holidays", visible: true });
  });

  it("r reads the events again and says how many it holds", async () => {
    const { user } = await openCalendar();
    const reads = callsOf("calendar_view").length;
    await user.keyboard("r");
    await waitFor(() => expect(callsOf("calendar_view")).toHaveLength(reads + 1));
    expect(await screen.findByText("Calendar refreshed (5 events)")).toBeInTheDocument();
  });

  it("reads again on calendar.changed, and only while it shows", async () => {
    const { user, list } = await openCalendar();
    await user.keyboard("{Escape}");
    await screen.findByRole("listbox", { name: "Inbox messages" });
    const reads = callsOf("calendar_view").length;
    act(() => {
      mock.events = mock.events.filter((e) => e.id !== "AAMkAD-robin");
      emitEnvelope("calendar.changed", { account: "home" });
    });
    await new Promise((r) => setTimeout(r, 20));
    expect(callsOf("calendar_view")).toHaveLength(reads);
    await user.keyboard(" a");
    const again = await screen.findByRole("listbox", { name: "Agenda" });
    await waitFor(() => expect(callsOf("calendar_view")).toHaveLength(reads + 1));
    await waitFor(() => expect(within(again).queryByRole("option", { name: /1:1 with Robin/ })).toBeNull());
    expect(list.isConnected).toBe(false);
  });

  it("follows an invitation's cancellation without a manual refresh", async () => {
    const { list } = await openCalendar();
    act(() => simulateInvite(true));
    await waitFor(() => expect(within(list).getByRole("option", { name: "10:00-11:00, Steering committee, work/Invitations, cancelled" })).toBeInTheDocument());
  });

  it("e opens an invitation's invite.ics, and a server event says it has none", async () => {
    const { user } = await openCalendar();
    await user.keyboard("e");
    await noticed("This event is the server's; no invitation mail stands behind it");
    expect(callsOf("invite_source_open")).toEqual([]);
    await user.keyboard("j");
    await user.keyboard("e");
    await waitFor(() => expect(callsOf("invite_source_open")).toEqual([{ account: "work", row_id: INVITE_ROW }]));
    expect(mock.editorOpens).toEqual([`/fixture/cache/renditions/invite-work-${INVITE_ROW}.ics`]);
  });

  it("Open mail shows the invitation mail in Mail", async () => {
    const { user } = await openCalendar();
    await user.keyboard("j");
    await user.click(within(card()).getByRole("button", { name: "Open mail" }));
    await screen.findByRole("listbox", { name: "Inbox messages" });
    await waitFor(() => expect(callsOf("message_html_meta")).toContainEqual({ account: "work", row_id: INVITE_ROW }));
    const inbox = screen.getByRole("listbox", { name: "Inbox messages" });
    await waitFor(() => expect(inbox.querySelector('[data-cursor="true"]')).toHaveAttribute("data-row-id", String(INVITE_ROW)));
    const reader = screen.getByRole("complementary", { name: "Reader" });
    const invitation = await within(reader).findByRole("region", { name: "Invitation" });
    expect(within(invitation).getByRole("heading", { name: "Steering committee" })).toBeInTheDocument();
  });

  it("an event that fails to load says so", async () => {
    const { user } = renderApp(1400, () =>
      mock.failing.set("calendar_view", { kind: "protocol", code: -32602, message: "calendar.view: the daemon refused the call: bad range (-32602)" }),
    );
    await shellReady();
    await user.keyboard(" a");
    expect(await screen.findByRole("alert")).toHaveTextContent("The calendar did not load:");
  });

  it("a calendar action from the palette outside the Calendar view says where it works", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await user.keyboard("Go to today");
    await user.keyboard("{Enter}");
    expect(await screen.findByText("Switch to the Calendar view first (Space a): this acts on the calendar")).toBeInTheDocument();
  });

  it("key help lists every calendar key as runnable", async () => {
    const { user } = await openCalendar();
    await user.keyboard("?");
    const help = await screen.findByRole("dialog", { name: "Keys" });
    const section = within(help).getByRole("group", { name: "CALENDAR" });
    for (const label of [
      "Previous / next day or week (also [ and ])",
      "Go to today",
      "Jump to date (e.g. 2026-07-06)",
      "Agenda / day / week layout",
      "Open the event card",
      "Open the invitation's .ics in $EDITOR",
      "Respond (accept / tentative / decline, comment)",
      "Show or hide calendars",
      "Read the events again",
    ]) {
      expect(within(section).getByText(label).closest("[cmdk-item]")).not.toHaveTextContent(/later|soon/);
    }
  });
});

describe("V and the respond choice in the Calendar view", () => {
  it("sends one event.respond with the comment and send_response on a server event", async () => {
    const { user } = await openCalendar();
    await user.keyboard("V");
    const dialog = await screen.findByRole("dialog", { name: "RSVP" });
    expect(dialog).toHaveTextContent("Design review");
    await user.keyboard("d");
    const toggle = within(dialog).getByRole("checkbox", { name: "Send a response to the organizer" });
    expect(toggle).toBeChecked();
    await user.click(toggle);
    await user.click(within(dialog).getByRole("textbox", { name: "Comment" }));
    await user.keyboard("On leave that week{Enter}");
    expect(callsOf("event_respond")).toEqual([
      { account: "home", event: "AAMkAD-design", response: "decline", comment: "On leave that week", send_response: false },
    ]);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull());
    act(() => void settleRsvp());
    expect(await screen.findByText("Replied decline to Design review; no response sent to the organizer")).toBeInTheDocument();
    await waitFor(() => expect(card()).toHaveTextContent("Your responseDeclined"));
    expect(callsOf("event_respond")).toHaveLength(1);
  });

  it("an invitation mail's event offers no send-response choice and sends the daemon's default", async () => {
    const { user } = await openCalendar();
    await user.keyboard("j");
    await user.click(within(card()).getByRole("button", { name: "Respond" }));
    const dialog = await screen.findByRole("dialog", { name: "RSVP" });
    expect(dialog).toHaveTextContent("Steering committee");
    expect(within(dialog).queryByRole("checkbox")).toBeNull();
    expect(dialog).toHaveTextContent("The answer goes to the organizer as a reply mail.");
    await user.keyboard("{Enter}");
    expect(callsOf("event_respond")).toEqual([{ account: "work", event: `imip:${INVITE_ROW}`, response: "accept", comment: null, send_response: null }]);
    act(() => void settleRsvp({ delivered: false }));
    expect(await screen.findByText("Replied accept to Steering committee; queued in the outbox")).toBeInTheDocument();
  });

  it("refuses the organizer's event with the TUI's sentence, and the daemon's refusal reaches the notice line", async () => {
    const { user } = await openCalendar();
    await user.keyboard("jj");
    expect(cursorEvent()).toBe("AAMkAD-robin");
    expect(within(card()).getByRole("button", { name: "Respond" })).toBeDisabled();
    expect(within(card()).getByText(ORGANIZER)).toBeInTheDocument();
    await user.keyboard("V");
    await noticed(ORGANIZER);
    expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull();
    mock.failing.set("event_respond", { kind: "protocol", code: -32602, message: IMIP_SILENT_REFUSAL });
    await user.keyboard("k");
    await user.keyboard("V");
    await screen.findByRole("dialog", { name: "RSVP" });
    await user.keyboard("{Enter}");
    expect(await screen.findByText(`RSVP failed: ${IMIP_SILENT_REFUSAL}`)).toBeInTheDocument();
  });
});
