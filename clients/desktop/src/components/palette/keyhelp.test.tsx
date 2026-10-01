import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { GUI_ENTRIES, SECTIONS } from "@/keymap/catalog";

const TOTAL = SECTIONS.reduce((n, s) => n + s.bindings.length, 0) + GUI_ENTRIES.filter((e) => e.keys.length > 0).length;

async function openHelp() {
  const { user } = renderApp();
  await shellReady();
  await user.keyboard("?");
  const help = await screen.findByRole("dialog", { name: "Keys" });
  const input = within(help).getByRole("combobox", { name: "Filter keys" });
  const rows = () => within(help).queryAllByTestId("help-item");
  return { user, help, input, rows };
}

describe("the key help filter", () => {
  it("opens with the filter focused and every row listed", async () => {
    const { input, rows } = await openHelp();
    expect(input).toHaveFocus();
    expect(rows()).toHaveLength(TOTAL);
  });

  it("narrows the rows as the user types, and hides a section left empty", async () => {
    const { user, help, rows } = await openHelp();
    await user.keyboard("archive");
    const labels = rows().map((r) => r.getAttribute("data-label"));
    expect(labels).toContain("Archive");
    expect(labels).not.toContain("Next message");
    expect(labels.length).toBeLessThan(TOTAL / 4);
    // A section with no match disappears, heading and all.
    expect(within(help).queryByRole("group", { name: "DESKTOP" })).toBeNull();
    expect(within(help).getByRole("group", { name: /MESSAGE/ })).toBeInTheDocument();
    // The typed keys stayed in the field: none ran a binding behind the dialog.
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });

  it("says so when nothing matches", async () => {
    const { user, help, rows } = await openHelp();
    await user.keyboard("zzqqxx");
    expect(rows()).toHaveLength(0);
    expect(within(help).getByText("No matching key")).toBeInTheDocument();
  });

  it("lists every row again once the filter is cleared", async () => {
    const { user, input, rows } = await openHelp();
    await user.keyboard("archive");
    expect(rows().length).toBeLessThan(TOTAL);
    await user.clear(input);
    expect(rows()).toHaveLength(TOTAL);
    expect(screen.getByRole("group", { name: "DESKTOP" })).toBeInTheDocument();
  });

  it("Enter on a row runs nothing and Escape closes the help", async () => {
    const { user, input } = await openHelp();
    await user.keyboard("archive");
    await user.keyboard("{Enter}");
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    expect(input).toHaveValue("archive");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Keys" })).toBeNull();
  });
});
