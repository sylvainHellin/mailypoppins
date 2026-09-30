import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { GRAPH_RSVP_REFUSAL, INVITE_ROW, mock, SEND_FAIL_REASON, settleRsvp, simulateInvite } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const reader = () => screen.getByRole("complementary", { name: "Reader" });
const card = () => within(reader()).findByRole("region", { name: "Invitation" });
const replies = async () => within(await card()).getByRole("group", { name: "Reply to the invitation" });
const button = (group: HTMLElement, name: string) => within(group).getByRole("button", { name });
/** A transient notice of the status line says `text`. */
const noticed = (text: string) =>
  waitFor(() => expect([...document.querySelectorAll('[role="status"]')].some((e) => e.textContent?.includes(text))).toBe(true));

/** The inbox's last row, the steering committee's invitation, in the reader. */
async function openInvitation() {
  const r = renderApp();
  await shellReady();
  await r.user.keyboard("G");
  await card();
  return r;
}

describe("the reader's invitation card", () => {
  it("shows the event under the header with three enabled replies, and Accept sends and settles", async () => {
    const { user } = await openInvitation();
    const c = await card();
    expect(within(c).getByRole("heading", { name: "Steering committee" })).toBeInTheDocument();
    expect(c).toHaveTextContent("Your RSVPNo response yet");
    expect(callsOf("invite_get")).toEqual([{ account: "work", row_id: INVITE_ROW }]);
    const group = await replies();
    for (const name of ["Accept", "Tentative", "Decline"]) expect(button(group, name)).toBeEnabled();
    await user.click(button(group, "Accept"));
    expect(callsOf("calendar_rsvp")).toEqual([{ account: "work", row_id: INVITE_ROW, response: "accept" }]);
    await waitFor(() => expect(button(group, "Accept")).toBeDisabled());
    expect(await within(c).findByText("Sending Accept…")).toBeInTheDocument();
    act(() => void settleRsvp());
    expect(await screen.findByText("Replied accept to Steering committee")).toBeInTheDocument();
    // The Sent invalidation and the settle read the card again, which now carries the reply.
    await waitFor(() => expect(callsOf("invite_get").length).toBeGreaterThan(1));
    expect(await within(reader()).findByText("Accepted", { selector: '[data-slot="event-rsvp"]' })).toBeInTheDocument();
  });

  it("a reply no recipient took yet says it waits in the outbox", async () => {
    const { user } = await openInvitation();
    await user.click(button(await replies(), "Decline"));
    act(() => void settleRsvp({ delivered: false }));
    expect(await screen.findByText("Replied decline to Steering committee; queued in the outbox")).toBeInTheDocument();
  });

  it("after rsvp_fail the failure is a notice in the TUI's words and the outbox has the row", async () => {
    const { user } = await openInvitation();
    await user.click(button(await replies(), "Tentative"));
    act(() => void settleRsvp({ fail: true }));
    expect(await screen.findByText(`RSVP failed: ${SEND_FAIL_REASON}`)).toBeInTheDocument();
    expect(mock.outbox.work.rows.some((r) => r.state === "failed" && r.last_error === SEND_FAIL_REASON)).toBe(true);
    await waitFor(() => expect(button(within(reader()).getByRole("group", { name: "Reply to the invitation" }), "Tentative")).toBeEnabled());
  });

  it("follows invite_cancel: the card says cancelled and the replies are disabled with the TUI's sentence", async () => {
    await openInvitation();
    act(() => simulateInvite(true));
    expect(await within(reader()).findByText("Cancelled by the organizer.")).toBeInTheDocument();
    const group = within(reader()).getByRole("group", { name: "Reply to the invitation" });
    for (const name of ["Accept", "Tentative", "Decline"]) expect(button(group, name)).toBeDisabled();
    const reason = within(reader()).getByText("This event was cancelled by the organizer; nothing to RSVP");
    expect(button(group, "Accept")).toHaveAttribute("aria-describedby", reason.id);
  });

  it("follows invite_update: the card shows the new time", async () => {
    await openInvitation();
    act(() => simulateInvite(false));
    expect(await within(reader()).findByText(/2099-10-15 10:00 \+02:00/)).toBeInTheDocument();
  });

  it("the cancellation's own email is a CANCEL, which no one replies to", async () => {
    const { user } = await openInvitation();
    act(() => simulateInvite(true));
    // The cancellation lands at the top of the inbox.
    await user.keyboard("gg");
    const c = await card();
    expect(c).toHaveTextContent("Cancelled by the organizer.");
    expect(within(c).getByText("Only received invitations (REQUEST) can be RSVP'd")).toBeInTheDocument();
  });

  it("a plain email has no card", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await within(reader()).findByTitle(/Message body/);
    expect(within(reader()).queryByRole("region", { name: "Invitation" })).toBeNull();
    expect(callsOf("invite_get")).toEqual([]);
  });
});

describe("tv and the RSVP choice", () => {
  it("tv on an email that is no invitation says so and opens nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("tv");
    expect(await screen.findByText("Not a calendar invite")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull();
  });

  it("tv opens the choice: a, t, d pick, j, k and Tab move, Enter sends the choice", async () => {
    const { user } = await openInvitation();
    await user.keyboard("tv");
    const dialog = await screen.findByRole("dialog", { name: "RSVP" });
    expect(dialog).toHaveTextContent("Steering committee");
    const selected = () => within(dialog).getByRole("option", { selected: true }).getAttribute("data-response");
    expect(selected()).toBe("accept");
    await user.keyboard("d");
    expect(selected()).toBe("decline");
    await user.keyboard("k");
    expect(selected()).toBe("tentative");
    await user.keyboard("a");
    await user.keyboard("{Tab}");
    expect(selected()).toBe("tentative");
    // Tab moved the choice, and the focus stayed in the dialog.
    expect(dialog.contains(document.activeElement)).toBe(true);
    await user.keyboard("j");
    expect(selected()).toBe("decline");
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(selected()).toBe("tentative");
    await user.keyboard("{Enter}");
    expect(callsOf("calendar_rsvp")).toEqual([{ account: "work", row_id: INVITE_ROW, response: "tentative" }]);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull());
  });

  it("Escape and q close the choice without sending", async () => {
    const { user } = await openInvitation();
    await user.keyboard("tv");
    await screen.findByRole("dialog", { name: "RSVP" });
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull());
    await user.keyboard("tv");
    await screen.findByRole("dialog", { name: "RSVP" });
    await user.keyboard("q");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull());
    expect(callsOf("calendar_rsvp")).toEqual([]);
  });

  it("tv on a cancelled invitation says why instead of opening", async () => {
    const { user } = await openInvitation();
    act(() => simulateInvite(true));
    await within(reader()).findByText("Cancelled by the organizer.");
    await user.keyboard("tv");
    await noticed("This event was cancelled by the organizer; nothing to RSVP");
    expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull();
  });
});

describe("V in the Calendar view", () => {
  async function openCalendar() {
    const r = renderApp();
    await shellReady();
    await r.user.keyboard(" a");
    await screen.findByRole("listbox", { name: "Agenda" });
    return r;
  }

  it("opens the choice for the cursor row and sends it; the agenda row follows", async () => {
    const { user } = await openCalendar();
    await user.keyboard("V");
    const dialog = await screen.findByRole("dialog", { name: "RSVP" });
    expect(dialog).toHaveTextContent("Steering committee");
    await user.keyboard("d");
    await user.keyboard("{Enter}");
    expect(callsOf("calendar_rsvp")).toEqual([{ account: "work", row_id: INVITE_ROW, response: "decline" }]);
    act(() => void settleRsvp());
    expect(await screen.findByText("Replied decline to Steering committee")).toBeInTheDocument();
    const agenda = screen.getByRole("listbox", { name: "Agenda" });
    expect(await within(agenda).findByRole("option", { name: "2099-10-14 10:00, Steering committee, declined" })).toBeInTheDocument();
  });

  it("refuses the organizer's row and a cancelled one with the TUI's sentences", async () => {
    const { user } = await openCalendar();
    await user.keyboard("j");
    const region = screen.getByRole("region", { name: "Event" });
    expect(within(region).getByRole("button", { name: "RSVP" })).toBeDisabled();
    await user.keyboard("V");
    await noticed("You are the organizer of this invite; nothing to RSVP");
    await user.keyboard("j");
    await user.keyboard("V");
    await noticed("This event was cancelled by the organizer; nothing to RSVP");
    expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull();
  });

  it("a Graph account's row shows the daemon's sentence, from the probe", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ga");
    await user.keyboard(" a");
    const agenda = await screen.findByRole("listbox", { name: "Agenda" });
    await within(agenda).findByRole("option", { name: /Dentist/ });
    expect(callsOf("invite_refusal")).toContainEqual({ account: "home" });
    const region = screen.getByRole("region", { name: "Event" });
    expect(await within(region).findByText(GRAPH_RSVP_REFUSAL)).toBeInTheDocument();
    expect(within(region).getByRole("button", { name: "RSVP" })).toBeDisabled();
    await user.keyboard("V");
    await noticed(GRAPH_RSVP_REFUSAL);
    expect(screen.queryByRole("dialog", { name: "RSVP" })).toBeNull();
    expect(callsOf("calendar_rsvp")).toEqual([]);
  });
});
