import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock } from "@/test/tauri-mock";
import type { CalendarFreeBusyParams } from "@/protocol/types";

const asked = () => mock.calls.filter((c) => c.cmd === "calendar_freebusy").map((c) => (c.args as { params: CalendarFreeBusyParams }).params);
const rowOf = (grid: HTMLElement, address: string) => grid.querySelector(`[data-slot="availability-row"][data-address="${address}"]`);

async function openForm(before?: () => void) {
  const r = renderApp(1400, before);
  await shellReady();
  await r.user.keyboard(" a");
  await screen.findByRole("region", { name: "Calendar" });
  await r.user.click(screen.getByRole("button", { name: "New event" }));
  const dialog = await screen.findByRole("dialog", { name: /New event/ });
  return { ...r, dialog };
}

describe("the event form's availability grid", () => {
  it("asks calendar_freebusy once the typed attendee stands still and shows a row per person, the chosen time outlined", async () => {
    const { user, dialog } = await openForm();
    await user.type(within(dialog).getByLabelText("Attendees"), "robin@example.com, someone@gmail.com");
    const grid = await within(dialog).findByRole("region", { name: "Availability" });
    await waitFor(() => expect(rowOf(grid, "robin@example.com")).not.toBeNull(), { timeout: 3000 });
    // Typing asked nothing per keystroke: every query is the empty form's
    // or the finished one's.
    const calls = asked();
    expect(calls.length).toBeGreaterThan(0);
    for (const call of calls) {
      expect([[], ["robin@example.com", "someone@gmail.com"]]).toContainEqual(call.addresses);
    }
    const last = calls[calls.length - 1];
    expect(last).toMatchObject({ account: "home", start: last.end, interval_minutes: 30, zone: "Europe/Berlin" });

    const own = rowOf(grid, "me@example.com")!;
    expect(own).not.toBeNull();
    const robin = rowOf(grid, "robin@example.com")!;
    const cells = [...robin.querySelectorAll("[data-status]")];
    expect(cells).toHaveLength(26);
    // Busy 10:00 to 11:00 (cells 6 and 7), tentative 15:00 to 16:00 (16, 17).
    expect(cells.slice(5, 9).map((c) => c.getAttribute("data-status"))).toEqual(["free", "busy", "busy", "free"]);
    expect(cells.slice(16, 18).map((c) => c.getAttribute("data-status"))).toEqual(["tentative", "tentative"]);
    // The form's 09:00 to 10:00 is outlined.
    const chosen = cells.map((c, n) => (c.hasAttribute("data-chosen") ? n : null)).filter((n) => n !== null);
    expect(chosen).toEqual([4, 5]);
    expect(within(rowOf(grid, "someone@gmail.com") as HTMLElement).getByText("no free/busy")).toBeInTheDocument();
  });

  it("keeps the last answer's rows, dimmed, while a changed query is asked again", async () => {
    const { user, dialog } = await openForm();
    const attendees = within(dialog).getByLabelText("Attendees");
    await user.type(attendees, "robin@example.com");
    const grid = await within(dialog).findByRole("region", { name: "Availability" });
    await waitFor(() => expect(rowOf(grid, "robin@example.com")).not.toBeNull(), { timeout: 3000 });
    const body = () => grid.querySelector('[data-slot="availability-rows"]')!;
    expect(body().hasAttribute("data-stale")).toBe(false);
    expect(grid.getAttribute("aria-busy")).toBe("false");

    await user.type(attendees, ", ada@example.org");
    // The new query waits out the debounce; the old rows stand meanwhile.
    expect(rowOf(grid, "robin@example.com")).not.toBeNull();
    expect(rowOf(grid, "ada@example.org")).toBeNull();
    expect(body().hasAttribute("data-stale")).toBe(true);
    expect(grid.getAttribute("aria-busy")).toBe("true");
    expect(within(grid).getByText("(checking…)")).toBeInTheDocument();

    await waitFor(() => expect(rowOf(grid, "ada@example.org")).not.toBeNull(), { timeout: 3000 });
    expect(body().hasAttribute("data-stale")).toBe(false);
    expect(rowOf(grid, "robin@example.com")).not.toBeNull();
  });

  it("shows on the edit form too, asking as the create form does, the event's current time ringed beside the chosen one", async () => {
    const r = renderApp(1400);
    await shellReady();
    await r.user.keyboard(" a");
    await screen.findByRole("region", { name: "Calendar" });
    await r.user.keyboard("gt");
    await waitFor(() => expect(document.activeElement).toHaveAttribute("aria-label", "Jump to date"));
    await r.user.keyboard("2099-10-14{Enter}");
    const list = await screen.findByRole("listbox", { name: "Agenda" });
    await within(list).findByRole("option", { name: /1:1 with Robin/ });
    const cursor = () => document.querySelector('[role="option"][aria-selected="true"]')?.getAttribute("data-event-id");
    for (let i = 0; i < 6 && cursor() !== "AAMkAD-robin"; i++) await r.user.keyboard("j");
    expect(cursor()).toBe("AAMkAD-robin");
    await r.user.keyboard("E");
    const dialog = await screen.findByRole("dialog", { name: "Edit event" });
    const grid = await within(dialog).findByRole("region", { name: "Availability" });
    await waitFor(() => expect(rowOf(grid, "robin@example.com")).not.toBeNull(), { timeout: 3000 });
    const first = asked();
    expect(first[first.length - 1]).toMatchObject({ account: "home", addresses: ["robin@example.com"], start: "2099-10-14", end: "2099-10-14" });

    // 10:00 to 10:30 is the meeting itself: busy on the user's row, ringed
    // on every row, and chosen while the time is unchanged.
    const marks = (address: string, attr: string) =>
      [...rowOf(grid, address)!.querySelectorAll("[data-status]")].map((c, n) => (c.hasAttribute(attr) ? n : null)).filter((n) => n !== null);
    const own = [...rowOf(grid, "me@example.com")!.querySelectorAll("[data-status]")];
    expect(own[6].getAttribute("data-status")).toBe("busy");
    expect(own[6].getAttribute("title")).toBe("10:00 busy, this event");
    expect(marks("me@example.com", "data-current")).toEqual([6]);
    expect(marks("robin@example.com", "data-current")).toEqual([6]);
    expect(marks("me@example.com", "data-chosen")).toEqual([6]);
    expect(within(grid).getByText("this event")).toBeInTheDocument();

    // Moving it to 11:00 moves the outline; the ring stays on the time it holds now.
    const start = within(dialog).getByLabelText("Start");
    const end = within(dialog).getByLabelText("End");
    fireEvent.change(start, { target: { value: "2099-10-14T11:00" } });
    fireEvent.change(end, { target: { value: "2099-10-14T11:30" } });
    await waitFor(() => expect(marks("me@example.com", "data-chosen")).toEqual([8]));
    expect(marks("me@example.com", "data-current")).toEqual([6]);

    // A new attendee asks again after the same debounce.
    const before = asked().length;
    await r.user.type(within(dialog).getByLabelText("Attendees"), ", ada@example.org");
    expect(asked().length).toBe(before);
    await waitFor(() => expect(rowOf(grid, "ada@example.org")).not.toBeNull(), { timeout: 3000 });
    expect(asked()[asked().length - 1].addresses).toEqual(["robin@example.com", "ada@example.org"]);
  });

  it("is not shown on a calendar whose backend serves no free/busy", async () => {
    const { user, dialog } = await openForm(() => {
      mock.calendars = mock.calendars.map((c) => ({ ...c, features: c.features.filter((f) => f !== "free-busy") }));
    });
    await user.type(within(dialog).getByLabelText("Attendees"), "robin@example.com");
    await new Promise((resolve) => setTimeout(resolve, 700));
    expect(within(dialog).queryByRole("region", { name: "Availability" })).toBeNull();
    expect(asked()).toHaveLength(0);
  });
});
