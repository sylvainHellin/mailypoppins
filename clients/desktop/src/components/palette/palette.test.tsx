import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { GUI_ENTRIES, paletteEntries, SECTIONS } from "@/keymap/catalog";
import { mock } from "@/test/tauri-mock";

describe("the command palette", () => {
  it("lists every action of the generated keymap", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    const labels = new Set(
      [...dialog.querySelectorAll("[data-testid='palette-item']")].map((el) => el.getAttribute("data-label")),
    );
    const actions = new Set(SECTIONS.flatMap((s) => s.bindings.map((b) => b.action)));
    expect(actions.size).toBeGreaterThan(50);
    for (const a of actions) expect(labels).toContain(a);
  });

  it("marks what this build cannot run as disabled with its milestone", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    const reply = within(dialog).getAllByText("Reply")[0].closest("[data-testid='palette-item']");
    expect(reply).toHaveAttribute("data-disabled", "true");
    expect(reply).toHaveTextContent("M3");
  });

  it("runs every mutation action and no row is left for M2", () => {
    const entries = [...paletteEntries(), ...GUI_ENTRIES];
    expect(entries.filter((e) => (e.badge as string | null) === "M2")).toEqual([]);
    const ids = new Set(entries.map((e) => e.id));
    for (const id of [
      "archive",
      "delete",
      "move",
      "toggle_flag",
      "toggle_read",
      "mark_toggle",
      "mark_range",
      "mark_all",
      "mark_clear",
      "cancel_hold",
      "quick_sync",
      "full_sync",
    ] as const) {
      expect(ids).toContain(id);
    }
  });

  it("Archive from the palette asks to confirm, and confirming archives the cursor row", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    const archive = within(dialog).getAllByText("Archive")[0].closest("[data-testid='palette-item']") as HTMLElement;
    expect(archive).not.toHaveAttribute("data-disabled", "true");
    await user.click(archive);
    expect(await screen.findByRole("dialog", { name: "Archive this email?" })).toBeInTheDocument();
    await user.keyboard("y");
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "message_archive").map((c) => c.args)).toEqual([
        { account: "work", row_ids: [1001] },
      ]),
    );
  });

  it("Toggle flag/star and Quick sync run from the palette", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard(":");
    let dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Toggle flag/star"));
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "message_set_flag").map((c) => c.args)).toEqual([
        { account: "work", row_ids: [1001], flagged: true },
      ]),
    );
    await user.keyboard(":");
    dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Quick sync"));
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "sync_trigger").map((c) => c.args)).toEqual([{ account: "work", mode: "quick" }]),
    );
  });

  it("Cancel the held send runs from the palette", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Cancel the held send"));
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "send_cancel_hold").map((c) => c.args)).toEqual([
        { operation_id: "fixture-hold-seed" },
      ]),
    );
  });

  it("runs an enabled action", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await user.keyboard("Toggle this help");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("merges keys that share an action within a section", () => {
    const palette = paletteEntries().find((e) => e.label.startsWith("Command palette"));
    expect(palette?.keys).toEqual([":", "Ctrl+p"]);
  });
});
