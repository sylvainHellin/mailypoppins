import { screen, waitFor, within } from "@testing-library/react";
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
