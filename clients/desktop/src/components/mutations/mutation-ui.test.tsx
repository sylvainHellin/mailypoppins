import { act, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, fixtures, mock } from "@/test/tauri-mock";
import { APPLIED_MS, HOLD_END_MS, HoldToast, NoticeToast } from "@/components/mutations/ActivityStack";
import type { ActivityNotice, HoldEntry } from "@/app/state";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
const row = (id: number) => document.querySelector<HTMLElement>(`[role="option"][data-row-id="${id}"]`)!;
const selectedRows = () =>
  [...document.querySelectorAll('[role="option"][aria-selected="true"]')].map((e) => Number(e.getAttribute("data-row-id")));
const seededHold = fixtures.bootstrap.snapshot.holds[0];

/** A promise the test settles, to hold a command's answer back. */
function gate() {
  let open = () => {};
  const promise = new Promise<void>((r) => (open = r));
  return { promise, open };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("row toggles and marks", () => {
  it("the flag and unread toggles act on their own row", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(within(row(1001)).getByRole("button", { name: "Flagged" }));
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1001], flagged: true }]));
    await waitFor(() => expect(within(row(1001)).getByRole("button", { name: "Flagged" })).toHaveAttribute("aria-pressed", "true"));
    const unread = within(row(1002)).getByRole("button", { name: "Unread" });
    expect(unread).toHaveAttribute("aria-pressed", "true");
    await user.click(unread);
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1002], read: true }]));
    // A toggle neither selects nor opens its row.
    expect(selectedRows()).toEqual([]);
  });

  it("marks rows by the box, Shift+click and Cmd+click, counts them and clears them", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(within(row(1001)).getByRole("checkbox", { name: "Mark" }));
    await user.click(within(row(1003)).getByRole("checkbox", { name: "Mark" }));
    expect(selectedRows()).toEqual([1001, 1003]);
    expect(screen.getByText("2 marked")).toBeInTheDocument();
    expect(within(row(1003)).getByRole("checkbox", { name: "Mark" })).toHaveAttribute("aria-checked", "true");

    await user.keyboard("{Shift>}");
    await user.click(within(row(1005)).getByRole("checkbox", { name: "Mark" }));
    await user.keyboard("{/Shift}");
    expect(selectedRows()).toEqual([1001, 1003, 1004, 1005]);

    await user.keyboard("{Meta>}");
    await user.click(row(1007));
    await user.keyboard("{/Meta}");
    expect(selectedRows()).toEqual([1001, 1003, 1004, 1005, 1007]);
    expect(screen.getByText("5 marked")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Clear marks" }));
    expect(screen.queryByText(/marked$/)).toBeNull();
    expect(selectedRows()).toEqual([]);
  });

  it("a row is aria-busy with the pending mark until its change is answered", async () => {
    const { user } = renderApp();
    await shellReady();
    const g = gate();
    mock.gates.set("message_set_flag", g.promise);
    await user.click(within(row(1003)).getByRole("button", { name: "Flagged" }));
    expect(row(1003)).toHaveAttribute("aria-busy", "true");
    expect(row(1003).querySelector('[data-slot="pending"]')).not.toBeNull();
    expect(row(1003)).toHaveAccessibleName(/change pending/);
    await act(async () => g.open());
    await waitFor(() => expect(row(1003)).not.toHaveAttribute("aria-busy"));
    expect(row(1003).querySelector('[data-slot="pending"]')).toBeNull();
  });
});

describe("the reader toolbar", () => {
  async function openSecond() {
    const view = renderApp();
    await shellReady();
    await view.user.keyboard("jj");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" });
    return { ...view, toolbar: within(reader).getByRole("toolbar", { name: "Message actions" }) };
  }

  it("marks read and flags the open message, and its labels follow", async () => {
    const { user, toolbar } = await openSecond();
    await user.click(within(toolbar).getByRole("button", { name: "Mark read" }));
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1002], read: true }]));
    expect(await within(toolbar).findByRole("button", { name: "Mark unread" })).toBeInTheDocument();
    await user.click(within(toolbar).getByRole("button", { name: "Flag" }));
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1002], flagged: true }]));
    expect(await within(toolbar).findByRole("button", { name: "Unflag" })).toBeInTheDocument();
  });

  it("acts on the open message only, whatever the list has marked", async () => {
    const { user, toolbar } = await openSecond();
    await user.click(within(row(1001)).getByRole("checkbox", { name: "Mark" }));
    await user.click(within(toolbar).getByRole("button", { name: "Flag" }));
    await waitFor(() => expect(callsOf("message_set_flag")).toEqual([{ account: "work", row_ids: [1002], flagged: true }]));
    expect(screen.getByText("1 marked")).toBeInTheDocument();
  });

  it("Archive asks first and archives on confirm", async () => {
    const { user, toolbar } = await openSecond();
    await user.click(within(toolbar).getByRole("button", { name: "Archive" }));
    const dialog = await screen.findByRole("dialog", { name: "Archive this email?" });
    expect(dialog).toHaveTextContent("Angebot Dachsanierung");
    await user.click(within(dialog).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(callsOf("message_archive")).toEqual([{ account: "work", row_ids: [1002] }]));
    await waitFor(() => expect(row(1002)).toBeNull());
  });

  it("Delete asks first, and Cancel deletes nothing", async () => {
    const { user, toolbar } = await openSecond();
    await user.click(within(toolbar).getByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("dialog", { name: "Delete this email?" });
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(callsOf("message_delete")).toEqual([]);
    expect(row(1002)).not.toBeNull();
  });

  it("Move opens the picker, which lists neither Drafts nor the current mailbox", async () => {
    const { user, toolbar } = await openSecond();
    await user.click(within(toolbar).getByRole("button", { name: "Move" }));
    const picker = await screen.findByRole("dialog", { name: "Move the message to" });
    const slugs = [...picker.querySelectorAll("[data-testid='move-destination']")].map((e) => e.getAttribute("data-slug"));
    expect(slugs).toEqual(["sent", "archive"]);
    await user.click(within(picker).getByText("Sent"));
    await waitFor(() => expect(callsOf("message_move")).toEqual([{ account: "work", row_ids: [1002], destination: "sent" }]));
  });
});

describe("mark read on an explicit open (MSG-08)", () => {
  const unreadPressed = (id: number) => within(row(id)).getByRole("button", { name: "Unread" }).getAttribute("aria-pressed");

  it("moving the cursor marks nothing; Enter on an unread row marks it read once", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jjj");
    await user.keyboard("k");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    await within(reader).findByRole("heading", { name: "Angebot Dachsanierung" });
    // The cursor walked over two unread rows and shows one of them.
    expect(callsOf("message_set_read")).toEqual([]);
    expect(unreadPressed(1003)).toBe("true");

    await user.keyboard("{Enter}");
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1002], read: true }]));
    await waitFor(() => expect(unreadPressed(1002)).toBe("false"));

    // Back to the list and into the reader again: the row is read, nothing more is sent.
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    await user.keyboard("{Tab}");
    expect(callsOf("message_set_read")).toHaveLength(1);
  });

  it("Tab into the reader and a double-click open an unread row and mark it read", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jjj");
    const reader = screen.getByRole("complementary", { name: "Reader" });
    await within(reader).findByRole("heading", { name: "This week in type: variable fonts" });
    expect(callsOf("message_set_read")).toEqual([]);
    await user.keyboard("{Tab}");
    await waitFor(() => expect(callsOf("message_set_read")).toEqual([{ account: "work", row_ids: [1003], read: true }]));

    await user.dblClick(row(1006));
    await waitFor(() => expect(callsOf("message_set_read")).toHaveLength(2));
    expect(callsOf("message_set_read")[1]).toEqual({ account: "work", row_ids: [1006], read: true });
    await waitFor(() => expect(unreadPressed(1006)).toBe("false"));
  });
});

describe("the move picker", () => {
  it("filters as the name is typed, and Escape cancels", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("jM");
    const picker = await screen.findByRole("dialog", { name: "Move the message to" });
    const input = within(picker).getByRole("combobox");
    await waitFor(() => expect(input).toHaveFocus());
    await user.keyboard("zzz");
    expect(await within(picker).findByText("No mailbox matches.")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(callsOf("message_move")).toEqual([]);
  });

  it("says why a Drafts row cannot move", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("jM");
    expect(await screen.findByText("Quick-move is not available in this mailbox")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("activity notices", () => {
  it("an applied batch shows a status, and Dismiss removes it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j*");
    const area = screen.getByRole("region", { name: "Activity" });
    const applied = (await within(area).findByText("Flagged 1 message")).closest<HTMLElement>("[role='status']")!;
    expect(applied).toHaveAttribute("data-notice", "applied");
    await user.click(within(applied).getByRole("button", { name: "Dismiss" }));
    expect(within(area).queryByText("Flagged 1 message")).toBeNull();
  });

  it("a failed command is an alert that stays, naming the rows put back", async () => {
    const { user } = renderApp();
    await shellReady();
    mock.failing.set("message_set_flag", { kind: "internal", message: "the daemon went away" });
    await user.keyboard("j*");
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/Could not flag 1 message/);
    expect(alert).toHaveTextContent(/Quarterly ledger review: the daemon went away/);
    await user.click(within(alert).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("a rollback and a failed sync are alerts", async () => {
    const { user } = renderApp();
    await shellReady();
    act(() => emitEnvelope("mutations.rolled_back", { account: "work", failed: 2 }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    mock.failing.set("sync_trigger", { kind: "internal", message: "no route" });
    await user.keyboard("ss");
    await waitFor(() => expect(screen.getAllByRole("alert")).toHaveLength(2));
    expect(screen.getAllByRole("alert")[1]).toHaveTextContent("Sync of work did not start: no route");
  });

  it("an applied notice leaves by itself after a few seconds, a failure does not", () => {
    vi.useFakeTimers();
    const onDismiss = vi.fn();
    const applied: ActivityNotice = { id: 1, kind: "applied", account: "work", text: "Archived 1 message", rows: [] };
    const failed: ActivityNotice = { id: 2, kind: "failed", account: "work", text: "Could not archive 1 message", rows: [] };
    render(
      <>
        <NoticeToast notice={applied} onDismiss={onDismiss} />
        <NoticeToast notice={failed} onDismiss={onDismiss} />
      </>,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Archived 1 message");
    expect(screen.getByRole("alert")).toHaveTextContent("Could not archive 1 message");
    act(() => vi.advanceTimersByTime(APPLIED_MS - 1));
    expect(onDismiss).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(onDismiss).toHaveBeenCalledWith(1);
    act(() => vi.advanceTimersByTime(60_000));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});

describe("held sends", () => {
  it("counts down from the daemon's ticks, and Cancel cancels it", async () => {
    const { user } = renderApp();
    await shellReady();
    const hold = await screen.findByRole("group", { name: `Held send: ${seededHold.subject}` });
    expect(hold).toHaveTextContent("Sending in 60 s");
    act(() => emitEnvelope("send.hold_tick", { ...seededHold, remaining_secs: 42 }));
    expect(hold).toHaveTextContent("Sending in 42 s");
    expect(within(hold).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "42");

    await user.click(within(hold).getByRole("button", { name: "Cancel send" }));
    expect(callsOf("send_cancel_hold")).toEqual([{ operation_id: "fixture-hold-seed" }]);
    await waitFor(() => expect(hold).toHaveTextContent("Send cancelled"));
    expect(within(hold).queryByRole("button", { name: "Cancel send" })).toBeNull();
    // The hold's toast says it; its notice is not shown twice.
    expect(screen.queryByText(/Send of .* cancelled/)).toBeNull();
  });

  it("a fired hold says Sent", async () => {
    renderApp();
    await shellReady();
    const hold = await screen.findByRole("group", { name: `Held send: ${seededHold.subject}` });
    act(() => emitEnvelope("send.hold_fired", { ...seededHold, remaining_secs: 0 }));
    expect(hold).toHaveTextContent("Sent");
  });

  it("Cancel is disabled while this window's cancel is in flight", () => {
    const entry: HoldEntry = { ...seededHold, remaining_secs: 12, state: "tick", cancelling: true };
    render(<HoldToast hold={entry} onCancel={() => {}} onGone={() => {}} />);
    const cancel = screen.getByRole("button", { name: "Cancel send" });
    expect(cancel).toBeDisabled();
    expect(cancel).toHaveTextContent("Cancelling");
  });

  it("an ended hold leaves after a moment", () => {
    vi.useFakeTimers();
    const onGone = vi.fn();
    const entry: HoldEntry = { ...seededHold, remaining_secs: 0, state: "fired", cancelling: false };
    render(<HoldToast hold={entry} onCancel={() => {}} onGone={onGone} />);
    expect(screen.getByRole("status")).toHaveTextContent("Sent");
    act(() => vi.advanceTimersByTime(HOLD_END_MS));
    expect(onGone).toHaveBeenCalledWith("fixture-hold-seed");
  });
});
