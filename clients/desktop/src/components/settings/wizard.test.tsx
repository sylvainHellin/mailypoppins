import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppShell } from "@/components/shell/AppShell";
import { StoreProvider, useAppState } from "@/app/store";
import type { AppState } from "@/app/state";
import { cancelledLine, failedLine } from "@/app/signin";
import { mailboxTwice, nameTaken, PASSWORD_EMPTY } from "@/app/wizard";
import { shellReady } from "@/test/render";
import { setWidth } from "@/test/setup";
import {
  emit,
  fixtures,
  mock,
  MOCK_CONFIG_PATH,
  OAUTH_DENIED,
  PROBE_GRAPH_FOLDERS,
  PROBE_IMAP_MAILBOXES,
  PROBE_WRONG_PASSWORD,
  probeConfiguredRefusal,
  probeRefused,
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

const stepOf = (scope: HTMLElement) => within(scope).getByRole("form", { name: "Account wizard" }).getAttribute("data-step");

/** Through provider, identity and servers of the IMAP preset, to the Connection step. */
async function toConnection(user: User, dialog: HTMLElement, name: string) {
  await next(user, dialog);
  await fill(user, dialog, "Account name", name);
  await next(user, dialog);
  await fill(user, dialog, "SMTP host", "smtp.fastmail.com");
  await fill(user, dialog, "SMTP username", "me@fastmail.com");
  await fill(user, dialog, "IMAP host", "imap.fastmail.com");
  await next(user, dialog);
  expect(stepOf(dialog)).toBe("connection");
}

/** Type the SMTP password and run the test until it passes. */
async function testWith(user: User, dialog: HTMLElement, password: string) {
  await fill(user, dialog, "SMTP password", password);
  await user.click(within(dialog).getByRole("button", { name: "Test connection" }));
}

/** The candidate a probe or a sign-in carries: the draft with no mailbox mapping. */
const FASTMAIL = {
  name: "fastmail",
  default_from: "me@fastmail.com",
  smtp: { host: "smtp.fastmail.com", port: 465, username: "me@fastmail.com" },
  imap: { host: "imap.fastmail.com", port: 993 },
};

describe("the account wizard", () => {
  it("opens on the provider, focused, with every field labelled", async () => {
    const { user, dialog } = await openWizard();
    const imap = within(dialog).getByRole("radio", { name: /^IMAP and SMTP/ });
    await waitFor(() => expect(imap).toHaveFocus());
    expect(imap).toBeChecked();
    expect(within(dialog).getAllByRole("radio")).toHaveLength(4);
    expect(within(dialog).getByRole("radio", { name: /Proton Mail/ })).toHaveAccessibleDescription(/bridge password/);
    expect(within(dialog).getByRole("list", { name: "Steps" }).querySelector("[aria-current='step']")).toHaveTextContent("1. Provider");
    expect(within(dialog).getByRole("list", { name: "Steps" })).toHaveTextContent("4. Connection");
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
    expect(stepOf(dialog)).toBe("servers");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("SMTP host")).toHaveAccessibleDescription(/The SMTP host is required/);
    expect(within(dialog).getByLabelText("SMTP username")).toHaveAttribute("aria-invalid", "true");
    expect(callsOf("config_add_account")).toEqual([]);
    expect(callsOf("config_probe")).toEqual([]);
  });

  it("tests the logins before anything is written, shows the server's refusal, and stores the password once the account is", async () => {
    const { user, probe, dialog } = await openWizard();
    await toConnection(user, dialog, "fastmail");
    expect(within(dialog).getByLabelText("IMAP uses the SMTP password")).toBeChecked();
    // No password, no test.
    await user.click(within(dialog).getByRole("button", { name: "Test connection" }));
    expect(within(dialog).getByLabelText("SMTP password")).toHaveAccessibleDescription(PASSWORD_EMPTY);
    expect(callsOf("config_probe")).toEqual([]);

    // A password the server refuses: its sentence, and the wizard stays.
    await testWith(user, dialog, PROBE_WRONG_PASSWORD);
    const results = await within(dialog).findByRole("list", { name: "Connection test" });
    expect(results).toHaveTextContent(`SMTP: the login failed. ${probeRefused("smtp")}`);
    expect(callsOf("config_probe")).toEqual([{ account: FASTMAIL, check: "smtp", password: PROBE_WRONG_PASSWORD }]);
    expect(within(dialog).queryByRole("button", { name: "Next" })).toBeNull();

    // The right one: SMTP, IMAP with the same password, the mailbox list.
    mock.calls = [];
    await testWith(user, dialog, SECRET);
    await waitFor(() => expect(within(dialog).getByRole("list", { name: "Connection test" })).toHaveTextContent("Mailboxes: the server listed 7."));
    const lines = within(within(dialog).getByRole("list", { name: "Connection test" })).getAllByRole("listitem");
    expect(lines.map((l) => l.textContent)).toEqual([
      "\u2713SMTP: the server accepted the login.",
      "\u2713IMAP: the server accepted the login.",
      "\u2713Mailboxes: the server listed 7.",
    ]);
    expect(callsOf("config_probe").map((c) => [c?.check, c?.password === SECRET])).toEqual([
      ["smtp", true],
      ["imap", true],
      ["mailboxes", true],
    ]);
    expect(callsOf("config_add_account")).toEqual([]);
    expect(callsOf("config_set_password")).toEqual([]);
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);

    await next(user, dialog);
    // The server's mailboxes, the CLI's picks preselected, the marked roles left to the server.
    const inbox = within(dialog).getByLabelText("Inbox");
    expect(inbox.tagName).toBe("SELECT");
    expect(within(inbox).getAllByRole("option").map((o) => o.textContent)).toEqual(PROBE_IMAP_MAILBOXES);
    expect(inbox).toHaveValue("INBOX");
    expect(within(dialog).getByLabelText("Archive")).toHaveValue("Archive");
    expect(within(dialog).getByLabelText("Sent")).toHaveValue("Sent");
    expect(within(dialog).getByLabelText("Junk")).toHaveValue("");
    await user.selectOptions(within(dialog).getByLabelText("Junk"), "Spam");
    // One mailbox plays one role.
    await user.selectOptions(within(dialog).getByLabelText("Archive"), "INBOX");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Archive")).toHaveAccessibleDescription(mailboxTwice("INBOX", "inbox"));
    await user.selectOptions(within(dialog).getByLabelText("Archive"), "Archive");
    await fill(user, dialog, "Policy overrides", "Lists/* status-only");
    await next(user, dialog);
    expect(within(dialog).getByText("smtp.fastmail.com:465 as me@fastmail.com")).toBeInTheDocument();
    expect(within(dialog).getByText("SMTP login passed, IMAP login passed, mailbox list passed")).toBeInTheDocument();
    expect(within(dialog).getByText("Lists/* status-only")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() =>
      expect(callsOf("config_add_account")).toEqual([
        {
          account: {
            ...FASTMAIL,
            mailboxes: { inbox: "INBOX", archive: "Archive", sent: "Sent", junk: "Spam", overrides: [{ match: "Lists/*", policy: "status-only" }] },
          },
        },
      ]),
    );
    expect(callsOf("config_init")).toEqual([]);
    // The password the test used, stored now that the account exists; IMAP falls back to it.
    await waitFor(() => expect(mock.passwords).toEqual([{ account: "fastmail", kind: "smtp", value: "<redacted>" }]));
    expect(callsOf("config_set_password")).toEqual([{ account: "fastmail", kind: "smtp", value: SECRET }]);
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).toBeNull());
    expect(screen.queryByRole("dialog", { name: /password/ })).toBeNull();
    expect(probe.state.notice).toBe("Added the account fastmail; its password is stored");
    await waitFor(() => expect(document.getElementById("account-fastmail")).toHaveTextContent("fastmail"));
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);
    expect(document.body.innerHTML).not.toContain(SECRET);
  });

  it("an IMAP password of its own must pass its own login, and is stored too", async () => {
    const { user, probe, dialog } = await openWizard();
    await toConnection(user, dialog, "fastmail");
    await user.click(within(dialog).getByLabelText("IMAP uses the SMTP password"));
    await fill(user, dialog, "SMTP password", SECRET);
    await fill(user, dialog, "IMAP password", PROBE_WRONG_PASSWORD);
    await user.click(within(dialog).getByRole("button", { name: "Test connection" }));
    const results = await within(dialog).findByRole("list", { name: "Connection test" });
    expect(results).toHaveTextContent(`IMAP: the login failed. ${probeRefused("imap")}`);
    // A failed login that must pass stops the test: no mailbox list.
    expect(callsOf("config_probe").map((c) => c?.check)).toEqual(["smtp", "imap"]);
    expect(within(dialog).queryByRole("button", { name: "Next" })).toBeNull();
    await fill(user, dialog, "IMAP password", "imap-secret");
    await user.click(within(dialog).getByRole("button", { name: "Test connection" }));
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Next" })).toBeInTheDocument());
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(mock.passwords.map((p) => p.kind)).toEqual(["smtp", "imap"]));
    expect(probe.state.notice).toBe("Added the account fastmail; its SMTP and IMAP passwords are stored");
  });

  it("a write that answers after the wizard closed still stores the password and opens nothing over the overlay opened since", async () => {
    const { user, probe, dialog } = await openWizard();
    let release = () => {};
    await toConnection(user, dialog, "late");
    await testWith(user, dialog, SECRET);
    await next(user, dialog);
    await next(user, dialog);
    mock.gates.set("config_add_account", new Promise<void>((r) => (release = r)));
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(callsOf("config_add_account")).toHaveLength(1));
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).toBeNull());
    await user.click(screen.getByRole("button", { name: "Activity log, key s l" }));
    await screen.findByRole("dialog", { name: "Activity log" });
    await act(async () => release());
    await waitFor(() => expect(probe.state.notice).toBe("Added the account late; its password is stored"));
    expect(mock.passwords).toEqual([{ account: "late", kind: "smtp", value: "<redacted>" }]);
    expect(probe.state.overlay).toBe("activity");
  });

  it("a password the daemon does not store leaves the account written, says so, and opens the password dialog", async () => {
    const { user, probe, dialog } = await openWizard();
    await toConnection(user, dialog, "fastmail");
    await testWith(user, dialog, SECRET);
    await next(user, dialog);
    await next(user, dialog);
    mock.failing.set("config_set_password", { kind: "internal", message: "the keyring is locked" });
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    expect(await screen.findByRole("dialog", { name: "Set SMTP password" })).toHaveAccessibleDescription(/For fastmail/);
    expect(probe.state.notice).toBe("Account fastmail is configured, but its SMTP password is not stored, so it cannot log in yet. Set it in Settings.");
    expect(callsOf("config_add_account")).toHaveLength(1);
  });

  it("a wizard cancelled after a passed test writes and stores nothing", async () => {
    const { user, dialog } = await openWizard();
    await toConnection(user, dialog, "fastmail");
    await testWith(user, dialog, SECRET);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).toBeNull());
    expect(callsOf("config_probe").length).toBeGreaterThan(0);
    expect(callsOf("config_add_account")).toEqual([]);
    expect(callsOf("config_set_password")).toEqual([]);
    expect(mock.passwords).toEqual([]);
  });

  it("shows the daemon's refusal of the write and stays open", async () => {
    const { user, dialog } = await openWizard();
    mock.failing.set("config_add_account", { kind: "protocol", code: -32007, message: "accounts[2].smtp.host: invalid" });
    await toConnection(user, dialog, "x");
    await testWith(user, dialog, SECRET);
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("accounts[2].smtp.host: invalid"));
    expect(screen.queryByRole("dialog", { name: /password/ })).toBeNull();
    expect(callsOf("config_set_password")).toEqual([]);
  });

  it("names a probe refused for a configured name as a taken name", async () => {
    const { user, dialog } = await openWizard();
    await toConnection(user, dialog, "fastmail");
    mock.failing.set("config_probe", { kind: "protocol", code: -32602, message: probeConfiguredRefusal("fastmail") });
    await testWith(user, dialog, SECRET);
    await waitFor(() =>
      expect(within(dialog).getByRole("alert")).toHaveTextContent(`${nameTaken("fastmail")}. Go back to Identity and choose another name.`),
    );
    expect(stepOf(dialog)).toBe("connection");
  });

  it("a Microsoft 365 account signs its candidate in on the Connection step, tests it, and the write promotes the sign-in", async () => {
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
    expect(stepOf(dialog)).toBe("connection");
    expect(within(dialog).queryByLabelText("SMTP password")).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    await waitFor(() =>
      expect(callsOf("config_oauth2_login")).toEqual([
        {
          account: "exchange",
          candidate: {
            name: "exchange",
            default_from: "me@contoso.com",
            auth_method: "oauth2",
            oauth2: { client_id: "client-1", tenant_id: "contoso.com" },
            smtp: { host: "smtp.office365.com", port: 587, username: "me@contoso.com" },
            imap: { host: "outlook.office365.com", port: 993 },
          },
        },
      ]),
    );
    // The code shows in the wizard, which stays open: no device-code dialog over it.
    await waitFor(() => expect(probe.state.signIn?.operation_id).toBe("fixture-oauth-1"));
    expect(probe.state.signIn?.candidate).toBe(true);
    expect(probe.state.overlay).toBe("account_wizard");
    act(() => {
      reportDeviceCode();
    });
    expect(await within(dialog).findByText("FXTR-CODE")).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Sign in exchange" })).toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Copy code" }));
    expect(writeText).toHaveBeenCalledWith("FXTR-CODE");
    await user.click(within(dialog).getByRole("button", { name: "Open verification page" }));
    await waitFor(() => expect(callsOf("open_external")).toEqual([{ url: "https://microsoft.com/devicelogin" }]));
    act(() => {
      settleSignIn();
    });
    // Stored: the test runs at once, with the staged token.
    await waitFor(() => expect(within(dialog).getByRole("list", { name: "Connection test" })).toHaveTextContent("Mailboxes: the server listed 7."));
    expect(callsOf("config_probe")).toEqual([{ account: (callsOf("config_oauth2_login")[0] as { candidate: unknown }).candidate, check: "mailboxes" }]);
    expect(within(dialog).getByText("Signed in. The token waits for the account to be written.")).toBeInTheDocument();
    expect(probe.state.signIn).toBeNull();
    expect(probe.state.activityLog.map((e) => e.text).join("\n")).not.toMatch(/OAuth2 token acquired/);
    expect(mock.stagedSignIns.has("exchange")).toBe(true);
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(callsOf("config_add_account")).toHaveLength(1));
    expect((callsOf("config_add_account")[0]?.account as { auth_method: string }).auth_method).toBe("oauth2");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).toBeNull());
    expect(probe.state.notice).toBe("Added the account exchange; its sign-in is stored");
    // Promoted by the write; no second sign-in, no password.
    expect(mock.stagedSignIns.has("exchange")).toBe(false);
    expect(callsOf("config_oauth2_login")).toHaveLength(1);
    expect(callsOf("config_set_password")).toEqual([]);
  });

  it("a Graph account left without a client and tenant signs in through the mailypoppins app and lists its folders", async () => {
    const { user, dialog } = await openWizard();
    await user.click(within(dialog).getByRole("radio", { name: /Graph API/ }));
    await next(user, dialog);
    await fill(user, dialog, "Email address", "me@contoso.com");
    await next(user, dialog);
    expect(within(dialog).getByLabelText("Client ID")).toHaveValue("");
    expect(within(dialog).getByLabelText("Client ID")).toHaveAccessibleDescription(/shared mailypoppins app/);
    expect(within(dialog).getByLabelText("Tenant ID")).toHaveAccessibleDescription(/Empty uses common/);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(mock.signIns).toHaveLength(1));
    act(() => {
      settleSignIn();
    });
    await waitFor(() => expect(within(dialog).getByRole("list", { name: "Connection test" })).toHaveTextContent("Graph: the sign-in reaches the mailbox."));
    expect(callsOf("config_probe").map((c) => c?.check)).toEqual(["graph", "mailboxes"]);
    await next(user, dialog);
    expect(within(within(dialog).getByLabelText("Sent")).getAllByRole("option").map((o) => o.textContent)).toEqual(PROBE_GRAPH_FOLDERS);
    expect(within(dialog).getByLabelText("Inbox")).toHaveValue("Inbox");
    expect(within(dialog).getByLabelText("Sent")).toHaveValue("Sent Items");
    await next(user, dialog);
    expect(within(dialog).getByText("the mailypoppins app, tenant common")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Add account" }));
    await waitFor(() => expect(callsOf("config_add_account")).toHaveLength(1));
    const account = callsOf("config_add_account")[0]?.account as { auth_method: string; oauth2: unknown; mailboxes: unknown };
    expect(account.auth_method).toBe("graph");
    expect(account.oauth2).toEqual({ client_id: "439d7ebd-7898-4df2-b2d6-88bb61e2597b", tenant_id: "common" });
    expect(account.mailboxes).toEqual({ inbox: "Inbox", archive: "Archive", sent: "Sent Items" });
  });

  it("a denied sign-in shows the provider's sentence and asks again; a cancelled one is forgotten", async () => {
    const { user, probe, dialog } = await openWizard();
    await user.click(within(dialog).getByRole("radio", { name: /Graph API/ }));
    await next(user, dialog);
    await fill(user, dialog, "Email address", "me@contoso.com");
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(mock.signIns).toHaveLength(1));
    act(() => {
      settleSignIn({ deny: true });
    });
    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent(failedLine("exchange", OAUTH_DENIED)));
    expect(callsOf("config_probe")).toEqual([]);
    expect(probe.state.signIn).toBeNull();

    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(probe.state.signIn?.operation_id).toBe("fixture-oauth-2"));
    // Back and Next wait while it runs.
    expect(within(dialog).getByRole("button", { name: "Back" })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-oauth-2" }]));
    await waitFor(() => expect(probe.state.signIn).toBeNull());
    expect(within(dialog).getByRole("button", { name: "Sign in" })).toBeEnabled();
    expect(within(dialog).getByRole("alert")).toHaveTextContent("");
  });

  it("closing the wizard while its sign-in runs cancels the sign-in", async () => {
    const { user, probe, dialog } = await openWizard();
    await user.click(within(dialog).getByRole("radio", { name: /Graph API/ }));
    await next(user, dialog);
    await fill(user, dialog, "Email address", "me@contoso.com");
    await next(user, dialog);
    await next(user, dialog);
    await user.click(within(dialog).getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(probe.state.signIn?.operation_id).toBe("fixture-oauth-1"));
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Add account" })).toBeNull());
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-oauth-1" }]));
    await waitFor(() => expect(probe.state.signIn).toBeNull());
    expect(callsOf("config_add_account")).toEqual([]);
    expect(mock.stagedSignIns.size).toBe(0);
  });

  it("a change of the servers after a test makes it stale, so the Connection step tests again", async () => {
    const { user, dialog } = await openWizard();
    await toConnection(user, dialog, "fastmail");
    await testWith(user, dialog, SECRET);
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Next" })).toBeInTheDocument());
    await user.click(within(dialog).getByRole("button", { name: "Back" }));
    await fill(user, dialog, "IMAP host", "imap2.fastmail.com");
    await next(user, dialog);
    expect(within(dialog).getByRole("button", { name: "Test connection" })).toBeInTheDocument();
    expect(within(dialog).queryByRole("list", { name: "Connection test" })).toBeNull();
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
    await waitFor(() => expect(callsOf("operation_cancel")).toEqual([{ operation_id: "fixture-oauth-1" }]));
    expect(await within(dialog).findByText(cancelledLine("home"))).toBeInTheDocument();
    expect(within(dialog).getByText(/may still complete/)).toBeInTheDocument();
    expect(probe.state.signIn?.outcome?.kind).toBe("cancelled");
  });

  it("Escape while it runs cancels too, and a denied sign-in shows the provider's sentence", async () => {
    const { user, dialog } = await signInHome();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(callsOf("operation_cancel")).toHaveLength(1));
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
  it("shows the setup screen on a daemon with no config.toml, and its wizard tests and writes the first one", async () => {
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
    await testWith(user, form, SECRET);
    await waitFor(() => expect(within(form).getByRole("button", { name: "Next" })).toBeInTheDocument());
    expect(callsOf("config_probe")[0]).toEqual({
      account: {
        name: "proton",
        default_from: "me@proton.me",
        smtp: { host: "127.0.0.1", port: 1025, username: "me@proton.me", accept_invalid_certs: true },
        imap: { host: "127.0.0.1", port: 1143, accept_invalid_certs: true },
      },
      check: "smtp",
      password: SECRET,
    });
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
        mailboxes: { inbox: "INBOX", archive: "Archive", sent: "Sent" },
      },
    });
    expect(callsOf("config_add_account")).toEqual([]);
    // The shell replaces the screen once the account is listed, and the password is stored all the same.
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Set up mailypoppins" })).toBeNull());
    await waitFor(() => expect(document.getElementById("account-proton")).toHaveTextContent("proton"));
    await waitFor(() => expect(probe.state.selection).toMatchObject({ account: "proton", mailbox: "inbox" }));
    await waitFor(() => expect(mock.passwords).toEqual([{ account: "proton", kind: "smtp", value: "<redacted>" }]));
    expect(probe.state.notice).toBe("Wrote config.toml with the account proton; its password is stored");
    expect(screen.queryByRole("dialog", { name: "Set SMTP password" })).toBeNull();
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
