import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppShell } from "@/components/shell/AppShell";
import { StoreProvider, useAppState } from "@/app/store";
import type { AppState } from "@/app/state";
import { shellReady } from "@/test/render";
import { setWidth } from "@/test/setup";
import { CONFIG_INVALID_AT, CONFIG_INVALID_MESSAGE, emitEnvelope, mock, MOCK_CONFIG_PATH, resetMock } from "@/test/tauri-mock";

const SECRET = "hunter2-correct-horse";

const callsOf = (cmd: string) => mock.calls.filter((c) => c.cmd === cmd).map((c) => c.args);

/** The app with a probe that reads the model, so a test can look for what must not be in it. */
async function openSettings() {
  resetMock();
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
  await shellReady();
  await user.click(screen.getByRole("button", { name: /^Settings$/ }));
  const view = await screen.findByRole("region", { name: "Settings" });
  await within(view).findByText(MOCK_CONFIG_PATH);
  return { user, probe, view };
}

const card = (view: HTMLElement, name: string) => within(view).getByRole("article", { name });

describe("the Settings view", () => {
  it("shows the daemon's configuration, read once on open, with the focus in the view", async () => {
    const { view } = await openSettings();
    expect(callsOf("config_get")).toHaveLength(1);
    expect(view.contains(document.activeElement)).toBe(true);
    expect(within(view).getByText("encrypted-file")).toBeInTheDocument();
    expect(within(view).getByText("10 seconds")).toBeInTheDocument();
    expect(within(view).getByText("Loaded")).toBeInTheDocument();
    const work = card(view, "work");
    expect(within(work).getByText("Password")).toBeInTheDocument();
    expect(within(work).getByText("Me <me@example.com>")).toBeInTheDocument();
    expect(within(work).getByText("smtp.work.example:465 as me@example.com")).toBeInTheDocument();
    expect(within(work).getByText("imap.work.example:993 as me@example.com")).toBeInTheDocument();
    const home = card(view, "home");
    expect(within(home).getByText("Microsoft 365 (Graph)")).toBeInTheDocument();
    expect(within(home).getByText("client 00000000-0000-0000-0000-00000000f1x7, tenant common")).toBeInTheDocument();
    // The editor setting of M3, in a labelled field.
    await waitFor(() => expect(within(view).getByLabelText("Editor command")).toHaveAttribute("placeholder", "code --wait {path}"));
  });

  it("offers Sign in on the OAuth2 and Graph cards and Add account below them, both live", async () => {
    const { user, view } = await openSettings();
    const home = card(view, "home");
    const signIn = within(home).getByRole("button", { name: "Sign in" });
    expect(signIn).toBeEnabled();
    expect(within(home).queryByRole("button", { name: /password/ })).toBeNull();
    const work = card(view, "work");
    expect(within(work).queryByRole("button", { name: "Sign in" })).toBeNull();
    expect(within(work).getByRole("button", { name: "Set SMTP password" })).toBeEnabled();
    expect(within(work).getByRole("button", { name: "Set IMAP password" })).toBeEnabled();

    // Sign in opens the device-code dialog directly, on a started sign-in.
    await user.click(signIn);
    const dialog = await screen.findByRole("dialog", { name: "Sign in home" });
    await waitFor(() => expect(callsOf("config_oauth2_login")).toEqual([{ account: "home" }]));
    expect(within(dialog).getByRole("status")).toHaveTextContent("Asking the provider for a device code");
    await user.click(within(dialog).getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Close" })).toBeInTheDocument());
    await user.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Sign in home" })).toBeNull());

    const add = within(view).getByRole("button", { name: "Add account" });
    expect(add).toBeEnabled();
    await user.click(add);
    expect(await screen.findByRole("dialog", { name: "Add account" })).toBeInTheDocument();
  });

  it("Open config.toml hands the daemon's file to the editor", async () => {
    const { user, view } = await openSettings();
    await user.click(within(view).getByRole("button", { name: "Open config.toml" }));
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH]));
  });

  it("Reload says what the swap did, logs it once, and reads the configuration again", async () => {
    const { user, probe, view } = await openSettings();
    await user.click(within(view).getByRole("button", { name: "Reload" }));
    await waitFor(() => expect(probe.state.notice).toBe("Configuration reloaded: no account changed"));
    expect(screen.getAllByText("Configuration reloaded: no account changed").length).toBeGreaterThan(0);
    expect(probe.state.activityLog.filter((e) => e.text === "Configuration reloaded: no account changed")).toHaveLength(1);
    await waitFor(() => expect(callsOf("config_get")).toHaveLength(2));
    await waitFor(() => expect(probe.state.config.data?.revision).toBe(1));
  });

  it("a refused reload raises the banner with its line, which opens config.toml and goes with the next good reload", async () => {
    const { user, probe, view } = await openSettings();
    mock.configInvalid = true;
    await user.click(within(view).getByRole("button", { name: "Reload" }));
    const banner = await screen.findByRole("alert", { name: "Configuration problem" });
    expect(banner).toHaveTextContent(`config.toml was refused (${MOCK_CONFIG_PATH}, line ${CONFIG_INVALID_AT}): ${CONFIG_INVALID_MESSAGE}`);
    await waitFor(() => expect(probe.state.notice).toBe(`config.toml was not reloaded: ${CONFIG_INVALID_MESSAGE}`));
    expect(probe.state.activityLog.filter((e) => e.level === "error")).toHaveLength(1);
    // The daemon kept what it served: nothing is read again.
    expect(callsOf("config_get")).toHaveLength(1);
    await user.click(within(banner).getByRole("button", { name: "Open config.toml" }));
    await waitFor(() => expect(mock.editorOpens).toEqual([MOCK_CONFIG_PATH]));
    // Another view keeps the banner: it is the window's, not the view's.
    await user.click(screen.getByRole("button", { name: /^Mail/ }));
    expect(screen.getByRole("alert", { name: "Configuration problem" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /^Settings$/ }));
    mock.configInvalid = false;
    await user.click(await screen.findByRole("button", { name: "Reload" }));
    await waitFor(() => expect(screen.queryByRole("alert", { name: "Configuration problem" })).toBeNull());
  });

  it("an event from another client raises and clears the banner too", async () => {
    await openSettings();
    act(() => emitEnvelope("config.invalid", { path: MOCK_CONFIG_PATH, line: null, message: "retention: body_horizon_days is negative" }));
    const banner = await screen.findByRole("alert", { name: "Configuration problem" });
    expect(banner).toHaveTextContent(`config.toml was refused (${MOCK_CONFIG_PATH}): retention: body_horizon_days is negative`);
    act(() => emitEnvelope("config.changed", { added: [], updated: [], removed: [], config_revision: 1 }));
    await waitFor(() => expect(screen.queryByRole("alert", { name: "Configuration problem" })).toBeNull());
  });
});

describe("the password dialog", () => {
  it("takes a masked, unremembered value in a labelled field, focused on open", async () => {
    const { user, view } = await openSettings();
    await user.click(within(card(view, "work")).getByRole("button", { name: "Set SMTP password" }));
    const dialog = await screen.findByRole("dialog", { name: "Set SMTP password" });
    expect(dialog).toHaveAccessibleDescription(/For work \(me@example.com\); stored in the encrypted-file secrets backend/);
    const field = within(dialog).getByLabelText("Password");
    expect(field).toHaveAttribute("type", "password");
    expect(field).toHaveAttribute("autocomplete", "off");
    await waitFor(() => expect(field).toHaveFocus());
    expect(within(dialog).getByRole("form", { name: "SMTP password for work" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Store password" })).toBeDisabled();
  });

  it("stores the password, says so without the value, and keeps it nowhere in the model", async () => {
    const { user, probe, view } = await openSettings();
    await user.click(within(card(view, "work")).getByRole("button", { name: "Set IMAP password" }));
    const dialog = await screen.findByRole("dialog", { name: "Set IMAP password" });
    await user.type(within(dialog).getByLabelText("Password"), SECRET);
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Set IMAP password" })).toBeNull());
    expect(callsOf("config_set_password")).toEqual([{ account: "work", kind: "imap", value: SECRET }]);
    expect(mock.passwords).toEqual([{ account: "work", kind: "imap", value: "<redacted>" }]);
    expect(probe.state.notice).toBe("Stored the IMAP password for work");
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);
    expect(document.body.innerHTML).not.toContain(SECRET);
    // Opened again, the field starts empty.
    await user.click(within(card(view, "work")).getByRole("button", { name: "Set IMAP password" }));
    const again = await screen.findByRole("dialog", { name: "Set IMAP password" });
    expect(within(again).getByLabelText("Password")).toHaveValue("");
  });

  it("clears the value when it closes, and shows a refusal with the field empty for a retry", async () => {
    const { user, probe, view } = await openSettings();
    await user.click(within(card(view, "work")).getByRole("button", { name: "Set SMTP password" }));
    let dialog = await screen.findByRole("dialog", { name: "Set SMTP password" });
    await user.type(within(dialog).getByLabelText("Password"), SECRET);
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Set SMTP password" })).toBeNull());
    expect(document.body.innerHTML).not.toContain(SECRET);
    expect(probe.state.passwordDialog).toBeNull();

    mock.failing.set("config_set_password", { kind: "internal", message: "storing the smtp password of work: the keyring is locked" });
    await user.click(within(card(view, "work")).getByRole("button", { name: "Set SMTP password" }));
    dialog = await screen.findByRole("dialog", { name: "Set SMTP password" });
    const field = within(dialog).getByLabelText("Password");
    expect(field).toHaveValue("");
    await user.type(field, SECRET);
    await user.click(within(dialog).getByRole("button", { name: "Store password" }));
    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("the keyring is locked"));
    expect(field).toHaveValue("");
    await waitFor(() => expect(field).toHaveFocus());
    expect(JSON.stringify(probe.state)).not.toContain(SECRET);
  });
});
