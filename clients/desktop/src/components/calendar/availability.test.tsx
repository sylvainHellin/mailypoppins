import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock } from "@/test/tauri-mock";
import { AVAILABILITY_DEBOUNCE_MS } from "@/app/availability";
import type { CalendarFreeBusyParams } from "@/protocol/types";

const asked = () => mock.calls.filter((c) => c.cmd === "calendar_freebusy").map((c) => (c.args as { params: CalendarFreeBusyParams }).params);
const rowOf = (grid: HTMLElement, address: string) => grid.querySelector(`[data-slot="availability-row"][data-address="${address}"]`);

// The grid asks once its query has stood still for AVAILABILITY_DEBOUNCE_MS
// of wall time, which typing on a loaded machine can spend between two
// keystrokes (the query stands still while ", someone@gmail" is typed), so
// every test holds setTimeout still before the form opens and moves the
// clock itself with `settle`. Under the held clock the input goes through
// fireEvent: user-event's async wrapper waits on a setTimeout that only
// jest's fake timers would advance, and hangs under vitest's.

/** Hold setTimeout still until the test's end. */
function holdClock() {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
}

/** Move the held clock by `ms`, then let the answers it asked for land. */
async function settle(ms: number) {
  await act(() => vi.advanceTimersByTimeAsync(ms));
}

/** Type `text` at the end of `field` a keystroke at a time, the held clock not moving between them. */
async function typeInto(field: HTMLElement, text: string) {
  const input = field as HTMLInputElement;
  for (const char of text) {
    fireEvent.change(input, { target: { value: input.value + char } });
    await settle(0);
  }
}

afterEach(() => {
  vi.useRealTimers();
});

async function openForm(before?: () => void) {
  const r = renderApp(1400, before);
  await shellReady();
  await r.user.keyboard(" a");
  await screen.findByRole("region", { name: "Calendar" });
  holdClock();
  fireEvent.click(screen.getByRole("button", { name: "New event" }));
  await settle(0);
  return { dialog: screen.getByRole("dialog", { name: /New event/ }) };
}

describe("the event form's availability grid", () => {
  it("asks calendar_freebusy once the typed attendee stands still and shows a row per person, the chosen time outlined", async () => {
    const { dialog } = await openForm();
    await typeInto(within(dialog).getByLabelText("Attendees"), "robin@example.com, someone@gmail.com");
    const grid = within(dialog).getByRole("region", { name: "Availability" });
    // Typing asked nothing per keystroke, and the finished query is asked
    // once it has stood still for the debounce, not before.
    expect(asked()).toEqual([]);
    await settle(AVAILABILITY_DEBOUNCE_MS - 1);
    expect(asked()).toEqual([]);
    await settle(1);
    const calls = asked();
    expect(calls.map((c) => c.addresses)).toEqual([["robin@example.com", "someone@gmail.com"]]);
    expect(rowOf(grid, "robin@example.com")).not.toBeNull();
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
    const { dialog } = await openForm();
    const attendees = within(dialog).getByLabelText("Attendees");
    await typeInto(attendees, "robin@example.com");
    const grid = within(dialog).getByRole("region", { name: "Availability" });
    await settle(AVAILABILITY_DEBOUNCE_MS);
    expect(rowOf(grid, "robin@example.com")).not.toBeNull();
    const body = () => grid.querySelector('[data-slot="availability-rows"]')!;
    expect(body().hasAttribute("data-stale")).toBe(false);
    expect(grid.getAttribute("aria-busy")).toBe("false");

    await typeInto(attendees, ", ada@example.org");
    // The new query waits out the debounce; the old rows stand meanwhile.
    expect(rowOf(grid, "robin@example.com")).not.toBeNull();
    expect(rowOf(grid, "ada@example.org")).toBeNull();
    expect(body().hasAttribute("data-stale")).toBe(true);
    expect(grid.getAttribute("aria-busy")).toBe("true");
    expect(within(grid).getByText("(checking…)")).toBeInTheDocument();

    await settle(AVAILABILITY_DEBOUNCE_MS);
    expect(rowOf(grid, "ada@example.org")).not.toBeNull();
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
    holdClock();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "E" });
    await settle(0);
    const dialog = screen.getByRole("dialog", { name: "Edit event" });
    const grid = within(dialog).getByRole("region", { name: "Availability" });
    expect(asked()).toEqual([]);
    await settle(AVAILABILITY_DEBOUNCE_MS);
    expect(rowOf(grid, "robin@example.com")).not.toBeNull();
    const first = asked();
    expect(first).toHaveLength(1);
    expect(first[0]).toMatchObject({ account: "home", addresses: ["robin@example.com"], start: "2099-10-14", end: "2099-10-14" });

    // 10:00 to 10:30 is the meeting itself: busy on the user's row, ringed
    // on every row, and chosen while the time is unchanged.
    const marks = (address: string, attr: string) =>
      [...rowOf(grid, address)!.querySelectorAll("[data-status]")].map((c, n) => (c.hasAttribute(attr) ? n : null)).filter((n) => n !== null);
    const own = [...rowOf(grid, "me@example.com")!.querySelectorAll("[data-status]")];
    expect(own[6].getAttribute("data-status")).toBe("busy");
    expect(own[6].getAttribute("data-tip")).toBe("10:00 busy, this event");
    fireEvent.mouseEnter(own[6]);
    expect(within(grid).getByRole("tooltip")).toHaveTextContent("You (me@example.com)10:00 to 10:30: busy, this event");
    expect(marks("me@example.com", "data-current")).toEqual([6]);
    expect(marks("robin@example.com", "data-current")).toEqual([6]);
    expect(marks("me@example.com", "data-chosen")).toEqual([6]);
    expect(within(grid).getByText("this event")).toBeInTheDocument();

    // Moving it to 11:00 moves the outline; the ring stays on the time it holds now.
    const start = within(dialog).getByLabelText("Start");
    const end = within(dialog).getByLabelText("End");
    fireEvent.change(start, { target: { value: "2099-10-14T11:00" } });
    fireEvent.change(end, { target: { value: "2099-10-14T11:30" } });
    expect(marks("me@example.com", "data-chosen")).toEqual([8]);
    expect(marks("me@example.com", "data-current")).toEqual([6]);

    // A new attendee asks again after the same debounce.
    await typeInto(within(dialog).getByLabelText("Attendees"), ", ada@example.org");
    expect(asked()).toHaveLength(1);
    await settle(AVAILABILITY_DEBOUNCE_MS);
    expect(rowOf(grid, "ada@example.org")).not.toBeNull();
    expect(asked().map((c) => c.addresses)).toEqual([["robin@example.com"], ["robin@example.com", "ada@example.org"]]);
  });

  it("is not shown on a calendar whose backend serves no free/busy", async () => {
    const { dialog } = await openForm(() => {
      mock.calendars = mock.calendars.map((c) => ({ ...c, features: c.features.filter((f) => f !== "free-busy") }));
    });
    await typeInto(within(dialog).getByLabelText("Attendees"), "robin@example.com");
    await settle(AVAILABILITY_DEBOUNCE_MS * 2);
    expect(within(dialog).queryByRole("region", { name: "Availability" })).toBeNull();
    expect(asked()).toHaveLength(0);
  });
});
