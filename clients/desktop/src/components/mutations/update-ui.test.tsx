// The app update through the rendered window (ticket 0139): the sidebar entry,
// the palette rows, the manual check from the palette, the App menu and
// Settings, the activity card fed by the install's channel, the restart and
// the failure. The command layer is the mock's (src/test/tauri-mock.ts).

import { act, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitAppEvent, mock, settleUpdateInstall, updateProgress } from "@/test/tauri-mock";
import { RELEASE_PAGE } from "@/app/updates";
import { terms } from "@/test/xterm-fake";

vi.mock("@xterm/xterm", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-fit", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-webgl", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-unicode11", () => import("@/test/xterm-fake"));
vi.mock("@xterm/addon-clipboard", () => import("@/test/xterm-fake"));

type User = ReturnType<typeof renderApp>["user"];

const views = () => within(screen.getByRole("navigation", { name: "Accounts and mailboxes" })).getByRole("list", { name: "Views" });
const entry = (name: string | RegExp) => within(views()).queryByRole("button", { name });
const card = (name: string | RegExp) => within(screen.getByRole("region", { name: "Activity" })).queryByRole("group", { name });
/** The card named `name`, once it shows. */
const findCard = (name: string | RegExp) =>
  waitFor(() => {
    const c = card(name);
    expect(c).not.toBeNull();
    return c as HTMLElement;
  });
const calls = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

beforeEach(() => {
  terms.length = 0;
});

async function ready() {
  await shellReady();
  await waitFor(() => expect(mock.listeners.has("update:available")).toBe(true));
}

async function announce(version = "0.12.0") {
  act(() => emitAppEvent("update:available", { version }));
  await waitFor(() => expect(entry(`Update to ${version}`)).not.toBeNull());
}

/** The palette's labels as shown. */
async function paletteLabels(user: User): Promise<string[]> {
  await user.keyboard(":");
  const dialog = await screen.findByRole("dialog", { name: "Command palette" });
  const labels = [...dialog.querySelectorAll("[data-testid='palette-item']")].map((el) => el.getAttribute("data-label") ?? "");
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull());
  return labels;
}

/** Run a palette row by typing its name and pressing Enter, as the keyboard does. */
async function runFromPalette(user: User, label: string) {
  await user.keyboard(":");
  const dialog = await screen.findByRole("dialog", { name: "Command palette" });
  await user.keyboard(label);
  await waitFor(() =>
    expect(dialog.querySelector("[data-testid='palette-item'][data-selected='true']")).toHaveAttribute("data-label", label),
  );
  await user.keyboard("{Enter}");
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull());
}

/** Start the install from the sidebar entry and report a 4 MB archive. */
async function startInstall(user: User) {
  await user.click(entry("Update to 0.12.0") as HTMLElement);
  await waitFor(() => expect(mock.updateInstalls).toHaveLength(1));
  act(() => updateProgress({ type: "started", content_length: 4_000_000 }));
}

describe("the sidebar entry and the palette rows", () => {
  it("show nothing until an update is known, then Update to the version", async () => {
    const { user } = renderApp();
    await ready();
    expect(entry(/^Update to/)).toBeNull();
    let labels = await paletteLabels(user);
    expect(labels).toContain("Check for updates");
    expect(labels.some((l) => l.startsWith("Update to"))).toBe(false);
    expect(labels).not.toContain("Restart to finish the update");

    await announce();
    labels = await paletteLabels(user);
    expect(labels).toContain("Check for updates");
    expect(labels).toContain("Update to v0.12.0");
    expect(labels).not.toContain("Restart to finish the update");
  });

  it("the palette's Update to row starts the install", async () => {
    const { user } = renderApp();
    await ready();
    await announce();
    await runFromPalette(user, "Update to v0.12.0");
    await waitFor(() => expect(card("Downloading mailypoppins 0.12.0…")).not.toBeNull());
    expect(calls("update_install")).toHaveLength(1);
  });
});

describe("a manual check", () => {
  it("from the palette says the app is up to date", async () => {
    const { user } = renderApp();
    await ready();
    mock.updateCheck = { state: "up_to_date", current: "0.11.0", last_check: "2026-10-05T12:00:00Z" };
    await runFromPalette(user, "Check for updates");
    expect(await screen.findByText("mailypoppins 0.11.0 is up to date")).toBeInTheDocument();
    expect(calls("update_check")).toContainEqual({ manual: true });
    expect(entry(/^Update to/)).toBeNull();
  });

  it("from the palette finds an update, and the sidebar entry appears", async () => {
    const { user } = renderApp();
    await ready();
    mock.updateCheck = { state: "available", current: "0.11.0", version: "0.12.0" };
    await runFromPalette(user, "Check for updates");
    expect(await screen.findByText("Update 0.12.0 available")).toBeInTheDocument();
    expect(entry("Update to 0.12.0")).not.toBeNull();
  });

  it("from the App menu runs the same check", async () => {
    renderApp();
    await ready();
    await waitFor(() => expect(mock.listeners.has("update:check_requested")).toBe(true));
    mock.updateCheck = { state: "available", current: "0.11.0", version: "0.12.0" };
    act(() => emitAppEvent("update:check_requested", null));
    expect(await screen.findByText("Update 0.12.0 available")).toBeInTheDocument();
    expect(calls("update_check")).toEqual([{ manual: true }]);
    expect(entry("Update to 0.12.0")).not.toBeNull();
  });

  it("says why a build does not check, and why a check failed", async () => {
    const { user } = renderApp();
    await ready();
    // The mock's default answer is fixture mode's gate.
    act(() => emitAppEvent("update:check_requested", null));
    expect(await screen.findByText("Updates are off in fixture mode.")).toBeInTheDocument();
    mock.updateCheck = { state: "failed", current: "0.11.0", reason: "error sending request" };
    await runFromPalette(user, "Check for updates");
    expect(await screen.findByText("The update check failed: error sending request")).toBeInTheDocument();
  });
});

describe("the install", () => {
  it("shows the download on a running card, then asks Restart now or Later", async () => {
    const { user } = renderApp();
    await ready();
    await announce();
    await startInstall(user);
    const downloading = card("Downloading mailypoppins 0.12.0…") as HTMLElement;
    expect(downloading).not.toBeNull();
    // Nothing to choose while it downloads.
    expect(entry(/^Update to/)).toBeNull();
    act(() => updateProgress({ type: "progress", downloaded: 1_000_000, content_length: 4_000_000 }));
    const bar = within(downloading).getByRole("progressbar", { name: "Update download" });
    expect(bar).toHaveAttribute("aria-valuenow", "1000000");
    expect(bar).toHaveAttribute("aria-valuemax", "4000000");
    expect(downloading).toHaveTextContent("1.0 MB of 4.0 MB");

    act(() => settleUpdateInstall());
    const installed = await findCard("mailypoppins 0.12.0 is installed");
    expect(within(installed).getByRole("status")).toHaveTextContent("mailypoppins 0.12.0 is installed; restart to finish the update.");
    expect(within(installed).getByRole("button", { name: "Restart now" })).toBeEnabled();
    expect(within(installed).getByRole("button", { name: "Later" })).toBeEnabled();
  });

  it("Later takes the card away, and the sidebar entry and the palette offer the restart", async () => {
    const { user } = renderApp();
    await ready();
    await announce();
    await startInstall(user);
    act(() => settleUpdateInstall());
    const installed = await findCard("mailypoppins 0.12.0 is installed");
    await user.click(within(installed).getByRole("button", { name: "Later" }));
    await waitFor(() => expect(card("mailypoppins 0.12.0 is installed")).toBeNull());
    expect(calls("update_restart")).toEqual([]);
    const restart = entry("Restart to finish the update") as HTMLElement;
    expect(restart).not.toBeNull();
    const labels = await paletteLabels(user);
    expect(labels).toContain("Restart to finish the update");
    expect(labels.some((l) => l.startsWith("Update to"))).toBe(false);
    await user.click(restart);
    await waitFor(() => expect(calls("update_restart")).toHaveLength(1));
  });

  it("Restart now restarts at once with no editor open", async () => {
    const { user } = renderApp();
    await ready();
    await announce();
    await startInstall(user);
    act(() => settleUpdateInstall());
    const installed = await findCard("mailypoppins 0.12.0 is installed");
    await user.click(within(installed).getByRole("button", { name: "Restart now" }));
    await waitFor(() => expect(calls("update_restart")).toHaveLength(1));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("Restart now asks the leave question first while an embedded editor runs", async () => {
    const { user } = renderApp(1400, () => mock.settings.set("editor", "nvim"));
    await ready();
    await announce();
    // Reply to the first inbox message in the embedded editor.
    await user.keyboard("j");
    await user.keyboard("r");
    await waitFor(() => expect(document.querySelector('[data-slot="editing-banner"] [data-editing]')).toHaveAttribute("data-status", "editing"));
    await startInstall(user);
    act(() => settleUpdateInstall());
    const installed = await findCard("mailypoppins 0.12.0 is installed");
    await user.click(within(installed).getByRole("button", { name: "Restart now" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart to finish the update?" });
    expect(dialog).toHaveTextContent("fixture-draft-1.md is still open in the editor");
    expect(calls("update_restart")).toEqual([]);
    await user.click(within(dialog).getByRole("button", { name: "Stay" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls("update_restart")).toEqual([]);

    await user.click(within(installed).getByRole("button", { name: "Restart now" }));
    const again = await screen.findByRole("dialog", { name: "Restart to finish the update?" });
    await user.click(within(again).getByRole("button", { name: "Close the editor and restart" }));
    await waitFor(() => expect(calls("update_restart")).toHaveLength(1));
    expect(mock.terminal.of("kill").map((k) => k.session)).toContain(1);
  });

  it("a failed install says why, opens the release page, and can be tried again", async () => {
    const { user } = renderApp();
    await ready();
    await announce();
    await startInstall(user);
    act(() => settleUpdateInstall({ fail: "the signature does not match" }));
    const failed = await findCard("The update to 0.12.0 failed");
    expect(failed).toHaveTextContent("The update to 0.12.0 failed: the signature does not match");
    // The update can be installed again while the card shows.
    expect(entry("Update to 0.12.0")).not.toBeNull();
    await user.click(within(failed).getByRole("button", { name: "Open the release page" }));
    await waitFor(() => expect(calls("open_external")).toEqual([{ url: RELEASE_PAGE }]));
    await user.click(within(failed).getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(card("The update to 0.12.0 failed")).toBeNull());
    await user.click(entry("Update to 0.12.0") as HTMLElement);
    await waitFor(() => expect(calls("update_install")).toHaveLength(2));
    expect(card("Downloading mailypoppins 0.12.0…")).not.toBeNull();
  });
});

describe("a window that starts or reloads", () => {
  it("offers the restart for an update this run already installed", async () => {
    const { user } = renderApp(1400, () => {
      mock.updateStatus = { current: "0.11.0", enabled: true, installed: "0.12.0", available: "0.13.0" };
    });
    await ready();
    await waitFor(() => expect(entry("Restart to finish the update")).not.toBeNull());
    expect(entry(/^Update to/)).toBeNull();
    // No card asks: the window only learned of the update, it did not watch it install.
    expect(card("mailypoppins 0.12.0 is installed")).toBeNull();
    expect(calls("update_check")).toEqual([]);
    await user.click(entry("Restart to finish the update") as HTMLElement);
    await waitFor(() => expect(calls("update_restart")).toHaveLength(1));
  });

  it("offers an update the Rust layer already holds", async () => {
    const { user } = renderApp(1400, () => {
      mock.updateStatus = { current: "0.11.0", enabled: true, available: "0.12.0" };
    });
    await ready();
    await waitFor(() => expect(entry("Update to 0.12.0")).not.toBeNull());
    expect(calls("update_check")).toEqual([]);
    await user.click(entry("Update to 0.12.0") as HTMLElement);
    await waitFor(() => expect(mock.updateInstalls).toHaveLength(1));
  });
});

describe("the Settings line", () => {
  it("reads the running version without a check, and Check now checks on the notice line", async () => {
    const { user } = renderApp(1400, () => {
      mock.updateStatus = { current: "0.11.0", enabled: true, last_check: "2026-10-05T12:00:00Z" };
    });
    await ready();
    mock.updateCheck = { state: "up_to_date", current: "0.11.0", last_check: "2026-10-05T12:00:00Z" };
    await user.click(entry("Settings") as HTMLElement);
    const settings = await screen.findByRole("region", { name: "Settings" });
    const line = await within(settings).findByText(/^mailypoppins 0\.11\.0, last checked /);
    expect(line).toBeInTheDocument();
    expect(calls("update_status").length).toBeGreaterThanOrEqual(2);
    expect(calls("update_check")).toEqual([]);
    expect(settings.querySelector("[data-slot='update-reason']")).toBeNull();
    expect(screen.queryByText("mailypoppins 0.11.0 is up to date")).toBeNull();

    await user.click(within(settings).getByRole("button", { name: "Check now" }));
    expect(await screen.findByText("mailypoppins 0.11.0 is up to date")).toBeInTheDocument();
    expect(calls("update_check")).toEqual([{ manual: true }]);
  });

  it("says why a build never checks, and Check now still answers", async () => {
    const { user } = renderApp();
    await ready();
    await user.click(entry("Settings") as HTMLElement);
    const settings = await screen.findByRole("region", { name: "Settings" });
    expect(await within(settings).findByText("mailypoppins 0.1.0, not checked yet")).toBeInTheDocument();
    expect(settings.querySelector("[data-slot='update-reason']")).toHaveTextContent("Updates are off in fixture mode.");
    expect(calls("update_check")).toEqual([]);

    await user.click(within(settings).getByRole("button", { name: "Check now" }));
    await waitFor(() => expect(calls("update_check")).toEqual([{ manual: true }]));
    // The check's answer replaces the status's: the mock's check says 0.11.0.
    expect(await within(settings).findByText("mailypoppins 0.11.0, not checked yet")).toBeInTheDocument();
    expect(settings.querySelector("[data-slot='update-reason']")).toHaveTextContent("Updates are off in fixture mode.");
  });
});
