import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitMenu } from "@/test/tauri-mock";

function selectedSubject(): string | null {
  const row = document.querySelector('[role="option"][aria-selected="true"]');
  return row?.getAttribute("aria-label") ?? null;
}

describe("keyboard routing", () => {
  it("j and k move the selection in the list and the reader follows", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    expect(selectedSubject()).toMatch(/Quarterly ledger review/);
    await user.keyboard("j");
    expect(selectedSubject()).toMatch(/Angebot Dachsanierung/);
    await user.keyboard("k");
    expect(selectedSubject()).toMatch(/Quarterly ledger review/);
    const reader = screen.getByRole("complementary", { name: "Reader" });
    expect(await within(reader).findByRole("heading", { name: "Quarterly ledger review" })).toBeInTheDocument();
    expect(within(reader).getByText(/the quarterly ledger is attached/)).toBeInTheDocument();
  });

  it("Tab and Shift+Tab cycle focus through sidebar, list and reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    const pane = () => document.activeElement?.closest("[data-pane]")?.getAttribute("data-pane");
    expect(pane()).toBe("list");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("reader");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("sidebar");
    await user.keyboard("{Tab}");
    expect(pane()).toBe("list");
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(pane()).toBe("sidebar");
  });

  it("the sidebar cursor moves with j and Enter opens the mailbox", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("gm");
    await user.keyboard("jj");
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "Sent" })).toBeInTheDocument();
    expect(await screen.findByRole("listbox", { name: "Sent messages" })).toBeInTheDocument();
  });

  it("digits jump to a mailbox of the selected account", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("4");
    expect(await screen.findByRole("listbox", { name: "Archive messages" })).toBeInTheDocument();
  });

  it(": and Ctrl+p open the palette, ? the key help", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard(":");
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("{Control>}p{/Control}");
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.keyboard("?");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("ignores keys while a text field has focus, except Escape", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("/");
    const input = screen.getByRole("searchbox", { name: /Filter this list/ });
    expect(input).toHaveFocus();
    await user.keyboard("j:");
    expect(selectedSubject()).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(input).toHaveValue("j:");
    await user.keyboard("{Escape}");
    expect(input).not.toHaveFocus();
  });

  it("z zooms the focused list pane, hiding the reader", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("j");
    await user.keyboard("z");
    expect(screen.queryByRole("complementary", { name: "Reader" })).toBeNull();
    await user.keyboard("z");
    expect(screen.getByRole("complementary", { name: "Reader" })).toBeInTheDocument();
  });

  it("a native menu item runs the same action as its key", async () => {
    renderApp();
    await shellReady();
    emitMenu("key_help");
    expect(await screen.findByRole("dialog", { name: "Keys" })).toBeInTheDocument();
  });

  it("a key for a later milestone says so instead of doing nothing", async () => {
    const { user } = renderApp();
    await shellReady();
    await user.keyboard("a");
    expect(await screen.findByText(/Archive arrives with mutations \(M2\)/)).toBeInTheDocument();
  });
});
