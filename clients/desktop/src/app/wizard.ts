// The account wizard (ACC-01, ACC-02; clients/desktop/docs/shell.md,
// "Account wizard"): the CLI wizard's four provider presets
// (src/config_cmd/init.rs:80-146), the form's steps, what each step checks
// before it lets the user on, and the `AccountDraft` the review writes.
// Pure; the dialog and the first-run screen hold the form in their own
// state. No password field exists here: the password step runs after the
// account is written, through the password dialog.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { startSignIn } from "@/app/signin";
import * as cmd from "@/lib/commands";
import { asGuiError, type AccountDraft, type AuthMethod } from "@/lib/gui-types";

export type Preset = "imap" | "proton" | "microsoft365" | "graph";

export type PresetInfo = {
  label: string;
  /** What the provider needs, one sentence. */
  hint: string;
  auth: AuthMethod;
  /** The name the CLI suggests. */
  name: string;
  smtpHost: string;
  smtpPort: string;
  imapHost: string;
  imapPort: string;
  /** Proton Bridge's self-signed certificate. */
  invalidCerts: boolean;
  inbox: string;
  archive: string;
  sent: string;
};

/** The CLI wizard's presets, in its order and with its defaults. */
export const PRESETS: Record<Preset, PresetInfo> = {
  imap: {
    label: "IMAP and SMTP",
    hint: "Any provider with IMAP and SMTP and a password.",
    auth: "password",
    name: "main",
    smtpHost: "",
    smtpPort: "465",
    imapHost: "",
    imapPort: "993",
    invalidCerts: false,
    inbox: "INBOX",
    archive: "Archive",
    sent: "Sent",
  },
  proton: {
    label: "Proton Mail (Proton Bridge)",
    hint: "Proton Bridge must be running; use the bridge password, not your Proton password.",
    auth: "password",
    name: "proton",
    smtpHost: "127.0.0.1",
    smtpPort: "1025",
    imapHost: "127.0.0.1",
    imapPort: "1143",
    invalidCerts: true,
    inbox: "INBOX",
    archive: "Archive",
    sent: "Sent",
  },
  microsoft365: {
    label: "Microsoft 365 (OAuth2, IMAP and SMTP)",
    hint: "Needs an Azure Entra ID app registration (docs/exchange-setup.md); you sign in with a device code.",
    auth: "oauth2",
    name: "exchange",
    smtpHost: "smtp.office365.com",
    smtpPort: "587",
    imapHost: "outlook.office365.com",
    imapPort: "993",
    invalidCerts: false,
    inbox: "INBOX",
    archive: "Archive",
    sent: "Sent",
  },
  graph: {
    label: "Microsoft 365 (Graph API)",
    hint: "Needs an Azure Entra ID app registration; no IMAP or SMTP server, you sign in with a device code.",
    auth: "graph",
    name: "exchange",
    smtpHost: "",
    smtpPort: "",
    imapHost: "",
    imapPort: "",
    invalidCerts: false,
    inbox: "Inbox",
    archive: "Archive",
    sent: "Sent Items",
  },
};

export const PRESET_ORDER: Preset[] = ["imap", "proton", "microsoft365", "graph"];

export type WizardForm = {
  preset: Preset;
  name: string;
  default_from: string;
  smtp_host: string;
  smtp_port: string;
  smtp_username: string;
  imap_host: string;
  imap_port: string;
  imap_username: string;
  client_id: string;
  tenant_id: string;
  inbox: string;
  archive: string;
  sent: string;
  /** Extra mailboxes to sync, one server name per line. */
  extra: string;
};

export type WizardField = Exclude<keyof WizardForm, "preset">;

export type WizardStep = "provider" | "identity" | "servers" | "mailboxes" | "review";

export const STEPS: WizardStep[] = ["provider", "identity", "servers", "mailboxes", "review"];

export const STEP_TITLES: Record<WizardStep, string> = {
  provider: "Provider",
  identity: "Identity",
  servers: "Servers",
  mailboxes: "Mailboxes",
  review: "Review",
};

/** A fresh form for `preset`, its defaults filled in. */
export function presetForm(preset: Preset): WizardForm {
  const p = PRESETS[preset];
  return {
    preset,
    name: p.name,
    default_from: "",
    smtp_host: p.smtpHost,
    smtp_port: p.smtpPort,
    smtp_username: "",
    imap_host: p.imapHost,
    imap_port: p.imapPort,
    imap_username: "",
    client_id: "",
    tenant_id: "",
    inbox: p.inbox,
    archive: p.archive,
    sent: p.sent,
    extra: "",
  };
}

/**
 * Switch the preset: the provider's own values (hosts, ports, mailbox
 * names, and the suggested name while the user kept the last one's) are
 * replaced, what the user typed about themselves is kept.
 */
export function withPreset(form: WizardForm, preset: Preset): WizardForm {
  const fresh = presetForm(preset);
  const keptName = form.name.trim() !== "" && form.name !== PRESETS[form.preset].name;
  return {
    ...fresh,
    name: keptName ? form.name : fresh.name,
    default_from: form.default_from,
    smtp_username: form.smtp_username,
    imap_username: form.imap_username,
    client_id: form.client_id,
    tenant_id: form.tenant_id,
  };
}

/** Whether the preset talks IMAP and SMTP (every one but Graph). */
export function usesServers(preset: Preset): boolean {
  return PRESETS[preset].auth !== "graph";
}

/** Whether the preset signs in with a device code rather than a password. */
export function signsIn(preset: Preset): boolean {
  return PRESETS[preset].auth !== "password";
}

/** An account name: it names the account in `mp://` links and its directory on disk. */
const SLUG = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

export const NAME_EMPTY = "Give the account a name";
export const NAME_SHAPE = "Use letters, digits, '.', '-' and '_' only, starting with a letter or a digit";
export const nameTaken = (name: string): string => `An account named ${name} is already configured`;
export const HOST_EMPTY = "The SMTP host is required";
export const USERNAME_EMPTY = "The SMTP username is required";
export const FROM_EMPTY = "The email address is required";
export const CLIENT_EMPTY = "The app registration's client ID is required";
export const TENANT_EMPTY = "The tenant ID is required";
export const MAILBOX_EMPTY = "A server mailbox name is required";
export const PORT_BAD = "A port is a whole number from 1 to 65535";

function port(value: string): boolean {
  if (!/^\d+$/.test(value.trim())) return false;
  const n = Number(value.trim());
  return n >= 1 && n <= 65535;
}

/**
 * What stops `step` from moving on, field by field; empty when it may.
 * `taken` is every configured account name, for the name's uniqueness.
 */
export function stepErrors(step: WizardStep, form: WizardForm, taken: readonly string[]): Partial<Record<WizardField, string>> {
  const out: Partial<Record<WizardField, string>> = {};
  const blank = (v: string) => v.trim() === "";
  if (step === "identity" || step === "review") {
    const name = form.name.trim();
    if (name === "") out.name = NAME_EMPTY;
    else if (!SLUG.test(name)) out.name = NAME_SHAPE;
    else if (taken.includes(name)) out.name = nameTaken(name);
    if (form.preset === "graph" && blank(form.default_from)) out.default_from = FROM_EMPTY;
  }
  if (step === "servers" || step === "review") {
    if (usesServers(form.preset)) {
      if (blank(form.smtp_host)) out.smtp_host = HOST_EMPTY;
      if (!port(form.smtp_port)) out.smtp_port = PORT_BAD;
      if (blank(form.smtp_username)) out.smtp_username = USERNAME_EMPTY;
      if (!port(form.imap_port)) out.imap_port = PORT_BAD;
    }
    if (signsIn(form.preset)) {
      if (blank(form.client_id)) out.client_id = CLIENT_EMPTY;
      if (blank(form.tenant_id)) out.tenant_id = TENANT_EMPTY;
    }
  }
  if (step === "mailboxes" || step === "review") {
    for (const role of ["inbox", "archive", "sent"] as const) if (blank(form[role])) out[role] = MAILBOX_EMPTY;
  }
  return out;
}

/** The step a field is entered on, for the review's "fix it" jump. */
export function stepOf(field: WizardField): WizardStep {
  if (field === "name" || field === "default_from") return "identity";
  if (field === "inbox" || field === "archive" || field === "sent" || field === "extra") return "mailboxes";
  return "servers";
}

/**
 * The `AccountDraft` the form describes, with exactly the keys the
 * daemon's `account_block` reads: a password account leaves `auth_method`
 * to the daemon's default, an empty IMAP host or username falls back to the
 * SMTP one there, and a Graph account has no server at all. The From
 * address defaults to the SMTP username, as the CLI's prompt does.
 */
export function accountDraft(form: WizardForm): AccountDraft {
  const t = (v: string) => v.trim();
  const p = PRESETS[form.preset];
  const from = t(form.default_from) || t(form.smtp_username);
  const draft: AccountDraft = { name: t(form.name) };
  if (from) draft.default_from = from;
  if (p.auth !== "password") {
    draft.auth_method = p.auth;
    draft.oauth2 = { client_id: t(form.client_id), tenant_id: t(form.tenant_id) };
  }
  if (usesServers(form.preset)) {
    const certs = p.invalidCerts ? { accept_invalid_certs: true } : {};
    draft.smtp = { host: t(form.smtp_host), port: Number(t(form.smtp_port)), username: t(form.smtp_username), ...certs };
    draft.imap = { port: Number(t(form.imap_port)), ...certs };
    if (t(form.imap_host)) draft.imap.host = t(form.imap_host);
    if (t(form.imap_username)) draft.imap.username = t(form.imap_username);
  }
  const extra = form.extra
    .split("\n")
    .map(t)
    .filter((line) => line !== "");
  draft.mailboxes = { inbox: t(form.inbox), archive: t(form.archive), sent: t(form.sent), extra };
  return draft;
}

/** The notice a written account leaves, and what comes next. */
export function writtenLine(name: string, preset: Preset, init: boolean): string {
  const where = init ? "Wrote config.toml with the account" : "Added the account";
  const next = signsIn(preset) ? "sign in with the device code next" : "store its SMTP password next (IMAP uses it too unless you set its own)";
  return `${where} ${name}; ${next}`;
}

/**
 * The review's write: `config_init` on a daemon with no configuration,
 * else `config_add_account`; then the password step (the password dialog
 * for the SMTP password, which IMAP falls back to) or the sign-in step
 * (the device-code dialog). Answers the daemon's refusal for the wizard to
 * show, or null once the account is written and the wizard handed over.
 * The hand-over opens an overlay, which closes whatever else is open, so
 * it runs only while `shown` says the wizard is still in front of the user;
 * after a wizard dismissed during the write, only the notice says what is next.
 */
export async function writeAccount(
  dispatch: Dispatch<Action>,
  form: WizardForm,
  init: boolean,
  shown: () => boolean = () => true,
): Promise<string | null> {
  const draft = accountDraft(form);
  try {
    if (init) await cmd.configInit(draft);
    else await cmd.configAddAccount(draft);
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
  dispatch({ type: "notice", text: writtenLine(draft.name, form.preset, init) });
  if (!shown()) return null;
  if (signsIn(form.preset)) void startSignIn(dispatch, draft.name);
  else dispatch({ type: "open_password", account: draft.name, kind: "smtp" });
  return null;
}
