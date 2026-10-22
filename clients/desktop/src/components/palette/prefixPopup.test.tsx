// The which-key popup for an armed prefix (ticket 0137, U3): its rows follow
// the keymap's own resolution, and it leaves with the next key, a click and
// the window's blur, never on a timer (ticket 0148).

import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { mock } from "@/test/tauri-mock";
import { getPendingPrefix } from "@/keymap/pendingPrefix";
import { isEditable } from "@/keymap/useKeymap";

const popup = () => document.querySelector('[data-slot="prefix-popup"]') as HTMLElement;
const title = () => popup().querySelector('[data-testid="prefix-title"]')?.textContent ?? null;
const rows = () =>
  [...popup().querySelectorAll<HTMLElement>('[data-testid="prefix-row"]')].map((r) => [
    r.dataset.key,
    r.querySelector("span")?.textContent,
  ]);
const pane = () => document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane");
const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
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
      ["ce", "Edit recipients (in Drafts or a conversation)"],
      ["cA", "Approve draft (in Drafts or a conversation)"],
      ["cD", "Unapprove, back to draft (in Drafts or a conversation)"],
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

  it("has no timeout: after a minute the popup is up and the continuation resolves", async () => {
    renderApp();
    await shellReady();
    press("j");
    vi.useFakeTimers();
    press("c");
    expect(title()).toBe("compose");
    act(() => vi.advanceTimersByTime(60_000));
    expect(rows()).toEqual(COMPOSE);
    vi.useRealTimers();
    press("a");
    expect(rows()).toEqual([]);
    // `ca` replies to all, and nothing is archived.
    await waitFor(() => expect(callsOf("draft_reply")).toEqual([{ account: "work", row_id: 1001, all: true, headers: null }]));
    expect(callsOf("message_archive")).toEqual([]);
  });

  it("Escape cancels the prefix and does nothing else", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const selected = () => screen.getByRole("listbox", { name: /messages/ }).querySelector('[aria-selected="true"]');
    const before = selected()?.textContent;
    expect(before).toBeTruthy();
    await user.keyboard("c");
    expect(rows()).toEqual(COMPOSE);
    await user.keyboard("{Escape}");
    expect(rows()).toEqual([]);
    expect(getPendingPrefix()).toBeNull();
    // The selection stays, no dialog opens, and the reader still shows it.
    expect(selected()?.textContent).toBe(before);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
    // The next `a` is the flat key again: the prefix is gone.
    await user.keyboard("g");
    await user.keyboard("{Escape}");
    await user.keyboard("m");
    expect(pane()).not.toBe("sidebar");
  });

  it("a click drops the prefix, and the next key is a flat key", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("g");
    expect(rows()).not.toEqual([]);
    act(() => void fireEvent.pointerDown(document.body));
    expect(rows()).toEqual([]);
    expect(getPendingPrefix()).toBeNull();
    await user.keyboard("m");
    expect(pane()).not.toBe("sidebar");
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
    // A bare click event, with no pointerdown, opens the dialog as one that
    // opens on its own would: no key and no pointer drops the prefix.
    act(() => void fireEvent.click(screen.getByRole("button", { name: "Activity log, key s l" })));
    await screen.findByRole("dialog", { name: "Activity log" });
    expect(getPendingPrefix()?.key).toBe("g");
    expect(isEditable(document.activeElement)).toBe(false);
    expect(rows()).toEqual([]);
  });
});
