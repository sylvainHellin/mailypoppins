import { screen, within } from "@testing-library/react";
import { windowFor, WINDOW_FROM } from "@/components/list/useWindow";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";

describe("accessibility primitives", () => {
  it("has the nav, main and complementary landmarks and a labelled listbox", async () => {
    renderApp();
    await shellReady();
    expect(screen.getByRole("navigation", { name: "Accounts and mailboxes" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
    const list = screen.getByRole("listbox", { name: "Inbox messages" });
    const options = screen.getAllByRole("option");
    expect(options.length).toBe(8);
    for (const o of options) expect(o).toHaveAttribute("aria-selected");
    expect(list).toBeInTheDocument();
  });

  it("gives every row its position and the set size, messages and drafts", async () => {
    const { user } = renderApp();
    await shellReady();
    const options = screen.getAllByRole("option");
    expect(options.map((o) => o.getAttribute("aria-posinset"))).toEqual(options.map((_, i) => String(i + 1)));
    for (const o of options) expect(o).toHaveAttribute("aria-setsize", String(options.length));

    await user.keyboard("2");
    const drafts = await screen.findByRole("listbox", { name: "Drafts messages" });
    const rows = within(drafts).getAllByRole("option");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.map((o) => o.getAttribute("aria-posinset"))).toEqual(rows.map((_, i) => String(i + 1)));
    for (const o of rows) expect(o).toHaveAttribute("aria-setsize", String(rows.length));
  });

  it("mounts every row: windowing is off in M1", () => {
    expect(WINDOW_FROM).toBe(Number.POSITIVE_INFINITY);
    expect(windowFor(5000, 40_000, 800)).toEqual({ start: 0, end: 5000, padTop: 0, padBottom: 0 });
  });

  it("keeps one tab stop per list (roving tabindex)", async () => {
    const { user } = renderApp();
    await shellReady();
    const stops = () => screen.getAllByRole("option").filter((o) => o.getAttribute("tabindex") === "0");
    expect(stops()).toHaveLength(1);
    await user.keyboard("jj");
    expect(stops()).toHaveLength(1);
    expect(stops()[0]).toHaveAttribute("aria-selected", "true");
    const nav = screen.getByRole("navigation", { name: "Accounts and mailboxes" });
    expect(nav.querySelectorAll('[data-mailbox][tabindex="0"]')).toHaveLength(1);
  });

  it("Tab from the page continues the pane cycle from the focused pane", async () => {
    const { user } = renderApp();
    await shellReady();
    (document.activeElement as HTMLElement | null)?.blur();
    await user.tab();
    // The first Tab from nowhere lands in the pane cycle: sidebar, list, reader.
    const order: (string | null | undefined)[] = [];
    for (let i = 0; i < 3; i++) {
      order.push(document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane"));
      await user.tab();
    }
    expect(order).toEqual(["reader", "sidebar", "list"]);
  });

  it("marks the selected mailbox as the current page", async () => {
    renderApp();
    await shellReady();
    expect(screen.getByRole("button", { name: /^Inbox, 4 unread of 8/ })).toHaveAttribute("aria-current", "page");
  });
});
