// The account wizard (ACC-01, ACC-02; clients/desktop/docs/shell.md,
// "Account wizard"): the CLI wizard's three provider presets
// (src/config_cmd/init.rs), the form's steps, what each step checks before
// it lets the user on, the connection test and the mailbox defaults it runs
// through `config.probe` as the CLI does, and the `AccountDraft` the review
// writes. The dialog and the first-run screen hold the form, and the
// passwords the test used, in their own state, never in the store; the
// passwords are stored once the account is written, as the CLI does.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import * as cmd from "@/lib/commands";
import {
  asGuiError,
  type AccountDraft,
  type AccountDraftMailboxes,
  type AccountDraftOverride,
  type AuthMethod,
  type ProbeCheck,
  type SecretKind,
} from "@/lib/gui-types";

export type Preset = "imap" | "proton" | "graph";

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
    hint: "Any provider with IMAP and SMTP and a password, on-premises Exchange included.",
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
  graph: {
    label: "Microsoft 365 (Graph API)",
    hint: "Signs in through the shared mailypoppins app with a device code; no IMAP or SMTP server, no app registration of your own.",
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

export const PRESET_ORDER: Preset[] = ["imap", "proton", "graph"];

/**
 * The shared mailypoppins app registration a Graph account signs in through
 * when the client ID is left empty; mirrors `MAILYPOPPINS_CLIENT_ID` in
 * crates/mp-core/src/oauth2.rs.
 */
export const MAILYPOPPINS_CLIENT_ID = "439d7ebd-7898-4df2-b2d6-88bb61e2597b";

/**
 * The tenant a Graph account signs in through when it is left empty: any
 * work, school or personal Microsoft account (`DEFAULT_TENANT` in
 * crates/mp-core/src/oauth2.rs).
 */
export const DEFAULT_TENANT = "common";

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
  /** The drafts, trash and junk folders, empty for the one the server marks (ticket 0141). */
  drafts: string;
  trash: string;
  junk: string;
  /** Policy overrides, one `<pattern> <policy>` per line, the first match winning. */
  overrides: string;
};

export type WizardField = Exclude<keyof WizardForm, "preset">;

export type WizardStep = "provider" | "identity" | "servers" | "connection" | "mailboxes" | "review";

export const STEPS: WizardStep[] = ["provider", "identity", "servers", "connection", "mailboxes", "review"];

export const STEP_TITLES: Record<WizardStep, string> = {
  provider: "Provider",
  identity: "Identity",
  servers: "Servers",
  connection: "Connection",
  mailboxes: "Mailboxes",
  review: "Review",
};

/**
 * The passwords of a password preset, typed on the Connection step: the
 * SMTP one, and the IMAP one unless IMAP uses the SMTP one, as the CLI's
 * "Use same password as SMTP?" defaults to. Held by the wizard component
 * only, tested through `config.probe` and stored with `config.set_password`
 * once the account is written.
 */
export type WizardSecrets = { smtp: string; imap: string; sameImap: boolean };

export const NO_SECRETS: WizardSecrets = { smtp: "", imap: "", sameImap: true };

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
    drafts: "",
    trash: "",
    junk: "",
    overrides: "",
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

/**
 * Whether the preset signs in with a device code rather than a password:
 * Graph only, whose empty client and tenant IDs are the mailypoppins app and
 * `common`.
 */
export function signsIn(preset: Preset): boolean {
  return PRESETS[preset].auth === "graph";
}

/** An account name: it names the account in `mp://` links and its directory on disk. */
const SLUG = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

export const NAME_EMPTY = "Give the account a name";
export const NAME_SHAPE = "Use letters, digits, '.', '-' and '_' only, starting with a letter or a digit";
export const nameTaken = (name: string): string => `An account named ${name} is already configured`;
export const HOST_EMPTY = "The SMTP host is required";
export const USERNAME_EMPTY = "The SMTP username is required";
export const FROM_EMPTY = "The email address is required";
export const MAILBOX_EMPTY = "A server mailbox name is required";
export const PASSWORD_EMPTY = "Type the password to test the login";
export const mailboxTwice = (name: string, role: string): string => `${name} is already the ${role} mailbox`;
export const PORT_BAD = "A port is a whole number from 1 to 65535";
export const overrideBad = (line: string): string =>
  `"${line}" is not an override: a pattern, then sync, status-only or ignore`;

/** The policies an override may give, as config.toml spells them. */
export const POLICIES = ["sync", "status-only", "ignore"] as const;

/**
 * The overrides typed one per line: a glob over the server name, then the
 * policy as the last word, so a pattern may hold spaces (`Sent Items/Old`).
 * Blank lines are skipped; a line that is not an override is answered as such.
 */
export function parseOverrides(text: string): { overrides: AccountDraftOverride[]; bad: string | null } {
  const overrides: AccountDraftOverride[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line === "") continue;
    const cut = line.search(/\s+\S+$/);
    const policy = cut < 0 ? "" : line.slice(cut).trim();
    const pattern = cut < 0 ? "" : line.slice(0, cut).trim();
    if (pattern === "" || !(POLICIES as readonly string[]).includes(policy)) return { overrides, bad: line };
    overrides.push({ match: pattern, policy });
  }
  return { overrides, bad: null };
}

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
  }
  if (step === "mailboxes" || step === "review") {
    // One server mailbox plays one role, as the CLI's pickers offer only
    // what the roles before did not take.
    const seen = new Map<string, string>();
    for (const role of ROLES) {
      const name = form[role].trim();
      if (name === "") {
        if ((STANDARD_ROLES as readonly string[]).includes(role)) out[role] = MAILBOX_EMPTY;
        continue;
      }
      const first = seen.get(name);
      if (first !== undefined && (STANDARD_ROLES as readonly string[]).includes(first)) out[role] = mailboxTwice(name, first);
      else if (first === undefined) seen.set(name, role);
    }
    const { bad } = parseOverrides(form.overrides);
    if (bad !== null) out.overrides = overrideBad(bad);
  }
  return out;
}

/** The three roles every account maps, then the three a server usually marks. */
export const STANDARD_ROLES = ["inbox", "archive", "sent"] as const;
export const MARKED_ROLES = ["drafts", "trash", "junk"] as const;
export const ROLES = [...STANDARD_ROLES, ...MARKED_ROLES] as const;
export type Role = (typeof ROLES)[number];

/**
 * The names the CLI's mailbox pickers preselect, in its order of
 * preference (`Labels::imap` and `Labels::graph` in src/config_cmd/init.rs).
 */
const PREFERRED: Record<"imap" | "graph", Record<(typeof STANDARD_ROLES)[number], string[]>> = {
  imap: { inbox: ["INBOX"], archive: ["Archive", "archive", "All Mail"], sent: ["Sent", "Sent Items", "Sent Messages", "INBOX.Sent"] },
  graph: { inbox: ["Inbox"], archive: ["Archive"], sent: ["Sent Items"] },
};

/**
 * The mailbox defaults the CLI picks from the server's list: for the inbox,
 * the archive and the sent folder in turn, the first preferred name the
 * server lists (case-insensitively), else the first mailbox the roles before
 * did not take, else, when the server listed fewer mailboxes than there are
 * roles, the provider's usual name, typed rather than picked, as the CLI's
 * typed prompt offers it; drafts, trash and junk are left to the folder the
 * server marks. The overrides are kept.
 */
export function withListing(form: WizardForm, listed: readonly string[]): WizardForm {
  const preferred = PREFERRED[form.preset === "graph" ? "graph" : "imap"];
  let available = [...listed];
  const picked: Partial<WizardForm> = { drafts: "", trash: "", junk: "" };
  for (const role of STANDARD_ROLES) {
    const match = preferred[role].map((p) => available.find((o) => o.toLowerCase() === p.toLowerCase())).find((o) => o !== undefined);
    const choice = match ?? available[0] ?? PRESETS[form.preset][role];
    picked[role] = choice;
    available = available.filter((o) => o !== choice);
  }
  return { ...form, ...picked };
}

/**
 * The block a connection test and a sign-in describe: the draft without its
 * mailbox mapping, which the CLI's probes do not carry either, since the
 * mailboxes are chosen from what the test lists.
 */
export function candidateDraft(form: WizardForm): AccountDraft {
  const draft = accountDraft(form);
  delete draft.mailboxes;
  return draft;
}

/**
 * What a connection test's result is for: the candidate and which password
 * IMAP uses. A test of another key is stale, and so is a sign-in; a changed
 * password resets the result where it is typed.
 */
export function connectionKey(form: WizardForm, sameImap: boolean): string {
  return JSON.stringify([candidateDraft(form), sameImap]);
}

/** One probe of the test: its check, the password it sends, and whether its failure stops the wizard. */
export type PlannedCheck = { check: ProbeCheck; password: SecretKind | null; required: boolean };

/**
 * The CLI wizard's probes, in its order. A password preset tests the SMTP
 * login, which must pass; then the IMAP login, which must pass with a
 * password of its own and is a warning with the SMTP one; then lists the
 * mailboxes with the IMAP password. A Graph preset, once signed in, tests
 * the Graph client, then lists the folders. Only a password login stops the
 * wizard: the CLI asks again for those, and goes on past a failed Graph test
 * or mailbox list, the names then typed.
 */
export function probePlan(preset: Preset, sameImap: boolean): PlannedCheck[] {
  const auth = PRESETS[preset].auth;
  if (auth === "graph") {
    return [
      { check: "graph", password: null, required: false },
      { check: "mailboxes", password: null, required: false },
    ];
  }
  const imap: SecretKind = sameImap ? "smtp" : "imap";
  return [
    { check: "smtp", password: "smtp", required: true },
    { check: "imap", password: imap, required: !sameImap },
    { check: "mailboxes", password: imap, required: false },
  ];
}

/** One probe the test ran, and what the daemon answered. */
export type CheckRun = { check: ProbeCheck; required: boolean; ok: boolean; error: string | null; mailboxes: string[] };

/**
 * A finished connection test: the key it is for, every probe it ran, the
 * server's mailbox list (null when it could not be read), and whether the
 * wizard may go on.
 */
export type ConnectionResult = { key: string; runs: CheckRun[]; listed: string[] | null; passed: boolean };

/** The daemon's refusal of a candidate named like a configured account (`config.probe`'s security fix), in plain words. */
export function probeRefusalLine(name: string, message: string): string {
  if (/is a configured account/.test(message)) return `${nameTaken(name)}. Go back to Identity and choose another name.`;
  return message;
}

/**
 * Run the test: each planned probe in turn through `config.probe`, until a
 * login that must pass does not. Answers the result, or the daemon's refusal
 * of the call in plain words. `live` is asked before each probe: once the
 * wizard is gone, the test stops, and the typed passwords go to no server
 * after that.
 */
export async function runConnectionTest(
  form: WizardForm,
  secrets: WizardSecrets,
  live: () => boolean = () => true,
): Promise<{ result: ConnectionResult } | { refusal: string } | { stopped: true }> {
  const candidate = candidateDraft(form);
  const runs: CheckRun[] = [];
  let listed: string[] | null = null;
  for (const planned of probePlan(form.preset, secrets.sameImap)) {
    if (!live()) return { stopped: true };
    const password = planned.password === null ? undefined : secrets[planned.password];
    let answer;
    try {
      answer = await cmd.configProbe(candidate, planned.check, password);
    } catch (e: unknown) {
      return { refusal: probeRefusalLine(candidate.name, asGuiError(e).message) };
    }
    runs.push({ check: planned.check, required: planned.required, ok: answer.ok, error: answer.error ?? null, mailboxes: answer.mailboxes ?? [] });
    if (planned.check === "mailboxes" && answer.ok && answer.mailboxes.length > 0) listed = [...answer.mailboxes];
    if (planned.required && !answer.ok) break;
  }
  const passed = runs.every((r) => r.ok || !r.required);
  return { result: { key: connectionKey(form, secrets.sameImap), runs, listed, passed } };
}

/** A server's reason as a sentence: a period added only when it ends in none. */
export function asSentence(text: string): string {
  const t = text.trim();
  return /[.!?]$/.test(t) ? t : `${t}.`;
}

/** What one probe found, in plain words. */
export function checkLine(run: CheckRun, sameImap: boolean): string {
  const why = asSentence(run.error ?? "no reason given");
  switch (run.check) {
    case "smtp":
      return run.ok ? "SMTP: the server accepted the login." : `SMTP: the login failed. ${why}`;
    case "imap":
      if (run.ok) return "IMAP: the server accepted the login.";
      return sameImap
        ? `IMAP: the SMTP password did not work for IMAP. ${why} Untick "IMAP uses the SMTP password" to give IMAP its own, or set it in Settings later.`
        : `IMAP: the login failed. ${why}`;
    case "graph":
      return run.ok ? "Graph: the sign-in reaches the mailbox." : `Graph: the test failed. ${why} You can sign in again from Settings later.`;
    case "mailboxes": {
      if (!run.ok) return `Mailboxes: the server's list could not be read. ${why} Type the names on the next step.`;
      const n = run.mailboxes.length;
      return n === 0 ? "Mailboxes: the server listed none; type the names on the next step." : `Mailboxes: the server listed ${n}.`;
    }
  }
}

/** The step a field is entered on, for the review's "fix it" jump. */
export function stepOf(field: WizardField): WizardStep {
  if (field === "name" || field === "default_from") return "identity";
  const mailboxes: WizardField[] = ["inbox", "archive", "sent", "drafts", "trash", "junk", "overrides"];
  if (mailboxes.includes(field)) return "mailboxes";
  return "servers";
}

/**
 * The `AccountDraft` the form describes, with exactly the keys the
 * daemon's `account_block` reads: a password account leaves `auth_method`
 * to the daemon's default, an empty IMAP host or username falls back to the
 * SMTP one there, and a Graph account has no server at all. The From
 * address defaults to the SMTP username, as the CLI's prompt does; a Graph
 * account's empty client and tenant IDs become the mailypoppins app and
 * `common`, as the CLI's prompts do.
 */
export function accountDraft(form: WizardForm): AccountDraft {
  const t = (v: string) => v.trim();
  const p = PRESETS[form.preset];
  const from = t(form.default_from) || t(form.smtp_username);
  const draft: AccountDraft = { name: t(form.name) };
  if (from) draft.default_from = from;
  if (p.auth !== "password") {
    draft.auth_method = p.auth;
    const fallback = (v: string, d: string) => (t(v) === "" ? d : t(v));
    draft.oauth2 = {
      client_id: fallback(form.client_id, MAILYPOPPINS_CLIENT_ID),
      tenant_id: fallback(form.tenant_id, DEFAULT_TENANT),
    };
  }
  if (usesServers(form.preset)) {
    const certs = p.invalidCerts ? { accept_invalid_certs: true } : {};
    draft.smtp = { host: t(form.smtp_host), port: Number(t(form.smtp_port)), username: t(form.smtp_username), ...certs };
    draft.imap = { port: Number(t(form.imap_port)), ...certs };
    if (t(form.imap_host)) draft.imap.host = t(form.imap_host);
    if (t(form.imap_username)) draft.imap.username = t(form.imap_username);
  }
  const mailboxes: AccountDraftMailboxes = { inbox: t(form.inbox), archive: t(form.archive), sent: t(form.sent) };
  for (const role of ["drafts", "trash", "junk"] as const) if (t(form[role])) mailboxes[role] = t(form[role]);
  const { overrides } = parseOverrides(form.overrides);
  if (overrides.length > 0) mailboxes.overrides = overrides;
  draft.mailboxes = mailboxes;
  return draft;
}

/** The passwords a password preset stores once written: the SMTP one, and the IMAP one when it has its own. */
export function passwordKinds(preset: Preset, sameImap: boolean): SecretKind[] {
  if (signsIn(preset)) return [];
  return sameImap ? ["smtp"] : ["smtp", "imap"];
}

/** The notice a written account leaves. */
export function writtenLine(name: string, preset: Preset, init: boolean, sameImap: boolean): string {
  const where = init ? "Wrote config.toml with the account" : "Added the account";
  const what = signsIn(preset) ? "its sign-in is stored" : sameImap ? "its password is stored" : "its SMTP and IMAP passwords are stored";
  return `${where} ${name}; ${what}`;
}

/**
 * The account was written and some of its passwords were not stored: the
 * CLI's `missing_passwords_message`, with Settings for the command.
 */
export function missingPasswordsLine(name: string, missing: readonly SecretKind[]): string {
  const kinds = missing.map((k) => k.toUpperCase()).join(" and ");
  const [noun, verb, pronoun] = missing.length === 1 ? ["password", "is", "it"] : ["passwords", "are", "them"];
  return `Account ${name} is configured, but its ${kinds} ${noun} ${verb} not stored, so it cannot log in yet. Set ${pronoun} in Settings.`;
}

/**
 * The review's write: `config_init` on a daemon with no configuration,
 * else `config_add_account`, which also promotes the sign-in the
 * Connection step staged; then each password the test used, through
 * `config_set_password`, now that the account exists. Answers the daemon's
 * refusal of the write for the wizard to show, or null once the account is
 * written. A password the daemon did not store leaves the account written:
 * the notice says which, and the password dialog opens for the first. That
 * dialog, like `done` (which closes the wizard), changes the overlay, so
 * both run only while `shown` says the wizard is still in front of the user.
 */
export async function writeAccount(
  dispatch: Dispatch<Action>,
  form: WizardForm,
  init: boolean,
  secrets: WizardSecrets,
  shown: () => boolean = () => true,
  done: () => void = () => {},
): Promise<string | null> {
  const draft = accountDraft(form);
  try {
    if (init) await cmd.configInit(draft);
    else await cmd.configAddAccount(draft);
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
  const missing: SecretKind[] = [];
  for (const kind of passwordKinds(form.preset, secrets.sameImap)) {
    try {
      await cmd.configSetPassword(draft.name, kind, secrets[kind]);
    } catch {
      missing.push(kind);
    }
  }
  if (missing.length > 0) {
    dispatch({ type: "notice", text: missingPasswordsLine(draft.name, missing), level: "error" });
    if (shown()) dispatch({ type: "open_password", account: draft.name, kind: missing[0] });
    return null;
  }
  dispatch({ type: "notice", text: writtenLine(draft.name, form.preset, init, secrets.sameImap) });
  if (shown()) done();
  return null;
}
