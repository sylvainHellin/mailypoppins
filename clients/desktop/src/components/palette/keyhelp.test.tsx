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
    // A substring match, as the TUI's: no loose fuzzy hit such as "Navigate results".
    expect(labels).toEqual(["Archive", "Archive"]);
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

  it("keeps the sections in their order, DESKTOP last, after a filter is cleared", async () => {
    const { user, help, input } = await openHelp();
    const headings = () => [...help.querySelectorAll("[cmdk-group-heading]")].map((h) => h.textContent);
    const original = [...SECTIONS.map((s) => s.title), "DESKTOP"];
    expect(headings()).toEqual(original);
    await user.keyboard("archive");
    expect(headings()).toEqual(["MESSAGE (list, headers, body)", "SERVER SEARCH"]);
    await user.clear(input);
    expect(headings()).toEqual(original);
    const firstRows = SECTIONS[0].bindings.map((b) => b.action);
    const group = within(help).getByRole("group", { name: SECTIONS[0].title });
    expect(within(group).getAllByTestId("help-item").map((r) => r.getAttribute("data-label"))).toEqual(firstRows);
  });

  it("matches a section title, and opens again with an empty filter", async () => {
    const { user, help, rows } = await openHelp();
    await user.keyboard("desktop");
    expect([...help.querySelectorAll("[cmdk-group-heading]")].map((h) => h.textContent)).toEqual(["DESKTOP"]);
    expect(rows()).toHaveLength(GUI_ENTRIES.filter((e) => e.keys.length > 0).length);
    await user.keyboard("{Escape}");
    await user.keyboard("?");
    const again = await screen.findByRole("dialog", { name: "Keys" });
    expect(within(again).getByRole("combobox", { name: "Filter keys" })).toHaveValue("");
    expect(within(again).getAllByTestId("help-item")).toHaveLength(TOTAL);
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
