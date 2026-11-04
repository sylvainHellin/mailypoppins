import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitAppEvent, invoke, mock, simulateReminder } from "@/test/tauri-mock";
import type { CalendarReminder } from "@/protocol/types";

const banner = () => screen.queryByRole("region", { name: "Reminders" });

/** Fire the fixture's reminder inside `act`, and answer it. */
function fire(): CalendarReminder {
  let fired: CalendarReminder | undefined;
  act(() => {
    fired = simulateReminder();
  });
  return fired!;
}

describe("the reminder banner", () => {
  it("lists a reminder as it fires, and Snooze takes it off through the daemon", async () => {
    const { user } = renderApp();
    await shellReady();
    expect(banner()).toBeNull();
    const fired = fire();
    const shown = await screen.findByRole("region", { name: "Reminders" });
    expect(shown).toHaveTextContent(fired.summary);
    await user.click(within(shown).getByRole("button", { name: `Snooze ${fired.summary} for 5 minutes` }));
    await waitFor(() => expect(banner()).toBeNull());
    expect(invoke).toHaveBeenCalledWith("reminder_snooze", { account: fired.account, event: fired.event, minutes: 5 });
    expect(mock.reminders).toEqual([]);
  });

  it("reads the waiting reminders at the bootstrap, and Dismiss takes one off", async () => {
    const { user } = renderApp(undefined, () => void simulateReminder());
    await shellReady();
    const shown = await screen.findByRole("region", { name: "Reminders" });
    const [waiting] = mock.reminders;
    await user.click(within(shown).getByRole("button", { name: `Dismiss ${waiting.summary}` }));
    await waitFor(() => expect(banner()).toBeNull());
    expect(invoke).toHaveBeenCalledWith("reminder_dismiss", { account: waiting.account, event: waiting.event });
  });

  it("opens the event in the Calendar view from the banner and from a notification click", async () => {
    const { user } = renderApp();
    await shellReady();
    const fired = fire();
    const shown = await screen.findByRole("region", { name: "Reminders" });
    await user.click(within(shown).getByRole("button", { name: /in the Calendar view/ }));
    await waitFor(() => expect(document.querySelector('[data-view="calendar"]')).not.toBeNull());

    await user.click(screen.getAllByRole("button", { name: /Mail/ })[0]);
    act(() => emitAppEvent("reminder:open", { account: fired.account, event: fired.event, start: fired.start }));
    await waitFor(() => expect(document.querySelector('[data-view="calendar"]')).not.toBeNull());
  });

  it("R focuses the first Snooze, and Enter and Space press the banner's buttons in the Mail view", async () => {
    const { user } = renderApp();
    await shellReady();
    const fired = fire();
    await screen.findByRole("region", { name: "Reminders" });
    await user.keyboard("R");
    expect(document.activeElement).toHaveAccessibleName(`Snooze ${fired.summary} for 5 minutes`);
    await user.keyboard("{Tab}");
    expect(document.activeElement).toHaveAccessibleName(`Dismiss ${fired.summary}`);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(banner()).toBeNull());
    expect(invoke).toHaveBeenCalledWith("reminder_dismiss", { account: fired.account, event: fired.event });
    expect(document.querySelector('[data-view="calendar"]')).toBeNull();

    const again = fire();
    await screen.findByRole("region", { name: "Reminders" });
    await user.keyboard("R");
    await user.keyboard(" ");
    await waitFor(() => expect(banner()).toBeNull());
    expect(invoke).toHaveBeenCalledWith("reminder_snooze", { account: again.account, event: again.event, minutes: 5 });
  });

  it("R focuses the first Snooze, and Enter presses it over a full-pane view", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(" a");
    await screen.findByRole("region", { name: "Calendar" });
    const fired = fire();
    await screen.findByRole("region", { name: "Reminders" });
    await user.keyboard("R");
    expect(document.activeElement).toHaveAccessibleName(`Snooze ${fired.summary} for 5 minutes`);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(banner()).toBeNull());
    expect(invoke).toHaveBeenCalledWith("reminder_snooze", { account: fired.account, event: fired.event, minutes: 5 });
    expect(screen.getByRole("region", { name: "Calendar" })).toBeInTheDocument();
  });

  it("raises a test notification from the palette's action", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await user.keyboard("test reminder");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(mock.testNotifications).toBe(1));
  });
});
