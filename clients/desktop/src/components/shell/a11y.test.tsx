import { screen } from "@testing-library/react";
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

  it("tabs from the page into the sidebar, then the list, then the reader", async () => {
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
