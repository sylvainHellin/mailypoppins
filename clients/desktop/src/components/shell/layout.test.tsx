import { act, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { layoutFor } from "@/app/layout";
import { renderApp, shellReady } from "@/test/render";
import { emitMenu } from "@/test/tauri-mock";

describe("the adaptive layout", () => {
  it("maps widths to layouts at 1100 and 760", () => {
    expect(layoutFor(1400)).toBe("wide");
    expect(layoutFor(1100)).toBe("wide");
    expect(layoutFor(1099)).toBe("medium");
    expect(layoutFor(760)).toBe("medium");
    expect(layoutFor(759)).toBe("narrow");
  });

  it("wide: expanded sidebar, list and reader", async () => {
    const { container } = renderApp(1400);
    await shellReady();
    expect(container.querySelector('[data-layout="wide"]')).not.toBeNull();
    expect(container.querySelector('[data-slot="sidebar"][data-state="expanded"]')).not.toBeNull();
    expect(screen.getByRole("navigation", { name: /mailboxes/ })).toBeInTheDocument();
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
    expect(screen.getByRole("separator", { name: /Resize/ })).toBeInTheDocument();
  });

  it("medium: the sidebar collapses to an icon rail, list and reader stay", async () => {
    const { container } = renderApp(900);
    await shellReady();
    expect(container.querySelector('[data-layout="medium"]')).not.toBeNull();
    expect(container.querySelector('[data-slot="sidebar"][data-collapsible="icon"]')).not.toBeNull();
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
  });

  it("narrow: one view at a time, with a back path", async () => {
    const { container, user } = renderApp(600);
    await shellReady();
    expect(container.querySelector('[data-layout="narrow"]')).not.toBeNull();
    expect(screen.queryByRole("complementary", { name: "Reader" })).toBeNull();
    expect(screen.queryByRole("navigation", { name: /mailboxes/ })).toBeNull();

    await user.keyboard("j");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("complementary", { name: "Reader" })).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();

    await user.click(screen.getByRole("button", { name: /Back to Messages/ }));
    expect(await screen.findByRole("listbox")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(await screen.findByRole("navigation", { name: /mailboxes/ })).toBeInTheDocument();
  });

  it("persists the list width and the collapsed sidebar", async () => {
    const { user } = renderApp(1400);
    await shellReady();
    const sep = screen.getByRole("separator", { name: /Resize/ });
    sep.focus();
    await user.keyboard("{ArrowRight}");
    expect(sep).toHaveAttribute("aria-valuenow", "436");
    const saved = JSON.parse(localStorage.getItem("mailypoppins.desktop.prefs.v1") ?? "{}");
    expect(saved.listWidth).toBe(436);
  });

  it("resizes the list from the palette and the View menu, not only the pointer", async () => {
    const { user } = renderApp(1400);
    await shellReady();
    const sep = screen.getByRole("separator", { name: /Resize/ });
    expect(sep).toHaveAttribute("aria-valuenow", "420");
    await user.keyboard(":");
    await user.keyboard("Widen list");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(sep).toHaveAttribute("aria-valuenow", "460"));
    act(() => emitMenu("narrow_list"));
    act(() => emitMenu("narrow_list"));
    await waitFor(() => expect(sep).toHaveAttribute("aria-valuenow", "380"));
  });
});
