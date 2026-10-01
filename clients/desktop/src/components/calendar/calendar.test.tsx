import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { INVITE_ROW, mock, simulateInvite } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const agenda = () => screen.findByRole("listbox", { name: "Agenda" });
const cursorRow = () => document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-row-id");
const rowIds = (list: HTMLElement) => within(list).getAllByRole("option").map((o) => Number(o.getAttribute("data-row-id")));
const card = () => screen.getByRole("region", { name: "Event" });

async function openCalendar() {
  const r = renderApp();
  await shellReady();
  await r.user.keyboard(" a");
  const list = await agenda();
  return { ...r, list };
}

describe("the Calendar view", () => {
  it("lists the upcoming agenda of the selected account with its badges, and the card of the cursor row", async () => {
    const { list } = await openCalendar();
    const region = screen.getByRole("region", { name: "Calendar" });
    expect(within(region).getByRole("heading", { name: "Calendar" })).toBeInTheDocument();
    expect(callsOf("calendar_events")).toEqual([{ account: "work" }]);
    expect(rowIds(list)).toEqual([INVITE_ROW, 9103, 9102, 9104]);
    expect(within(list).getByRole("option", { name: "2099-11-20 14:00, Budget review, cancelled" })).toHaveAttribute("data-cancelled", "true");
    expect(within(list).getByRole("option", { name: "2099-11-02 09:00, Team sync, organizer" })).toBeInTheDocument();
    expect(within(list).getByRole("option", { name: "undated, Offsite planning, tentative" })).toBeInTheDocument();
    expect(within(list).getByRole("option", { name: "2099-10-14 10:00, Steering committee, no reply" })).toHaveAttribute("aria-selected", "true");
    expect(region.querySelector('[data-slot="calendar-scope"]')).toHaveTextContent("work: 4 upcoming events");
    const c = card();
    expect(within(c).getByRole("heading", { name: "Steering committee" })).toBeInTheDocument();
    expect(c).toHaveTextContent("2099-10-14 10:00 +02:00 to 11:00 +02:00");
    expect(c).toHaveTextContent("Room 2.14");
    expect(c).toHaveTextContent("chair@example.com");
    expect(c).toHaveTextContent("Your RSVPNo response yet");
    const attendees = within(c).getByRole("list", { name: "Attendees" });
    expect(within(attendees).getAllByRole("listitem").map((li) => li.textContent)).toEqual([
      "chair@example.comAccepted",
      "me@example.comNo response yet",
      "finance@example.comDeclined",
    ]);
  });

  it("j, k, G and gg move the cursor and the card follows; the organizer's card has no RSVP and names its cancelled occurrence", async () => {
    const { user } = await openCalendar();
    await user.keyboard("j");
    expect(cursorRow()).toBe("9103");
    const c = card();
    expect(c).toHaveTextContent("Weekly on Monday");
    expect(c).not.toHaveTextContent("Your RSVP");
    expect(c).toHaveTextContent("1 occurrence cancelled: 2099-11-16 09:00 +01:00");
    await user.keyboard("j");
    expect(within(card()).getByText("Cancelled by the organizer.")).toBeInTheDocument();
    await user.keyboard("G");
    expect(cursorRow()).toBe("9104");
    await user.keyboard("k");
    expect(cursorRow()).toBe("9102");
    await user.keyboard("gg");
    expect(cursorRow()).toBe(String(INVITE_ROW));
    expect(document.activeElement).toHaveAttribute("data-row-id", String(INVITE_ROW));
  });

  it("t shows the past events with the TUI's status line and arms no t family key", async () => {
    const { user, list } = await openCalendar();
    await user.keyboard("t");
    expect(await screen.findByText("Calendar: showing all events")).toBeInTheDocument();
    await waitFor(() => expect(rowIds(list)).toEqual([9101, INVITE_ROW, 9103, 9102, 9104]));
    expect(cursorRow()).toBe("9101");
    // `to` in Mail opens an attachment; here `t` toggled and `o` does nothing.
    await user.keyboard("o");
    expect(callsOf("attachment_open")).toEqual([]);
    await user.keyboard("t");
    expect(await screen.findByText("Calendar: showing upcoming events")).toBeInTheDocument();
    await waitFor(() => expect(rowIds(list)).toHaveLength(4));
    expect(screen.queryByText(/arrives in/)).toBeNull();
  });

  it("Enter and e open the cursor row's invite.ics in the editor", async () => {
    const { user } = await openCalendar();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(callsOf("invite_source_open")).toEqual([{ account: "work", row_id: INVITE_ROW }]));
    expect(mock.editorOpens).toEqual(["/fixture/cache/renditions/invite-work-1008.ics"]);
    expect(await screen.findByText(/Opened the invite.ics of Steering committee in code --wait/)).toBeInTheDocument();
    await user.keyboard("je");
    await waitFor(() => expect(callsOf("invite_source_open")).toHaveLength(2));
    expect(callsOf("invite_source_open")[1]).toEqual({ account: "work", row_id: 9103 });
    expect(callsOf("editor_open")).toEqual([]);
  });

  it("a row with no ics says so in the TUI's words, and a failed editor names why", async () => {
    const { user } = await openCalendar();
    await user.keyboard("G");
    await user.keyboard("e");
    expect(await screen.findByText("That event has no ics source in the store")).toBeInTheDocument();
    expect(mock.editorOpens).toEqual([]);
    mock.editorFailure = { kind: "setup", message: "The editor did not start: `zed` exited with 1; check the editor setting." };
    await user.keyboard("gg");
    await user.keyboard("e");
    expect(await screen.findByText(/Open failed: The editor did not start/)).toBeInTheDocument();
  });

  it("Enter on a sidebar mailbox still brings Mail back with it", async () => {
    const { user } = await openCalendar();
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(document.activeElement?.closest("[data-pane]")).toHaveAttribute("data-pane", "sidebar");
    await user.keyboard("j{Enter}");
    expect(await screen.findByRole("listbox", { name: "Drafts messages" })).toBeInTheDocument();
    expect(callsOf("invite_source_open")).toEqual([]);
  });

  it("r reads the agenda again and says how many events it shows, without replying", async () => {
    const { user } = await openCalendar();
    await user.keyboard("r");
    await waitFor(() => expect(callsOf("calendar_events")).toHaveLength(2));
    expect(await screen.findByText("Calendar refreshed (4 events)")).toBeInTheDocument();
    expect(callsOf("draft_reply")).toEqual([]);
  });

  it("follows a cancellation and an update without a manual refresh", async () => {
    const { list } = await openCalendar();
    await act(async () => simulateInvite(true));
    await waitFor(() =>
      expect(within(list).getByRole("option", { name: "2099-10-14 10:00, Steering committee, cancelled" })).toBeInTheDocument(),
    );
    expect(within(card()).getByText("Cancelled by the organizer.")).toBeInTheDocument();
    await act(async () => simulateInvite(false));
    await waitFor(() => expect(within(list).getByRole("option", { name: /^2099-10-15 10:00, Steering committee/ })).toBeInTheDocument());
    expect(callsOf("calendar_events")).toHaveLength(3);
  });

  it("reads nothing while another view shows, and reads a stale agenda when it comes back", async () => {
    const { user } = await openCalendar();
    await user.keyboard("{Escape}");
    await screen.findByRole("listbox", { name: "Inbox messages" });
    await act(async () => simulateInvite(true));
    await new Promise((r) => setTimeout(r, 20));
    expect(callsOf("calendar_events")).toHaveLength(1);
    await user.keyboard(" a");
    const list = await agenda();
    await waitFor(() => expect(within(list).getByRole("option", { name: /Steering committee, cancelled/ })).toBeInTheDocument());
    expect(callsOf("calendar_events")).toHaveLength(2);
  });

  it("shows an account with no store yet as such, not as an error", async () => {
    const { user } = renderApp(1400, () =>
      mock.failing.set("calendar_events", {
        kind: "protocol",
        code: -32006,
        message: "calendar.events: the daemon refused the call: work has no local store to read yet; run `mp sync -A work` (-32006)",
      }),
    );
    await shellReady();
    await user.keyboard(" a");
    expect(await screen.findByText(/work has no local store yet, so it has no agenda/)).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("an agenda action from the palette outside the Calendar view says where it works", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await user.keyboard("Show past events");
    await user.keyboard("{Enter}");
    expect(await screen.findByText("Switch to the Calendar view first (Space a): this acts on the agenda")).toBeInTheDocument();
  });

  it("key help lists every agenda key as runnable, RSVP included", async () => {
    const { user } = await openCalendar();
    await user.keyboard("?");
    const help = await screen.findByRole("dialog", { name: "Keys" });
    const section = within(help).getByRole("group", { name: "CALENDAR" });
    for (const label of [
      "Open the invite email in $EDITOR",
      "RSVP to invitation (Accept/Tentative/Decline)",
      "Show past events / upcoming only",
      "Refresh events from disk",
    ]) {
      expect(within(section).getByText(label).closest("[cmdk-item]")).not.toHaveTextContent("M4");
    }
  });
});
