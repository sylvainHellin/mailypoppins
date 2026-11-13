import { act, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp, shellReady } from "@/test/render";
import { emitEnvelope, fixtures, mock, MOCK_CONFIG_PATH } from "@/test/tauri-mock";

const seeded = fixtures.bootstrap.snapshot.diagnostics[0];
const line = (name: string) => document.querySelector<HTMLElement>(`[data-health="${name}"]`);
const MAILBOX_WARNING =
  'account "home": unknown mailbox key `accounts.mailboxes.spesen` is ignored';

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
    act(() => emitEnvelope("diagnostic.check_changed", { name: seeded.name, status: "ok", detail: "serving" }));
    await waitFor(() => expect(document.querySelector("[data-health]")).toBeNull());
  });

  it("dismisses a line until its check's detail changes", async () => {
    const { user } = renderApp();
    await shellReady();
    const shown = await waitFor(() => {
      const el = line(seeded.name);
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    await user.click(within(shown).getByRole("button", { name: "Dismiss Account home" }));
    await waitFor(() => expect(line(seeded.name)).toBeNull());
    act(() => emitEnvelope("diagnostic.check_changed", { ...seeded, detail: "blocked: a second engine took the lock" }));
    await waitFor(() => expect(line(seeded.name)).toHaveTextContent("Account home: blocked: a second engine took the lock"));
  });

  // PERSO-296: the daemon holds a password its IMAP server rejected, and says
  // so in the account's check, with the sentence mp daemon health prints.
  const REJECTED = "the server rejected the IMAP password; set it with mp config set-password -A work imap";

  // Two passwords of their own, so the dialog is the refused one's (PERSO-297).
  const apart = () => {
    mock.passwords = [
      { account: "work", kind: "imap", value: "<redacted>" },
      { account: "work", kind: "smtp", value: "<redacted>" },
    ];
  };

  it("offers the IMAP password dialog on an account whose login the server rejected", async () => {
    const { user } = renderApp(undefined, () => {
      mock.bootstrap.snapshot.diagnostics = [];
      mock.loginRejected = [{ account: "work", login: "imap" }];
      apart();
    });
    await shellReady();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "account:work", status: "warn", detail: REJECTED }));
    const shown = await waitFor(() => {
      const el = line("account:work");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    expect(shown).toHaveTextContent(`Account work: ${REJECTED}`);
    await user.click(await within(shown).findByRole("button", { name: "Set IMAP password for work" }));
    const dialog = await screen.findByRole("dialog", { name: "Set IMAP password" });
    expect(within(dialog).getByRole("form", { name: "IMAP password for work" })).toBeInTheDocument();
  });

  it("offers the SMTP password dialog on an account whose SMTP login the server rejected", async () => {
    const smtp = "the server rejected the SMTP password; set it with mp config set-password -A work smtp";
    const { user } = renderApp(undefined, () => {
      mock.bootstrap.snapshot.diagnostics = [];
      mock.loginRejected = [{ account: "work", login: "smtp" }];
      apart();
    });
    await shellReady();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "account:work", status: "warn", detail: smtp }));
    const shown = await waitFor(() => {
      const el = line("account:work");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    await user.click(await within(shown).findByRole("button", { name: "Set SMTP password for work" }));
    const dialog = await screen.findByRole("dialog", { name: "Set SMTP password" });
    expect(within(dialog).getByRole("form", { name: "SMTP password for work" })).toBeInTheDocument();
  });

  it("offers the one-password dialog on an account whose IMAP and SMTP logins share a password", async () => {
    const shared = "the server rejected the IMAP password; set it with mp config set-password -A work";
    const { user } = renderApp(undefined, () => {
      mock.bootstrap.snapshot.diagnostics = [];
      mock.loginRejected = [{ account: "work", login: "imap" }];
      mock.passwords = [{ account: "work", kind: null, value: "<redacted>" }];
    });
    await shellReady();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "account:work", status: "warn", detail: shared }));
    const shown = await waitFor(() => {
      const el = line("account:work");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    await user.click(await within(shown).findByRole("button", { name: "Set password for work" }));
    const dialog = await screen.findByRole("dialog", { name: "Set password" });
    expect(within(dialog).getByRole("form", { name: "Password for work" })).toBeInTheDocument();
  });

  it("offers a new sign-in on a Graph account whose sign-in Microsoft refused", async () => {
    const graph = "the sign-in expired or was revoked; sign in again with mp config oauth2-login -A home";
    const { user } = renderApp(undefined, () => {
      mock.bootstrap.snapshot.diagnostics = [];
      mock.loginRejected = [{ account: "home", login: "graph" }];
    });
    await shellReady();
    act(() => emitEnvelope("diagnostic.check_changed", { name: "account:home", status: "warn", detail: graph }));
    const shown = await waitFor(() => {
      const el = line("account:home");
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    expect(shown).toHaveTextContent(`Account home: ${graph}`);
    expect(within(shown).queryByRole("button", { name: /^Set .* password/ })).toBeNull();
    await user.click(await within(shown).findByRole("button", { name: "Sign home in again" }));
    await waitFor(() => expect(mock.signIns.map((r) => r.account)).toEqual(["home"]));
  });

  it("offers no password on an account check about something else", async () => {
    renderApp();
    await shellReady();
    const shown = await waitFor(() => {
      const el = line(seeded.name);
      expect(el).not.toBeNull();
      return el as HTMLElement;
    });
    await waitFor(() => expect(mock.calls.some((c) => c.cmd === "account_credentials")).toBe(true));
    expect(within(shown).queryByRole("button", { name: /^Set IMAP password/ })).toBeNull();
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
