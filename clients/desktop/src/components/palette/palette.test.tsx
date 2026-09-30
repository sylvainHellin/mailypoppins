import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { paletteEntries, SECTIONS } from "@/keymap/catalog";

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

  it("marks what M1 cannot run as disabled with its milestone", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    const archive = within(dialog).getAllByText("Archive")[0].closest("[data-testid='palette-item']");
    expect(archive).toHaveAttribute("data-disabled", "true");
    expect(archive).toHaveTextContent("M2");
    const reply = within(dialog).getAllByText("Reply")[0].closest("[data-testid='palette-item']");
    expect(reply).toHaveTextContent("M3");
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
