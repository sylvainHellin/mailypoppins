// The Settings view (clients/desktop/docs/shell.md, "Settings"): the daemon's
// configuration as `config_get` answers it, the reload of config.toml, the
// banner a refused configuration raises, and the password dialog. There is
// no per-key writer: a setting changes in config.toml, through the editor,
// then Reload. Pure functions over the model, which the reducer routes to,
// and at the bottom what the view's buttons run.

import type { Dispatch } from "react";
import type { ConfigChanged, ConfigInvalid } from "@/protocol/types";
import type { Action } from "@/app/reducer";
import { configChangedLine } from "@/app/activity";
import { accountNames, markStale, type AccountWizard, type AppState, type PasswordDialog } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, type ConfigAccount, type ConfigSnapshot, type ConfigSwap, type GuiError, type SecretKind } from "@/lib/gui-types";

/** The daemon's `ConfigInvalid` code: the file did not load, and the daemon kept what it served. */
export const CONFIG_INVALID_CODE = -32007;


/** The word a secret kind shows as. */
export function kindLabel(kind: SecretKind): string {
  return kind === "smtp" ? "SMTP" : "IMAP";
}

/** The notice a stored password leaves: which one, for which account, never the value. */
export function storedLine(account: string, kind: SecretKind): string {
  return `Stored the ${kindLabel(kind)} password for ${account}`;
}

/** What a reload did, in the words of the activity log's `config.changed` line. */
export function swapLine(swap: ConfigSwap): string {
  return configChangedLine({ ...swap, config_revision: 0 });
}

/** Whether an account signs in with a password, and so has passwords to set. */
export function usesPassword(account: ConfigAccount): boolean {
  return account.auth_method === "password" || account.auth_method === "";
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/** The Settings view opened: its configuration is read again. */
export function openSettings(s: AppState): AppState {
  return { ...s, config: markStale(s.config) };
}

/**
 * `config.get` answered. An answer to the current read also says whether
 * the daemon started on a file that did not load: `invalid` shows the
 * banner, with the reason of a `config.invalid` when one came, and any
 * other state drops a startup banner, since the file has loaded since. An
 * older answer, such as one from a daemon that has since restarted, leaves
 * the banner alone.
 */
export function configLoaded(s: AppState, gen: number, snapshot: ConfigSnapshot): AppState {
  const next: AppState = { ...s, config: { ...s.config, data: snapshot, loadedGen: gen, error: null } };
  if (gen !== s.config.gen) return next;
  const p = s.configProblem;
  if (snapshot.state === "invalid") {
    return { ...next, configProblem: p ? { ...p, atStartup: true } : { path: snapshot.path, line: null, message: "", atStartup: true } };
  }
  return p?.atStartup ? { ...next, configProblem: null } : next;
}

export function configFailed(s: AppState, gen: number, error: GuiError): AppState {
  return { ...s, config: { ...s.config, loadedGen: gen, error } };
}

/**
 * `config.invalid`: the banner shows the file, the line and why, until a
 * configuration loads. A refused reload changes nothing the daemon serves,
 * so a daemon that started on a bad file still serves none.
 */
export function configInvalid(s: AppState, p: ConfigInvalid): AppState {
  return { ...s, configProblem: { path: p.path, line: p.line ?? null, message: p.message, atStartup: s.configProblem?.atStartup ?? false } };
}

/**
 * `config.changed`: a configuration loaded, so the banner goes, the view's
 * copy and the account list are read again, and each added or updated
 * account's mailboxes too. A removed account is gone already: the daemon
 * publishes `state.remove` of `account:<name>` for it before this event, so
 * `remove` runs only for a name the window still knows.
 */
export function configChanged(
  s: AppState,
  p: ConfigChanged,
  stale: (s: AppState, account: string) => AppState,
  remove: (s: AppState, account: string) => AppState,
): AppState {
  let next: AppState = { ...s, configProblem: null, config: markStale(s.config), accounts: markStale(s.accounts) };
  for (const name of [...(p.added ?? []), ...(p.updated ?? [])]) next = stale(next, name);
  for (const name of p.removed ?? []) if (accountNames(next).includes(name)) next = remove(next, name);
  return next;
}

/** Open the password dialog for one password of one account. */
export function openPasswordDialog(s: AppState, dialog: PasswordDialog): AppState {
  return {
    ...s,
    overlay: "password",
    passwordDialog: dialog,
    dialog: null,
    composeDialog: null,
    attachDialog: null,
    rsvpDialog: null,
    inviteDialog: null,
    signaturesDialog: null,
    accountWizard: null,
  };
}

/** Open the account wizard on `preset`. */
export function openAccountWizard(s: AppState, preset: AccountWizard["preset"]): AppState {
  return {
    ...s,
    overlay: "account_wizard",
    accountWizard: { preset },
    dialog: null,
    composeDialog: null,
    attachDialog: null,
    rsvpDialog: null,
    inviteDialog: null,
    signaturesDialog: null,
    passwordDialog: null,
  };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/**
 * Reload config.toml. The daemon's `config.changed` or `config.invalid`
 * logs the outcome, so the notice here says it again on the notice line
 * without a second log line; any other failure is a logged error.
 */
export async function reloadConfig(dispatch: Dispatch<Action>): Promise<void> {
  try {
    const swap = await cmd.configReload();
    dispatch({ type: "config_reloaded", text: swapLine(swap) });
  } catch (e: unknown) {
    const error = asGuiError(e);
    if ("code" in error && error.code === CONFIG_INVALID_CODE) {
      dispatch({ type: "config_reloaded", text: `config.toml was not reloaded: ${error.message}` });
    } else {
      dispatch({ type: "notice", text: `Reload failed: ${error.message}`, level: "error" });
    }
  }
}

/**
 * Store one password. The value goes to the Rust layer and nowhere else:
 * not the model, not a notice, not the log. Answers the refusal's sentence
 * for the dialog to show, or null once it is stored and the dialog closed.
 */
export async function storePassword(dispatch: Dispatch<Action>, account: string, kind: SecretKind, value: string): Promise<string | null> {
  try {
    await cmd.configSetPassword(account, kind, value);
    dispatch({ type: "overlay", overlay: null });
    dispatch({ type: "notice", text: storedLine(account, kind) });
    return null;
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
}

/** Save the editor command template, or clear it with an empty field. */
export async function saveEditorSetting(dispatch: Dispatch<Action>, template: string): Promise<string | null> {
  const editor = template.trim() === "" ? null : template.trim();
  try {
    const setting = await cmd.editorSettingSet(editor);
    dispatch({ type: "notice", text: setting.editor ? `The editor is now ${setting.editor}` : `The editor setting is cleared; ${setting.effective} is used` });
    return null;
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
}
