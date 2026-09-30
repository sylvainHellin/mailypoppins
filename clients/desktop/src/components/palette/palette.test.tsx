import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { GUI_ENTRIES, paletteEntries, SECTIONS } from "@/keymap/catalog";
import { emitEnvelope, fixtures, mock } from "@/test/tauri-mock";

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
    const attach = within(dialog).getByText("Attach file to draft (Drafts only)").closest("[data-testid='palette-item']");
    expect(attach).toHaveAttribute("data-disabled", "true");
    expect(attach).toHaveTextContent("M3");
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
      "dismiss_notice",
      "dismiss_all_notices",
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

  it("Cancel the held send says so when no send is held", async () => {
    const { user } = renderApp();
    await shellReady();
    act(() => emitEnvelope("send.hold_fired", { ...fixtures.bootstrap.snapshot.holds[0], remaining_secs: 0 }));
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Cancel the held send"));
    expect(await screen.findByText("No send is being held")).toBeInTheDocument();
    expect(mock.calls.filter((c) => c.cmd === "send_cancel_hold")).toEqual([]);
  });

  it("Dismiss the newest notice and Dismiss all notices run from the palette", async () => {
    const { user } = renderApp();
    await shellReady();
    for (const failed of [1, 2, 3]) act(() => emitEnvelope("mutations.rolled_back", { account: "work", failed }));
    await waitFor(() => expect(screen.getAllByRole("alert")).toHaveLength(3));
    await user.keyboard(":");
    let dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Dismiss the newest notice"));
    await waitFor(() => expect(screen.getAllByRole("alert")).toHaveLength(2));
    await user.keyboard(":");
    dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Dismiss all notices"));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
  });

  it("runs an enabled action", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await user.keyboard("Toggle this help");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("the key help lists the desktop client's own keys after the TUI's", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("?");
    const help = await screen.findByRole("dialog", { name: "Keys" });
    const desktop = within(help).getByRole("region", { name: "DESKTOP" });
    expect(desktop).toHaveTextContent("Cancel the held send");
    expect(desktop).toHaveTextContent("Mark range");
    expect(desktop).toHaveTextContent("Dismiss the newest notice");
    // The M2 rows lost their badge.
    const archive = within(help).getAllByText("Archive")[0].closest("tr");
    expect(archive).not.toHaveTextContent("M2");
  });

  it("runs every compose and send action and keeps only the attach row for later in M3", () => {
    const entries = [...paletteEntries(), ...GUI_ENTRIES];
    const ids = new Set(entries.map((e) => e.id));
    for (const id of [
      "new_draft",
      "reply",
      "reply_all",
      "forward",
      "open_editor",
      "edit_recipients",
      "approve",
      "demote",
      "send",
      "send_all",
    ] as const) {
      expect(ids).toContain(id);
    }
    expect(entries.filter((e) => e.badge === "M3").map((e) => e.label)).toEqual(["Attach file to draft (Drafts only)"]);
  });

  it("New draft, Reply all and Approve draft run from the palette", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard(":");
    let dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Reply all"));
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "draft_reply").map((c) => c.args)).toEqual([
        { account: "work", row_id: 1001, all: true, headers: null },
      ]),
    );
    await user.keyboard(":");
    dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("New draft"));
    expect(await screen.findByRole("dialog", { name: "New draft" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("j");
    await user.keyboard(":");
    dialog = await screen.findByRole("dialog", { name: "Command palette" });
    await user.click(within(dialog).getByText("Approve draft (Drafts only)"));
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.cmd === "draft_approve").map((c) => c.args)).toEqual([
        // The reply above is the newest draft, at the top.
        { account: "work", ids: ["fixture-draft-1"] },
      ]),
    );
  });

  it("merges keys that share an action within a section", () => {
    const palette = paletteEntries().find((e) => e.label.startsWith("Command palette"));
    expect(palette?.keys).toEqual([":", "Ctrl+p"]);
  });
});
