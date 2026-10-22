import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, fixtures, mock, MOCK_CONFIG_PATH } from "@/test/tauri-mock";

const seeded = fixtures.bootstrap.snapshot.diagnostics[0];
const line = (name: string) => document.querySelector<HTMLElement>(`[data-health="${name}"]`);
const MAILBOX_WARNING =
  '/fixture/config.toml loaded, 2 account(s); account "home": unknown mailbox key `accounts.mailboxes.spesen` is ignored';

describe("the health banner", () => {
  it("shows the snapshot's checks from the start, in the polite status region", async () => {
    renderApp();
    await shellReady();
    const shown = line(seeded.name);
    expect(shown).toHaveTextContent(`Account home: ${seeded.detail}`);
    expect(shown).toHaveAttribute("data-status", "warn");
    expect(shown?.closest('[role="status"]')).toHaveAttribute("aria-live", "polite");
  });

  it("follows diagnostic.check_changed: a config warning with its file, and each line clears on ok", async () => {
    const { user } = renderApp();
    await shellReady();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "config_loaded", status: "warn", detail: MAILBOX_WARNING }));
    const config = await waitFor(() => {
      const el = line("config_loaded");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    expect(config).toHaveTextContent(`config.toml: ${MAILBOX_WARNING}`);
    await user.click(within(config).getByRole("button", { name: "Open config.toml" }));
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH]));

    act(() => emitEnvelope("diagnostic.check_changed", { name: "config_loaded", status: "ok", detail: "/fixture/config.toml loaded, 2 account(s)" }));
    act(() => emitEnvelope("diagnostic.check_changed", { name: seeded.name, status: "ok", detail: "home is serving" }));
    await waitFor(() => expect(document.querySelector("[data-health]")).toBeNull());
  });

  it("shows a failing check in the destructive colour, and gives config_loaded to the config.toml banner when it shows", async () => {
    renderApp(undefined, () => void (mock.bootstrap.snapshot.diagnostics = []));
    await shellReady();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "config_loaded", status: "fail", detail: "expected a table, at line 3 of /c/config.toml" }));
    act(() => emitEnvelope("config.invalid", { path: "/c/config.toml", line: 3, message: "expected a table" }));
    expect(await screen.findByRole("alert", { name: "Configuration problem" })).toBeInTheDocument();
    expect(line("config_loaded")).toBeNull();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "store_open", status: "fail", detail: "0 of 2 account store(s) are open" }));
    await waitFor(() => expect(line("store_open")?.querySelector("svg")).toHaveClass("text-destructive"));
  });
});
