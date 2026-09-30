import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppShell } from "@/components/shell/AppShell";
import { StoreProvider, useAppState } from "@/app/store";
import type { AppState } from "@/app/state";
import { cancelledLine, storedTokenLine } from "@/app/signin";
import { nameTaken } from "@/app/wizard";
import { shellReady } from "@/test/render";
import { setWidth } from "@/test/setup";
import {
  emit,
  fixtures,
  mock,
  MOCK_CONFIG_PATH,
  OAUTH_DENIED,
  reportDeviceCode,
  resetMock,
  settleSignIn,
  simulateConfigAbsent,
} from "@/test/tauri-mock";

const SECRET = "hunter2-correct-horse";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** The app with a probe on the model; `before` shapes the mock first. */
function start(before?: () => void) {
  resetMock();
  before?.();
  setWidth(1400);
  const probe = {} as { state: AppState };
  function Probe() {
    probe.state = useAppState();
    return null;
  }
  const user = userEvent.setup();
  render(
    <StoreProvider>
      <TooltipProvider>
        <AppShell />
        <Probe />
      </TooltipProvider>
    </StoreProvider>,
  );
  // user-event installs its own clipboard; the dialog's Copy goes to this one.
  const writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  return { user, probe, writeText };
}

async function openWizard() {
  const r = start();
  await shellReady();
  await r.user.click(screen.getByRole("button", { name: /^Settings$/ }));
  const view = await screen.findByRole("region", { name: "Settings" });
  await within(view).findByText(MOCK_CONFIG_PATH);
  await r.user.click(within(view).getByRole("button", { name: "Add account" }));
  const dialog = await screen.findByRole("dialog", { name: "Add account" });
  return { ...r, view, dialog };
}

type User = ReturnType<typeof userEvent.setup>;

async function next(user: User, scope: HTMLElement) {
  await user.click(within(scope).getByRole("button", { name: "Next" }));
}

async function fill(user: User, scope: HTMLElement, label: string, value: string) {
  const field = within(scope).getByLabelText(label);
  await user.clear(field);
  if (value) await user.type(field, value);
}

describe("the account wizard", () => {
  it("opens on the provider, focused, with every field labelled", async () => {
    const { user, dialog } = await openWizard();
    const imap = within(dialog).getByRole("radio", { name: /^IMAP and SMTP/ });
    await waitFor(() => expect(imap).toHaveFocus());
    expect(imap).toBeChecked();
    expect(within(dialog).getAllByRole("radio")).toHaveLength(4);
    expect(within(dialog).getByRole("radio", { name: /Proton Mail/ })).toHaveAccessibleDescription(/bridge password/);
    expect(within(dialog).getByRole("list", { name: "Steps" }).querySelector("[aria-current='step']")).toHaveTextContent("1. Provider");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Account name")).toHaveAccessibleDescription(/unique slug/);
    await waitFor(() => expect(within(dialog).getByLabelText("Account name")).toHaveFocus());
    expect(within(dialog).getByLabelText("From address")).toBeInTheDocument();
  });

  it("refuses to move on without a name, with a taken one, and without the SMTP host", async () => {
    const { user, dialog } = await openWizard();
    await next(user, dialog);
    await fill(user, dialog, "Account name", "");
    await next(user, dialog);
    const name = within(dialog).getByLabelText("Account name");
    expect(name).toHaveAttribute("aria-invalid", "true");
    expect(name).toHaveAccessibleDescription(/Give the account a name/);
    await fill(user, dialog, "Account name", "work");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Account name")).toHaveAccessibleDescription(new RegExp(nameTaken("work")));
    await fill(user, dialog, "Account name", "fastmail");
    await next(user, dialog);
    expect(dialog).toHaveAttribute("data-slot", "account-wizard-dialog");
    expect(within(dialog).getByRole("form", { name: "Account wizard" })).toHaveAttribute("data-step", "servers");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("SMTP host")).toHaveAccessibleDescription(/The SMTP host is required/);
    expect(within(dialog).getByLabelText("SMTP username")).toHaveAttribute("aria-invalid", "true");
    expect(callsOf("config_add_account")).toEqual([]);
  });

  it("adds a password account, then stores its SMTP password with no password in the store", async () => {
    const { user, probe, dialog } = await openWizard();
    await next(user, dialog);
    await fill(user, dialog, "Account name", "fastmail");
    await next(user, dialog);
    await fill(user, dialog, "SMTP host", "smtp.fastmail.com");
    await fill(user, dialog, "SMTP username", "me@fastmail.com");
    await fill(user, dialog, "IMAP host", "imap.fastmail.com");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Inbox")).toHaveValue("INBOX");
    expect(within(dialog).getByLabelText("Sent")).toHaveValue("Sent");
    await next(user, dialog);
    expect(within(dialog).getByText("smtp.fastmail.com:465 as me@fastmail.com")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() =>
      expect(callsOf("config_add_account")).toEqual([
        {
          account: {
            name: "fastmail",
            default_from: "me@fastmail.com",
            smtp: { host: "smtp.fastmail.com", port: 465, username: "me@fastmail.com" },
            imap: { host: "imap.fastmail.com", port: 993 },
            mailboxes: { inbox: "INBOX", archive: "Archive", sent: "Sent", extra: [] },
          },
        },
      ]),
    );
    expect(callsOf("config_init")).toEqual([]);
    // The password step: the password dialog, for the new account's SMTP password.
    const password = await screen.findByRole("dialog", { name: "Set SMTP password" });
    expect(password).toHaveAccessibleDescription(/For fastmail/);
    expect(probe.state.notice).toMatch(/^Added the account fastmail; store its SMTP password next/);
    // The daemon's config.changed brought the account to the sidebar.
    await waitFor(() => expect(document.getElementById("account-fastmail")).toHaveTextContent("fastmail"));
    await user.type(within(password).getByLabelText("Password"), SECRET);
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Set SMTP password" })).toBeNull());
    expect(mock.passwords).toEqual([{ account: "fastmail", kind: "smtp", value: "<redacted>" }]);
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);
    expect(document.body.innerHTML).not.toContain(SECRET);
  });

  it("shows the daemon's refusal and stays open", async () => {
    const { user, dialog } = await openWizard();
    mock.failing.set("config_add_account", { kind: "protocol", code: -32007, message: "accounts[2].smtp.host: invalid" });
    await next(user, dialog);
    await fill(user, dialog, "Account name", "x");
    await next(user, dialog);
    await fill(user, dialog, "SMTP host", "h");
    await fill(user, dialog, "SMTP username", "u");
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("accounts[2].smtp.host: invalid"));
    expect(screen.queryByRole("dialog", { name: /password/ })).toBeNull();
  });

  it("a Microsoft 365 account signs in next: the code from the progress, Copy, the verification page, stored", async () => {
    const { user, probe, dialog, writeText } = await openWizard();
    await user.click(within(dialog).getByRole("radio", { name: /OAuth2, IMAP and SMTP/ }));
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Account name")).toHaveValue("exchange");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("SMTP host")).toHaveValue("smtp.office365.com");
    expect(within(dialog).getByLabelText("SMTP port")).toHaveValue("587");
    await fill(user, dialog, "SMTP username", "me@contoso.com");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Client ID")).toHaveAccessibleDescription(/client ID is required/);
    expect(within(dialog).getByLabelText("Tenant ID")).toHaveAccessibleDescription(/tenant ID is required/);
    await fill(user, dialog, "Client ID", "client-1");
    await fill(user, dialog, "Tenant ID", "contoso.com");
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    const signIn = await screen.findByRole("dialog", { name: "Sign in exchange" });
    await waitFor(() => expect(callsOf("config_oauth2_login")).toEqual([{ account: "exchange" }]));
    expect((callsOf("config_add_account")[0]?.account as { auth_method: string }).auth_method).toBe("oauth2");

    await waitFor(() => expect(probe.state.signIn?.operation_id).toBe("fixture-oauth-1"));
    act(() => {
      reportDeviceCode();
    });
    expect(await within(signIn).findByText("FXTR-CODE")).toBeInTheDocument();
    expect(within(signIn).getByText("https://microsoft.com/devicelogin")).toBeInTheDocument();
    const copy = within(signIn).getByRole("button", { name: "Copy code" });
    await waitFor(() => expect(copy).toHaveFocus());
    await user.click(copy);
    expect(writeText).toHaveBeenCalledWith("FXTR-CODE");
    await user.click(within(signIn).getByRole("button", { name: "Open verification page" }));
    await waitFor(() => expect(callsOf("open_external")).toEqual([{ url: "https://microsoft.com/devicelogin" }]));

    act(() => {
      settleSignIn();
    });
    expect(await within(signIn).findByText(storedTokenLine("exchange"))).toBeInTheDocument();
    expect(probe.state.activityLog.map((e) => e.text)).toContain(storedTokenLine("exchange"));
    await user.click(within(signIn).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Sign in exchange" })).toBeNull());
    expect(probe.state.signIn).toBeNull();
  });
});

describe("the device-code dialog", () => {
  async function signInHome() {
    const r = start();
    await shellReady();
    await r.user.click(screen.getByRole("button", { name: /^Settings$/ }));
    const view = await screen.findByRole("region", { name: "Settings" });
    await within(view).findByText(MOCK_CONFIG_PATH);
    await r.user.click(within(within(view).getByRole("article", { name: "home" })).getByRole("button", { name: "Sign in" }));
    const dialog = await screen.findByRole("dialog", { name: "Sign in home" });
    await waitFor(() => expect(r.probe.state.signIn?.operation_id).toBe("fixture-oauth-1"));
    return { ...r, dialog };
  }

  it("shows the code a re-bootstrap's operation status carries when the progress event was missed", async () => {
    const { dialog } = await signInHome();
    expect(within(dialog).queryByText("FXTR-CODE")).toBeNull();
    const bootstrap = structuredClone(fixtures.bootstrap);
    bootstrap.snapshot.operations = [
      {
        operation_id: "fixture-oauth-1",
        method: "config.oauth2_login",
        state: "running",
        scope: "durable",
        progress: { phase: "device_code", done: 0, total: null, message: "https://microsoft.com/devicelogin FXTR-CODE" },
        result: null,
        error: null,
      },
    ];
    act(() => emit({ type: "rebootstrapped", cause: "resync", bootstrap }));
    expect(await within(dialog).findByText("FXTR-CODE")).toBeInTheDocument();
  });

  it("Cancel cancels the operation and says the sign-in may still complete", async () => {
    const { user, probe, dialog } = await signInHome();
    act(() => {
      reportDeviceCode();
    });
    await within(dialog).findByText("FXTR-CODE");
    await user.click(within(dialog).getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(callsOf("config_oauth2_cancel")).toEqual([{ operation_id: "fixture-oauth-1" }]));
    expect(await within(dialog).findByText(cancelledLine("home"))).toBeInTheDocument();
    expect(within(dialog).getByText(/may still complete/)).toBeInTheDocument();
    expect(probe.state.signIn?.outcome?.kind).toBe("cancelled");
  });

  it("Escape while it runs cancels too, and a denied sign-in shows the provider's sentence", async () => {
    const { user, dialog } = await signInHome();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(callsOf("config_oauth2_cancel")).toHaveLength(1));
    expect(screen.getByRole("dialog", { name: "Sign in home" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Sign in home" })).toBeNull());

    await user.click(within(within(screen.getByRole("region", { name: "Settings" })).getByRole("article", { name: "home" })).getByRole("button", { name: "Sign in" }));
    const again = await screen.findByRole("dialog", { name: "Sign in home" });
    await waitFor(() => expect(callsOf("config_oauth2_login")).toHaveLength(2));
    await waitFor(() => expect(mock.signIns).toHaveLength(1));
    act(() => {
      settleSignIn({ deny: true });
    });
    expect(await within(again).findByRole("alert")).toHaveTextContent(`The sign-in of home failed: ${OAUTH_DENIED}`);
    void dialog;
  });
});

describe("the first run", () => {
  it("shows the setup screen on a daemon with no config.toml, and its wizard writes the first one", async () => {
    const { user, probe } = start(simulateConfigAbsent);
    const title = await screen.findByRole("heading", { name: "Set up mailypoppins" });
    expect(title).toBeInTheDocument();
    expect(screen.getByText(MOCK_CONFIG_PATH)).toBeInTheDocument();
    expect(screen.queryByRole("listbox")).toBeNull();
    const form = screen.getByRole("form", { name: "Account wizard" });
    // No Cancel: there is nothing to go back to.
    expect(within(form).queryByRole("button", { name: "Cancel" })).toBeNull();
    await user.click(within(form).getByRole("radio", { name: /Proton Mail/ }));
    await next(user, form);
    await fill(user, form, "From address", "me@proton.me");
    await next(user, form);
    expect(within(form).getByLabelText("SMTP host")).toHaveValue("127.0.0.1");
    await fill(user, form, "SMTP username", "me@proton.me");
    await next(user, form);
    await next(user, form);
    await user.click(within(form).getByRole("button", { name: "Write config.toml" }));
    await waitFor(() => expect(callsOf("config_init")).toHaveLength(1));
    expect(callsOf("config_init")[0]).toEqual({
      account: {
        name: "proton",
        default_from: "me@proton.me",
        smtp: { host: "127.0.0.1", port: 1025, username: "me@proton.me", accept_invalid_certs: true },
        imap: { host: "127.0.0.1", port: 1143, accept_invalid_certs: true },
        mailboxes: { inbox: "INBOX", archive: "Archive", sent: "Sent", extra: [] },
      },
    });
    expect(callsOf("config_add_account")).toEqual([]);
    // The shell replaces the screen once the account is listed, and the password step follows.
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Set up mailypoppins" })).toBeNull());
    await waitFor(() => expect(document.getElementById("account-proton")).toHaveTextContent("proton"));
    // The first account is selected, its inbox open (behind the modal password dialog).
    await waitFor(() => expect(probe.state.selection).toMatchObject({ account: "proton", mailbox: "inbox" }));
    expect(await screen.findByRole("dialog", { name: "Set SMTP password" })).toHaveAccessibleDescription(/For proton/);
    expect(probe.state.notice).toMatch(/^Wrote config.toml with the account proton/);
  });

  it("shows the ordinary shell with zero accounts when a config.toml exists", async () => {
    start(() => {
      simulateConfigAbsent();
      mock.configState = "ok";
    });
    await waitFor(() => expect(callsOf("config_get")).toHaveLength(1));
    expect(screen.queryByRole("heading", { name: "Set up mailypoppins" })).toBeNull();
  });
});
