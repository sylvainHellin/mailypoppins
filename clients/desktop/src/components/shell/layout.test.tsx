import { act, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { layoutFor, listWidthFor } from "@/app/layout";
import { READER_MIN } from "@/app/state";
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

  it("places the panes where index.css squares the focus ring's shared corners (PERSO-95)", async () => {
    const { container, user } = renderApp(1400);
    await shellReady();
    // The selectors index.css draws the ring's corners with.
    const squareLeft = (pane: string) => container.querySelector(`[data-panes] > :not(:first-child) [data-pane="${pane}"]`);
    const squareRight = (pane: string) => container.querySelector(`[data-panes] > :not(:last-child) [data-pane="${pane}"]`);
    expect(squareLeft("list")).toBeNull();
    expect(squareRight("list")).not.toBeNull();
    expect(squareLeft("reader")).not.toBeNull();
    expect(squareRight("reader")).toBeNull();

    // A zoomed pane is alone in the row and keeps its four rounded corners.
    await user.keyboard("z");
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "Reader" })).toBeNull());
    expect(squareLeft("list")).toBeNull();
    expect(squareRight("list")).toBeNull();

    // So does a full-pane view such as Settings.
    await user.keyboard("z");
    act(() => emitMenu("settings"));
    await screen.findByRole("region", { name: "Settings" });
    expect(container.querySelector('[data-panes] > :first-child:last-child [data-view="settings"][data-pane="list"]')).not.toBeNull();
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

  it("narrow: a view stands in the list's place, titled, with the way up to the sidebar", async () => {
    const { user } = renderApp(600);
    await shellReady();
    await user.keyboard(" a");
    expect(await screen.findByRole("region", { name: "Calendar" })).toBeInTheDocument();
    expect(screen.queryByRole("listbox", { name: /messages/ })).toBeNull();
    await user.click(screen.getByRole("button", { name: /Back to Mailboxes/ }));
    const nav = await screen.findByRole("navigation", { name: /mailboxes/ });
    expect(nav.querySelector('[data-view-entry="calendar"]')).toHaveAttribute("aria-current", "page");
  });

  it("wide and medium: a view takes the list's and the reader's place, the splitter too", async () => {
    const { user } = renderApp(900);
    await shellReady();
    await user.keyboard(" c");
    expect(await screen.findByRole("region", { name: "Contacts" })).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Reader" })).toBeNull();
    expect(screen.queryByRole("separator", { name: /Resize/ })).toBeNull();
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

  it("clamps a stored list width to what leaves the reader READER_MIN", () => {
    expect(listWidthFor(720, 0)).toEqual({ width: 720, max: 720 });
    expect(listWidthFor(720, 850)).toEqual({ width: 530, max: 530 });
    expect(listWidthFor(400, 850)).toEqual({ width: 400, max: 530 });
    expect(listWidthFor(720, 400)).toEqual({ width: 260, max: 260 });
  });

  it("medium: a persisted 720 px list leaves the reader its minimum", async () => {
    const Real = globalThis.ResizeObserver;
    // The pane row measures 850 px; other observed elements hear nothing.
    class Measured {
      constructor(private cb: ResizeObserverCallback) {}
      observe(el: Element) {
        if (!el.hasAttribute("data-panes")) return;
        this.cb([{ target: el, contentRect: { width: 850 } } as unknown as ResizeObserverEntry], this as unknown as ResizeObserver);
      }
      unobserve() {}
      disconnect() {}
    }
    globalThis.ResizeObserver = Measured as unknown as typeof ResizeObserver;
    try {
      renderApp(900, () =>
        localStorage.setItem("mailypoppins.desktop.prefs.v1", JSON.stringify({ sidebarCollapsed: false, listWidth: 720 })),
      );
      await shellReady();
      const listPane = screen.getByRole("region", { name: "Message list" }).parentElement;
      await waitFor(() => expect(listPane).toHaveStyle({ width: `${850 - READER_MIN}px` }));
      const sep = screen.getByRole("separator", { name: /Resize/ });
      expect(sep).toHaveAttribute("aria-valuenow", "530");
      expect(sep).toHaveAttribute("aria-valuemax", "530");
      // The preference is kept for a wider window.
      expect(JSON.parse(localStorage.getItem("mailypoppins.desktop.prefs.v1") ?? "{}").listWidth).toBe(720);
    } finally {
      globalThis.ResizeObserver = Real;
    }
  });

  it("medium: Widen and Narrow step from the drawn width, within what the reader leaves", async () => {
    const Real = globalThis.ResizeObserver;
    class Measured {
      constructor(private cb: ResizeObserverCallback) {}
      observe(el: Element) {
        if (!el.hasAttribute("data-panes")) return;
        this.cb([{ target: el, contentRect: { width: 850 } } as unknown as ResizeObserverEntry], this as unknown as ResizeObserver);
      }
      unobserve() {}
      disconnect() {}
    }
    globalThis.ResizeObserver = Measured as unknown as typeof ResizeObserver;
    const stored = () => JSON.parse(localStorage.getItem("mailypoppins.desktop.prefs.v1") ?? "{}").listWidth;
    try {
      const { user } = renderApp(900, () =>
        localStorage.setItem("mailypoppins.desktop.prefs.v1", JSON.stringify({ sidebarCollapsed: false, listWidth: 720 })),
      );
      await shellReady();
      const sep = screen.getByRole("separator", { name: /Resize/ });
      await waitFor(() => expect(sep).toHaveAttribute("aria-valuenow", "530"));

      act(() => emitMenu("narrow_list"));
      await waitFor(() => expect(sep).toHaveAttribute("aria-valuenow", "490"));
      expect(stored()).toBe(490);

      act(() => emitMenu("widen_list"));
      await waitFor(() => expect(sep).toHaveAttribute("aria-valuenow", "530"));
      expect(stored()).toBe(530);

      // At the measured max, neither the menu nor the splitter's keys store more.
      act(() => emitMenu("widen_list"));
      sep.focus();
      await user.keyboard("{ArrowRight}");
      expect(sep).toHaveAttribute("aria-valuenow", "530");
      expect(stored()).toBe(530);
    } finally {
      globalThis.ResizeObserver = Real;
    }
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
