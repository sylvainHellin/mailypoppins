// The which-key popup for an armed prefix (ticket 0137, U3): its rows follow
// the keymap's own resolution, and it leaves with the next key, the timeout
// and the window's blur.

import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { getPendingPrefix } from "@/keymap/pendingPrefix";
import { isEditable, PREFIX_TIMEOUT_MS } from "@/keymap/useKeymap";

const popup = () => document.querySelector('[data-slot="prefix-popup"]') as HTMLElement;
const title = () => popup().querySelector('[data-testid="prefix-title"]')?.textContent ?? null;
const rows = () =>
  [...popup().querySelectorAll<HTMLElement>('[data-testid="prefix-row"]')].map((r) => [
    r.dataset.key,
    r.querySelector("span")?.textContent,
  ]);
const pane = () => document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane");
/** A key as the keymap sees it, without user-event's own timers. */
const press = (key: string) => act(() => void fireEvent.keyDown(document.activeElement ?? document.body, { key }));

const COMPOSE = [
  ["cn", "New draft"],
  ["cs", "Manage signatures"],
  ["cr", "Reply"],
  ["ca", "Reply all"],
  ["cf", "Forward"],
];

afterEach(() => {
  vi.useRealTimers();
});

describe("the prefix popup", () => {
  it("c in Mail lists the compose continuations in the catalog's order, titled compose", async () => {
    const { user } = renderApp();
    await shellReady();
    expect(rows()).toEqual([]);
    await user.keyboard("j");
    await user.keyboard("c");
    expect(title()).toBe("compose");
    expect(rows()).toEqual(COMPOSE);
    expect(popup()).toHaveAttribute("role", "status");
    expect(popup()).toHaveAttribute("aria-live", "polite");
  });

  it("the Drafts-only keys show in Drafts only, and the row keys not from the sidebar", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("2");
    await screen.findByRole("listbox", { name: "Drafts messages" });
    await user.keyboard("c");
    expect(rows()).toEqual([
      ...COMPOSE,
      ["ce", "Edit recipients (Drafts only)"],
      ["cA", "Approve draft (Drafts only)"],
      ["cD", "Unapprove, back to draft (Drafts only)"],
      ["cX", "Send all approved drafts (Drafts only)"],
    ]);
    await user.keyboard("{Escape}");
    await user.keyboard("gm");
    expect(pane()).toBe("sidebar");
    await user.keyboard("c");
    expect(rows()).toEqual(COMPOSE.slice(0, 2));
  });

  it("g in a full-pane view lists only that view's g combos", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("g");
    expect(title()).toBe("go");
    expect(rows().map(([k]) => k)).toEqual(["gm", "ga", "gj / gk", "gg / G", "gr", "go"]);
    await user.keyboard("{Escape}");
    await user.keyboard(" c");
    await screen.findByRole("region", { name: "Contacts" });
    await user.keyboard("g");
    expect(rows()).toEqual([["gg / G", "Jump to top / bottom"]]);
  });

  it("Space lists the view family", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(" ");
    expect(title()).toBe("view");
    expect(rows()).toEqual([
      ["Space m", "Switch to Mail view"],
      ["Space c", "Switch to Contacts view"],
      ["Space a", "Switch to Calendar view"],
    ]);
  });

  it("the next key hides it, whether it resolves or not", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("g");
    expect(rows()).not.toEqual([]);
    // `gz` binds nothing: the popup leaves and `z` does not zoom the list.
    await user.keyboard("z");
    expect(rows()).toEqual([]);
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
    await user.keyboard("g");
    expect(rows()).not.toEqual([]);
    await user.keyboard("m");
    expect(pane()).toBe("sidebar");
    expect(rows()).toEqual([]);
  });

  it("the timeout hides it, and a continuation after the timeout does not resolve", async () => {
    renderApp();
    await shellReady();
    vi.useFakeTimers();
    press("g");
    expect(title()).toBe("go");
    act(() => vi.advanceTimersByTime(PREFIX_TIMEOUT_MS - 1));
    expect(rows()).not.toEqual([]);
    act(() => vi.advanceTimersByTime(1));
    expect(rows()).toEqual([]);
    press("m");
    expect(pane()).not.toBe("sidebar");
    // Within the timeout the same keys resolve.
    press("g");
    press("m");
    expect(pane()).toBe("sidebar");
  });

  it("leaving the window hides it, and the continuation then does nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("g");
    expect(rows()).not.toEqual([]);
    act(() => void fireEvent.blur(window));
    expect(rows()).toEqual([]);
    await user.keyboard("m");
    expect(pane()).not.toBe("sidebar");
  });

  it("no popup inside an editable field, and focusing one drops the prefix", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("/");
    expect(document.activeElement?.tagName).toBe("INPUT");
    await user.keyboard("c");
    expect(rows()).toEqual([]);
    await user.keyboard("{Escape}");
    await user.keyboard("c");
    expect(rows()).toEqual(COMPOSE);
    act(() => (document.getElementById("mp-list-filter") as HTMLInputElement).focus());
    expect(rows()).toEqual([]);
  });

  it("nothing renders while the embedded editor's terminal has the focus", async () => {
    const { user } = renderApp();
    await shellReady();
    const term = document.createElement("div");
    term.setAttribute("data-slot", "terminal");
    const area = document.createElement("textarea");
    term.appendChild(area);
    document.body.appendChild(term);
    try {
      act(() => area.focus());
      await user.keyboard("c");
      expect(rows()).toEqual([]);
      act(() => area.blur());
      await user.keyboard("g");
      expect(rows()).not.toEqual([]);
      act(() => area.focus());
      expect(rows()).toEqual([]);
    } finally {
      term.remove();
    }
  });

  it("a dialog opened over an armed prefix hides it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("g");
    expect(rows()).not.toEqual([]);
    // A click opens the dialog, so no key resolves or drops the prefix.
    act(() => void fireEvent.click(screen.getByRole("button", { name: "Activity log, key s l" })));
    await screen.findByRole("dialog", { name: "Activity log" });
    expect(getPendingPrefix()?.key).toBe("g");
    expect(isEditable(document.activeElement)).toBe(false);
    expect(rows()).toEqual([]);
  });
});
