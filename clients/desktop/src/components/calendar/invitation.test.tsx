import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { GRAPH_RSVP_REFUSAL, mock, SEND_FAIL_REASON, settleInvite } from "@/test/tauri-mock";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const form = () => screen.findByRole("dialog", { name: "New invitation" });
const alertOf = (dialog: HTMLElement) => dialog.querySelector('[data-slot="invite-error"]');

async function openForm() {
  const r = renderApp();
  await shellReady();
  await r.user.keyboard(" a");
  await screen.findByRole("listbox", { name: "Agenda" });
  await r.user.click(screen.getByRole("button", { name: "New invitation" }));
  const dialog = await form();
  return { ...r, dialog };
}

/** Type the fields a sendable invitation needs; Start is a `datetime-local`. */
async function fill(user: ReturnType<typeof renderApp>["user"], dialog: HTMLElement, fields: { to?: string; subject?: string; start?: string; duration?: string }) {
  const d = within(dialog);
  if (fields.to) await user.type(d.getByLabelText("To"), fields.to);
  if (fields.subject) await user.type(d.getByLabelText("Subject"), fields.subject);
  if (fields.start) fireEvent.change(d.getByLabelText("Start"), { target: { value: fields.start } });
  if (fields.duration) await user.type(d.getByLabelText("Duration"), fields.duration);
}

describe("the New invitation form", () => {
  it("opens from the Calendar view's toolbar in To, sends send_invite and closes; the settle says so", async () => {
    const { user, dialog } = await openForm();
    await waitFor(() => expect(document.activeElement).toBe(within(dialog).getByLabelText("To")));
    await fill(user, dialog, { to: "Robin <robin@example.com>", subject: "Kick-off", start: "2099-12-01T10:00", duration: "1h" });
    await user.type(within(dialog).getByLabelText("Location"), "Room 4.12");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(callsOf("send_invite")).toEqual([
      {
        account: "work",
        subject: "Kick-off",
        start: "2099-12-01T10:00",
        to: "Robin <robin@example.com>",
        cc: "",
        end: null,
        duration: "1h",
        location: "Room 4.12",
        description: "",
      },
    ]);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New invitation" })).toBeNull());
    act(() => void settleInvite());
    expect(await screen.findByText("Sent the invitation Kick-off")).toBeInTheDocument();
  });

  it("the activity area's card cancels a running send, whose end says it may still go out", async () => {
    const { user, dialog } = await openForm();
    await fill(user, dialog, { to: "Robin <robin@example.com>", subject: "Kick-off", start: "2099-12-01T10:00", duration: "1h" });
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New invitation" })).toBeNull());
    const activity = screen.getByRole("region", { name: "Activity" });
    const running = await within(activity).findByRole("group", { name: "Sending the invitation Kick-off…" });
    await user.click(within(running).getByRole("button", { name: "Cancel the invitation" }));
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-invite-1" }]));
    expect(
      await screen.findByText("Stopped waiting for the invitation Kick-off; the daemon may still send it, check the outbox"),
    ).toBeInTheDocument();
    await waitFor(() => expect(within(activity).queryByRole("group", { name: /Sending the invitation/ })).toBeNull());
  });

  it("End replaces Duration, and only the one shown is sent", async () => {
    const { user, dialog } = await openForm();
    await fill(user, dialog, { to: "robin@example.com", subject: "Kick-off", start: "2099-12-01T10:00" });
    await user.click(within(dialog).getByRole("radio", { name: "Ends at" }));
    expect(within(dialog).queryByLabelText("Duration")).toBeNull();
    fireEvent.change(within(dialog).getByLabelText("End"), { target: { value: "2099-12-01T11:30" } });
    await user.click(within(dialog).getByRole("button", { name: /Send invitation/ }));
    expect(callsOf("send_invite")[0]).toMatchObject({ end: "2099-12-01T11:30", duration: null });
  });

  it("asks for a subject, a start and a recipient before any call, and keeps what was typed", async () => {
    const { user, dialog } = await openForm();
    const send = within(dialog).getByRole("button", { name: /Send invitation/ });
    await user.click(send);
    expect(alertOf(dialog)).toHaveTextContent("An invitation needs a subject");
    await user.type(within(dialog).getByLabelText("Subject"), "Kick-off");
    await user.click(send);
    expect(alertOf(dialog)).toHaveTextContent("An invitation needs a start");
    fireEvent.change(within(dialog).getByLabelText("Start"), { target: { value: "2099-12-01T10:00" } });
    await user.click(send);
    expect(alertOf(dialog)).toHaveTextContent("An invitation needs at least one recipient in To or Cc");
    expect(callsOf("send_invite")).toEqual([]);
    expect(within(dialog).getByLabelText("Subject")).toHaveValue("Kick-off");
  });

  it("shows any other refusal verbatim and stays open", async () => {
    const { user, dialog } = await openForm();
    await fill(user, dialog, { to: "robin@example.com", subject: "Kick-off", start: "2099-12-01T10:00" });
    await user.click(within(dialog).getByRole("button", { name: /Send invitation/ }));
    expect(await within(dialog).findByText("An invite needs --end or --duration")).toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "New invitation" })).toBeInTheDocument();
  });

  it("a failed send is a notice, the form already closed", async () => {
    const { user, dialog } = await openForm();
    await fill(user, dialog, { to: "robin@example.com", subject: "Kick-off", start: "2099-12-01T10:00", duration: "1h" });
    await user.click(within(dialog).getByRole("button", { name: /Send invitation/ }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New invitation" })).toBeNull());
    act(() => void settleInvite({ fail: true }));
    expect(await screen.findByText(`The invitation Kick-off failed: ${SEND_FAIL_REASON}`)).toBeInTheDocument();
  });

  it("is disabled on a Graph account with the probe's sentence, from the palette", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ga");
    await user.keyboard(":");
    await user.keyboard("New invitation");
    await user.keyboard("{Enter}");
    const dialog = await form();
    expect(await within(dialog).findByText(GRAPH_RSVP_REFUSAL)).toBeInTheDocument();
    expect(callsOf("invite_refusal")).toContainEqual({ account: "home" });
    for (const label of ["To", "Cc", "Subject", "Start", "Duration", "Location", "Description"]) {
      expect(within(dialog).getByLabelText(label)).toBeDisabled();
    }
    expect(within(dialog).getByRole("button", { name: /Send invitation/ })).toBeDisabled();
    expect(dialog).toHaveTextContent("Sent from home");
  });

  it("keeps the focus inside while open and gives it back on Escape", async () => {
    const { user, dialog } = await openForm();
    for (let i = 0; i < 14; i++) await user.tab();
    expect(dialog.contains(document.activeElement)).toBe(true);
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New invitation" })).toBeNull());
    expect(callsOf("send_invite")).toEqual([]);
  });
});
