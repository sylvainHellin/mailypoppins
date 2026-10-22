// The account picker `ga` opens and the sidebar's collapsed accounts (ticket
// 0147): the fixture has two accounts, `work` (selected at start) and `home`.

import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, mock } from "@/test/tauri-mock";

const picker = () => screen.findByRole("dialog", { name: "Switch account" });
const row = (name: RegExp) => screen.getByRole("button", { name });
const nav = () => screen.getByRole("navigation", { name: "Accounts and mailboxes" });
const toggle = (account: string) => nav().querySelector(`[data-account-toggle="${account}"]`) as HTMLButtonElement;
const listName = () => screen.getByRole("listbox").getAttribute("aria-label");

describe("the account picker", () => {
  it("ga opens it on the current account, and j then Enter switches to the next", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ga");
    const dialog = await picker();
    expect(within(dialog).getByText("j and k move, Enter switches, 1 to 9 jump, Escape cancels")).toBeInTheDocument();
    const work = row(/^work, current/);
    expect(work).toHaveAttribute("aria-current", "true");
    expect(work).toHaveAttribute("data-selected");
    await waitFor(() => expect(document.activeElement).toBe(work));
    expect(row(/^home, .*unread in the inbox, key 2$/)).toBeInTheDocument();
    await user.keyboard("j");
    expect(row(/^home/)).toHaveAttribute("data-selected");
    await user.keyboard("j");
    expect(row(/^home/)).toHaveAttribute("data-selected");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
    await waitFor(() => expect(toggle("home")).toHaveAttribute("aria-expanded", "true"));
    expect(toggle("work")).toHaveAttribute("aria-expanded", "false");
  });

  it("a digit switches straight to the nth account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ga");
    await picker();
    await user.keyboard("2");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
    await waitFor(() => expect(toggle("home")).toHaveAttribute("aria-expanded", "true"));
  });

  it("Escape and q close it with nothing changed", async () => {
    const { user } = renderApp();
    await shellReady();
    const before = listName();
    for (const key of ["{Escape}", "q"]) {
      await user.keyboard("ga");
      await picker();
      await user.keyboard("j");
      await user.keyboard(key);
      await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
      expect(toggle("work")).toHaveAttribute("aria-expanded", "true");
      expect(listName()).toBe(before);
    }
  });

  it("a click on an account switches to it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("ga");
    await picker();
    await user.click(row(/^home/));
    await waitFor(() => expect(toggle("home")).toHaveAttribute("aria-expanded", "true"));
  });

  it("Enter on the current account only closes it, so a search stays open", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("/");
    await user.keyboard("ledger{Enter}");
    const results = /^Search results for/;
    await screen.findByRole("listbox", { name: results });
    act(() => (document.activeElement as HTMLElement).blur());
    await user.keyboard("ga");
    await picker();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
    expect(screen.getByRole("listbox", { name: results })).toBeInTheDocument();
  });

  it("the palette's Switch account opens it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Switch account{Enter}");
    expect(await picker()).toBeInTheDocument();
  });

  it("marks an account whose last sync failed, and follows a sync.completed", async () => {
    const { user } = renderApp(1400, () => {
      const home = mock.bootstrap.snapshot.accounts.find((a) => a.name === "home");
      if (home) home.sync_health = { state: "failed" };
    });
    await shellReady();
    await user.keyboard("ga");
    const dialog = await picker();
    const home = row(/^home, /);
    expect(home).toHaveAccessibleName(/^home, last sync failed, /);
    expect(home.querySelector('[data-slot="sync-failed"]')).toHaveClass("text-warning");
    expect(row(/^work, /)).not.toHaveAccessibleName(/last sync failed/);
    expect(within(dialog).getAllByTitle("Last sync failed")).toHaveLength(1);
    act(() => emitEnvelope("sync.completed", { account: "home", error: null, new_inbox_mail: [] }));
    await waitFor(() => expect(row(/^home, /)).not.toHaveAccessibleName(/last sync failed/));
  });

  it("with one account ga and the palette open nothing", async () => {
    const { user } = renderApp(1400, () => {
      mock.accounts = mock.accounts.filter((a) => a.name === "work");
      mock.bootstrap.snapshot.accounts = mock.bootstrap.snapshot.accounts.filter((a) => a.name === "work");
      delete mock.bootstrap.snapshot.mailboxes.home;
    });
    await shellReady();
    await user.keyboard("ga");
    await user.keyboard(":");
    await screen.findByRole("dialog", { name: "Command palette" });
    await user.keyboard("Switch account{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull());
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull();
  });
});

describe("the sidebar's collapsed accounts", () => {
  it("starts with only the selected account expanded, and a header click toggles one without selecting it", async () => {
    const { user } = renderApp();
    await shellReady();
    expect(toggle("work")).toHaveAttribute("aria-expanded", "true");
    expect(toggle("home")).toHaveAttribute("aria-expanded", "false");
    expect(nav().querySelector('[data-mailbox^="work/"]')).not.toBeNull();
    expect(nav().querySelector('[data-mailbox^="home/"]')).toBeNull();
    // The header keeps the sidebar's one tab stop on a mailbox.
    expect(toggle("home")).toHaveAttribute("tabindex", "-1");
    const before = listName();
    await user.click(toggle("home"));
    expect(toggle("home")).toHaveAttribute("aria-expanded", "true");
    expect(nav().querySelector('[data-mailbox^="home/"]')).not.toBeNull();
    expect(listName()).toBe(before);
    act(() => void fireEvent.click(toggle("home")));
    expect(nav().querySelector('[data-mailbox^="home/"]')).toBeNull();
  });

  it("a collapsed account's header shows its inbox unread count, an expanded one's does not", async () => {
    const { user } = renderApp();
    await shellReady();
    await waitFor(() => expect(toggle("home").querySelector('[data-slot="account-unread"]')).not.toBeNull());
    const count = toggle("home").querySelector('[data-slot="account-unread"]')?.textContent ?? "";
    expect(count).toMatch(/^, [1-9]\d* unread in the inbox$/);
    expect(toggle("home")).toHaveAccessibleName(/unread in the inbox$/);
    expect(toggle("work").querySelector('[data-slot="account-unread"]')).toBeNull();
    await user.click(toggle("home"));
    expect(toggle("home").querySelector('[data-slot="account-unread"]')).toBeNull();
  });

  it("picking the current account in ga expands it again after a hand collapse", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(toggle("work"));
    expect(toggle("work")).toHaveAttribute("aria-expanded", "false");
    act(() => (document.activeElement as HTMLElement).blur());
    await user.keyboard("ga");
    await picker();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
    expect(toggle("work")).toHaveAttribute("aria-expanded", "true");
    expect(nav().querySelector('[data-mailbox^="work/"]')).not.toBeNull();
  });

  it("an account expanded by hand stays expanded after a switch away from it", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.click(toggle("home"));
    await user.keyboard("ga");
    await picker();
    await user.keyboard("2");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
    await user.keyboard("{Escape}");
    await user.keyboard("ga");
    await picker();
    await user.keyboard("1");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Switch account" })).toBeNull());
    expect(toggle("work")).toHaveAttribute("aria-expanded", "true");
    expect(toggle("home")).toHaveAttribute("aria-expanded", "true");
  });
});
