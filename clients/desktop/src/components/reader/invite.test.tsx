import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { INVITE_ROW, mock, SEND_FAIL_REASON, settleRsvp, simulateInvite } from "@/test/tauri-mock";

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
    // An account with no calendar backend answers with a reply mail, and the card says so.
    expect(within(c).getByText(/The answer is a reply mail to the organizer/)).toBeInTheDocument();
    const group = await replies();
    for (const name of ["Accept", "Tentative", "Decline"]) expect(button(group, name)).toBeEnabled();
    await user.click(button(group, "Accept"));
    expect(callsOf("event_respond")).toEqual([{ account: "work", event: `imip:${INVITE_ROW}`, response: "accept", comment: null, send_response: null }]);
    await waitFor(() => expect(button(group, "Accept")).toBeDisabled());
    expect(await within(c).findByText("Sending Accept…")).toBeInTheDocument();
    act(() => void settleRsvp());
    expect(await screen.findByText("Replied accept to Steering committee")).toBeInTheDocument();
    // The Sent invalidation and the settle read the card again, which now carries the reply.
    await waitFor(() => expect(callsOf("invite_get").length).toBeGreaterThan(1));
    expect(await within(reader()).findByText("Accepted", { selector: '[data-slot="event-rsvp"]' })).toBeInTheDocument();
  });

  it("a reply the organizer was not told of is a failure that points at the outbox", async () => {
    const { user } = await openInvitation();
    await user.click(button(await replies(), "Decline"));
    act(() => void settleRsvp({ delivered: false }));
    expect(
      await screen.findByText("RSVP failed: Replied decline to Steering committee, but the organizer could not be told; the outbox says why"),
    ).toBeInTheDocument();
  });

  it("after rsvp_fail the failure is a notice in the TUI's words and the outbox has the row", async () => {
    const { user } = await openInvitation();
    await user.click(button(await replies(), "Tentative"));
    act(() => void settleRsvp({ fail: true }));
    expect(await screen.findByText(`RSVP failed: ${SEND_FAIL_REASON}`)).toBeInTheDocument();
    expect(mock.outbox.work.rows.some((r) => r.state === "failed" && r.last_error === SEND_FAIL_REASON)).toBe(true);
    await waitFor(() => expect(button(within(reader()).getByRole("group", { name: "Reply to the invitation" }), "Tentative")).toBeEnabled());
  });

  it("the activity area's card cancels a running reply, whose end says it may still go out", async () => {
    const { user } = await openInvitation();
    await user.click(button(await replies(), "Accept"));
    const activity = screen.getByRole("region", { name: "Activity" });
    const running = await within(activity).findByRole("group", { name: "Sending Accept to Steering committee…" });
    await user.click(within(running).getByRole("button", { name: "Cancel the RSVP" }));
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-respond-1" }]));
    expect(
      await screen.findByText("Stopped waiting for the RSVP to Steering committee; the daemon may still send it, check the outbox"),
    ).toBeInTheDocument();
    await waitFor(() => expect(button(within(reader()).getByRole("group", { name: "Reply to the invitation" }), "Accept")).toBeEnabled());
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

  it("the arrows move the choice with the focus in the dialog", async () => {
    const { user } = await openInvitation();
    await user.keyboard("tv");
    const dialog = await screen.findByRole("dialog", { name: "RSVP" });
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
    const selected = () => within(dialog).getByRole("option", { selected: true }).getAttribute("data-response");
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(selected()).toBe("decline");
    await user.keyboard("{ArrowUp}");
    expect(selected()).toBe("tentative");
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
    expect(callsOf("event_respond")).toEqual([{ account: "work", event: `imip:${INVITE_ROW}`, response: "tentative", comment: null, send_response: null }]);
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
    expect(callsOf("event_respond")).toEqual([]);
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

describe("an invitation on an account with a calendar backend", () => {
  /** `home`'s inbox holds the dentist's invitation, row 9201, whose server event message.invite names. */
  function withHomeInvitation() {
    const row = structuredClone(mock.rows.work.inbox.find((r) => r.id === INVITE_ROW)!);
    Object.assign(row, {
      id: 9201,
      uid: 9201,
      message_id: "<dentist-9201@fixture.example>",
      subject: "Termin: Dentist",
      selector: "mp://home/inbox/dentist-9201@fixture.example",
      date_sort: "2026-09-30T23:00:00",
    });
    mock.rows.home.inbox.unshift(row);
  }

  async function openHomeInvitation() {
    const r = renderApp(1400, withHomeInvitation);
    await shellReady();
    await r.user.keyboard("ga");
    await screen.findByRole("dialog", { name: "Switch account" });
    await r.user.keyboard("2");
    await screen.findByRole("listbox", { name: "Inbox messages" });
    await r.user.keyboard("gg");
    return r;
  }

  it("answers the server event message.invite names, and says the server calendar tells the organizer", async () => {
    const { user } = await openHomeInvitation();
    const c = await card();
    expect(within(c).getByRole("heading", { name: "Dentist" })).toBeInTheDocument();
    expect(callsOf("invite_get")).toContainEqual({ account: "home", row_id: 9201 });
    expect(within(c).getByText(/The answer goes to the server calendar/)).toBeInTheDocument();
    expect(within(c).queryByText(/RSVP is not supported/)).toBeNull();
    expect(callsOf("invite_refusal")).toEqual([]);
    await user.click(button(await replies(), "Tentative"));
    expect(callsOf("event_respond")).toEqual([{ account: "home", event: "AAMkAD-dentist", response: "tentative", comment: null, send_response: null }]);
    act(() => void settleRsvp());
    expect(await screen.findByText("Replied tentative to Dentist")).toBeInTheDocument();
  });

  it("tv offers the comment and the send-response choice, and sends them", async () => {
    const { user } = await openHomeInvitation();
    await card();
    await user.keyboard("tv");
    const dialog = await screen.findByRole("dialog", { name: "RSVP" });
    expect(dialog).toHaveTextContent("Dentist");
    await user.keyboard("jj{Tab}");
    await waitFor(() => expect(document.activeElement).toHaveAttribute("aria-label", "Comment"));
    await user.keyboard("Running late");
    await user.click(within(dialog).getByRole("checkbox", { name: "Send a response to the organizer" }));
    await user.click(within(dialog).getByRole("button", { name: /^Send Decline/ }));
    expect(callsOf("event_respond")).toEqual([{ account: "home", event: "AAMkAD-dentist", response: "decline", comment: "Running late", send_response: false }]);
  });
});
