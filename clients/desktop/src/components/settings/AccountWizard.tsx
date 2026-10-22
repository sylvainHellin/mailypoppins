// The account wizard (ACC-01, ACC-02): provider, identity, servers,
// connection, mailboxes and review, the questions the CLI wizard asks
// (src/config_cmd/init.rs). The Connection step tests the candidate through
// `config.probe` before anything is written: a password preset with the
// passwords typed there, a Microsoft 365 preset once the candidate signed in
// with a device code (`config_oauth2_login` with the draft), whose token the
// daemon keeps aside until the write promotes it. The Mailboxes step offers
// the server's list with the CLI's defaults, and a typed name besides for
// a server that lists fewer than it has roles. The review writes through
// `config_add_account` (or `config_init` on a daemon with no config.toml),
// then stores the passwords. The form and the passwords are the component's
// own state, never the store's; the same form runs in the `account_wizard`
// dialog and inline on the first-run screen.

import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DeviceCodeView } from "@/components/settings/DeviceCodeDialog";
import { accountNames, type AccountWizard as AccountWizardModel } from "@/app/state";
import { useAppState, useDispatch } from "@/app/store";
import { deviceCodeOf, startSignIn } from "@/app/signin";
import {
  DEFAULT_TENANT,
  MAILYPOPPINS_CLIENT_ID,
  NO_SECRETS,
  PASSWORD_EMPTY,
  PRESET_ORDER,
  PRESETS,
  STEP_TITLES,
  STEPS,
  accountDraft,
  candidateDraft,
  checkLine,
  connectionKey,
  hasDefaultApp,
  presetForm,
  runConnectionTest,
  signsIn,
  stepErrors,
  stepOf,
  usesServers,
  withListing,
  withPreset,
  writeAccount,
  type CheckRun,
  type ConnectionResult,
  type Preset,
  type Role,
  type WizardField,
  type WizardForm,
  type WizardSecrets,
  type WizardStep,
} from "@/app/wizard";

type SecretField = "smtp_password" | "imap_password";
type Errors = Partial<Record<WizardField | SecretField, string>>;

/** What the review says a test found, check by check. */
const CHECK_NAMES: Record<CheckRun["check"], string> = { smtp: "SMTP login", imap: "IMAP login", graph: "Graph", mailboxes: "mailbox list" };

/** Said when the review finds the last test is not for what it would write. */
const RETEST = "The servers changed since the last test: test the connection again.";

export type AccountWizardFormProps = {
  preset: Preset;
  /** Write the first config.toml (`config_init`) rather than append. */
  init: boolean;
  /** Every configured account name. */
  taken: readonly string[];
  /** Cancel: absent on the first-run screen, which has nowhere to go back to. */
  onCancel?: () => void;
  /** The account is written and its passwords stored: the dialog closes. */
  onDone?: () => void;
  /** Where the dialog puts its initial focus. */
  firstRef?: RefObject<HTMLInputElement | null>;
};

/** One labelled field, its error announced and tied to it. */
function Field({
  id,
  label,
  hint,
  error,
  children,
}: {
  id: string;
  label: string;
  hint?: string;
  error?: string;
  children: (describedBy: string | undefined) => ReactNode;
}) {
  const described = [hint ? `${id}-hint` : null, error ? `${id}-error` : null].filter(Boolean).join(" ") || undefined;
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={id} className="text-sm text-muted-foreground">
        {label}
      </label>
      {children(described)}
      {hint ? (
        <p id={`${id}-hint`} className="text-xs text-muted-foreground">
          {hint}
        </p>
      ) : null}
      {error ? (
        <p id={`${id}-error`} data-slot="wizard-field-error" className="text-xs text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );
}

const SELECT_CLASS =
  "h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

/** A role picker's last option: a name the server did not list, typed in the field below it. */
const TYPE_NAME = "\u0000type";

/** The test's lines: a check mark, a cross for a failure that stops the wizard, a warning sign for one that does not. */
function ConnectionLines({ result, sameImap, token }: { result: ConnectionResult; sameImap: boolean; token: boolean }) {
  return (
    <ul aria-label="Connection test" data-slot="connection-results" className="flex flex-col gap-1 text-sm">
      {result.runs.map((r) => (
        <li
          key={r.check}
          data-check={r.check}
          data-ok={r.ok}
          className={r.ok ? "flex gap-2" : r.required ? "flex gap-2 text-destructive" : "flex gap-2 text-muted-foreground"}
        >
          <span aria-hidden="true">{r.ok ? "\u2713" : r.required ? "\u2717" : "\u26a0"}</span>
          <span className="min-w-0 break-words">{checkLine(r, sameImap, token)}</span>
        </li>
      ))}
    </ul>
  );
}

/**
 * Next checks the step and moves on, Back returns, Enter in a field is
 * Next. On the Connection step the same button signs in and tests, and
 * becomes Next once the test passed; the review writes. A refusal of the
 * daemon (a taken name, a block that does not load) shows in the alert and
 * the wizard stays open.
 */
export function AccountWizardForm({ preset, init, taken, onCancel, onDone, firstRef }: AccountWizardFormProps) {
  const s = useAppState();
  const dispatch = useDispatch();
  const id = useId();
  const [form, setForm] = useState<WizardForm>(() => presetForm(preset));
  const [secrets, setSecrets] = useState<WizardSecrets>(NO_SECRETS);
  const [step, setStep] = useState<WizardStep>("provider");
  const [errors, setErrors] = useState<Errors>({});
  const [refusal, setRefusal] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The last connection test, and the key of the candidate the last
  // sign-in staged a token for; both stale once the key moves.
  const [result, setResult] = useState<ConnectionResult | null>(null);
  const [signedIn, setSignedIn] = useState<string | null>(null);
  // The server listing the mailbox defaults were last taken from, so a
  // test that lists the same again keeps the user's picks.
  const [applied, setApplied] = useState<string | null>(null);
  // The roles the user chose to type a name for rather than pick one.
  const [typing, setTyping] = useState<Partial<Record<Role, boolean>>>({});
  const bodyRef = useRef<HTMLDivElement>(null);
  const moved = useRef(false);
  // The dialog's form unmounts once the wizard closes, so a write that
  // settles after hands over to nothing; the first-run screen's cannot be
  // dismissed, and goes as the write succeeds, so it always hands over.
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const key = connectionKey(form, secrets.sameImap);
  const fresh = result !== null && result.key === key ? result : null;
  const signing = signsIn(form.preset);
  const run = s.signIn?.candidate ? s.signIn : null;
  const signInRuns = run !== null && run.outcome === null;
  const code = signInRuns ? deviceCodeOf(s) : null;
  // The key the running sign-in was started for.
  const pendingKey = useRef<string | null>(null);

  // A step's first field takes the focus once the user moved to it.
  useEffect(() => {
    if (!moved.current) return;
    bodyRef.current?.querySelector<HTMLElement>("input, textarea, select, button[data-review-fix]")?.focus();
  }, [step]);

  const test = async () => {
    setBusy(true);
    setRefusal(null);
    // A wizard cancelled mid-test stops it, so no probe after the cancel
    // carries the typed password.
    const outcome = await runConnectionTest(form, secrets, () => mounted.current);
    if (!mounted.current || "stopped" in outcome) return;
    setBusy(false);
    if ("refusal" in outcome) {
      setRefusal(outcome.refusal);
      return;
    }
    const listed = outcome.result.listed;
    if (listed !== null && JSON.stringify(listed) !== applied) {
      setForm((f) => withListing(f, listed));
      setApplied(JSON.stringify(listed));
      setTyping({});
    }
    setResult(outcome.result);
  };

  // The candidate's sign-in ended: stored, the test runs at once; failed,
  // the alert says why. Either way the model forgets it.
  useEffect(() => {
    if (!run || run.outcome === null || pendingKey.current === null) return;
    const at = pendingKey.current;
    pendingKey.current = null;
    dispatch({ type: "sign_in_closed" });
    if (run.outcome.kind === "stored") {
      setSignedIn(at);
      if (at === key) void test();
    } else {
      setRefusal(run.outcome.text);
    }
    // `test` and `key` are this render's, the one the end arrived in.
  }, [run, dispatch]);

  // A wizard closed while its sign-in runs cancels it; the device-code
  // dialog, always mounted, sends `operation_cancel` once the id is known.
  const runRef = useRef(run);
  runRef.current = run;
  useEffect(
    () => () => {
      const r = runRef.current;
      if (r && r.outcome === null) dispatch({ type: "sign_in_cancelling" });
    },
    [dispatch],
  );

  const set = (field: WizardField, value: string) => {
    setForm((f) => ({ ...f, [field]: value }));
    setErrors((e) => {
      if (!(field in e)) return e;
      const next = { ...e };
      delete next[field];
      return next;
    });
  };
  const setSecret = (patch: Partial<WizardSecrets>) => {
    setSecrets((x) => ({ ...x, ...patch }));
    setResult(null);
    setErrors((e) => {
      const next = { ...e };
      delete next.smtp_password;
      delete next.imap_password;
      return next;
    });
  };
  const at = STEPS.indexOf(step);
  const go = (to: WizardStep) => {
    moved.current = true;
    setRefusal(null);
    setStep(to);
  };

  const signIn = () => {
    pendingKey.current = key;
    setRefusal(null);
    setResult(null);
    void startSignIn(dispatch, form.name.trim(), candidateDraft(form));
  };

  const connect = async () => {
    // The name is checked again: an account of that name may have been
    // configured since, and the daemon refuses to probe a token candidate
    // named like one.
    const identity = stepErrors("identity", form, taken);
    if (Object.keys(identity).length > 0) {
      setErrors(identity);
      go("identity");
      return;
    }
    if (signing) {
      if (signedIn !== key) signIn();
      else if (!fresh) await test();
      else go("mailboxes");
      return;
    }
    const missing: Errors = {};
    if (secrets.smtp === "") missing.smtp_password = PASSWORD_EMPTY;
    if (!secrets.sameImap && secrets.imap === "") missing.imap_password = PASSWORD_EMPTY;
    setErrors(missing);
    if (Object.keys(missing).length > 0) {
      bodyRef.current?.querySelector<HTMLElement>("[aria-invalid='true']")?.focus();
      return;
    }
    if (fresh?.passed) go("mailboxes");
    else await test();
  };

  const next = async () => {
    if (busy || signInRuns) return;
    if (step === "connection") {
      await connect();
      return;
    }
    const found = stepErrors(step, form, taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      if (step === "review") {
        const first = Object.keys(found)[0] as WizardField;
        go(stepOf(first));
      } else {
        bodyRef.current?.querySelector<HTMLElement>("[aria-invalid='true']")?.focus();
      }
      return;
    }
    if (step !== "review") {
      go(STEPS[at + 1]);
      return;
    }
    if (!fresh?.passed || (signing && signedIn !== key)) {
      go("connection");
      setRefusal(RETEST);
      return;
    }
    setBusy(true);
    const refused = await writeAccount(
      dispatch,
      form,
      init,
      secrets,
      () => !onCancel || mounted.current,
      () => onDone?.(),
    );
    if (!mounted.current) return;
    setBusy(false);
    setRefusal(refused);
  };

  const text = (field: WizardField, label: string, opts: { hint?: string; port?: boolean; placeholder?: string } = {}) => {
    const fid = `${id}-${field}`;
    return (
      <Field id={fid} label={label} hint={opts.hint} error={errors[field]}>
        {(describedBy) => (
          <Input
            id={fid}
            data-field={field}
            type="text"
            inputMode={opts.port ? "numeric" : undefined}
            value={form[field]}
            placeholder={opts.placeholder}
            autoComplete="off"
            spellCheck={false}
            aria-invalid={errors[field] ? true : undefined}
            aria-describedby={describedBy}
            onChange={(e) => set(field, e.currentTarget.value)}
          />
        )}
      </Field>
    );
  };

  const secret = (field: SecretField, label: string, kind: "smtp" | "imap") => {
    const fid = `${id}-${field}`;
    return (
      <Field id={fid} label={label} error={errors[field]}>
        {(describedBy) => (
          <Input
            id={fid}
            data-field={field}
            type="password"
            value={secrets[kind]}
            autoComplete="off"
            spellCheck={false}
            disabled={busy}
            aria-invalid={errors[field] ? true : undefined}
            aria-describedby={describedBy}
            onChange={(e) => setSecret({ [kind]: e.currentTarget.value })}
          />
        )}
      </Field>
    );
  };

  /**
   * A role picked from the server's list, or typed: "Another name" opens a
   * field for a name the server did not list, which a standard role whose
   * list ran out (a server listing only INBOX) starts with. A marked role
   * may stay with the folder the server marks.
   */
  const pick = (role: Role, label: string, listed: readonly string[], marked: boolean) => {
    const fid = `${id}-${role}`;
    const value = form[role];
    const typed = typing[role] === true || (value !== "" && !listed.includes(value)) || (!marked && value === "");
    return (
      <Field id={fid} label={label} hint={typed && marked ? "Empty: the folder the server marks." : undefined} error={errors[role]}>
        {(describedBy) => (
          <>
            <select
              id={fid}
              data-field={role}
              value={typed ? TYPE_NAME : value}
              aria-invalid={!typed && errors[role] ? true : undefined}
              aria-describedby={typed ? undefined : describedBy}
              onChange={(e) => {
                const v = e.currentTarget.value;
                setTyping((t) => ({ ...t, [role]: v === TYPE_NAME }));
                set(role, v === TYPE_NAME ? "" : v);
              }}
              className={SELECT_CLASS}
            >
              {marked ? <option value="">The one the server marks</option> : null}
              {listed.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
              <option value={TYPE_NAME}>Another name…</option>
            </select>
            {typed ? (
              <Input
                data-field={`${role}_name`}
                aria-label={`${label} name`}
                type="text"
                value={value}
                autoComplete="off"
                spellCheck={false}
                aria-invalid={errors[role] ? true : undefined}
                aria-describedby={describedBy}
                onChange={(e) => set(role, e.currentTarget.value)}
              />
            ) : null}
          </>
        )}
      </Field>
    );
  };

  const p = PRESETS[form.preset];
  const draft = step === "review" ? accountDraft(form) : null;

  let body: ReactNode;
  if (step === "provider") {
    body = (
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-1 text-sm text-muted-foreground">Which provider hosts this account?</legend>
        {PRESET_ORDER.map((key, i) => (
          <label key={key} className="flex cursor-pointer items-start gap-2 rounded-lg border border-border px-3 py-2 has-checked:border-ring">
            <input
              ref={i === 0 ? firstRef : undefined}
              type="radio"
              name={`${id}-preset`}
              value={key}
              checked={form.preset === key}
              aria-describedby={`${id}-preset-${key}`}
              onChange={() => setForm((f) => withPreset(f, key))}
              className="mt-1"
            />
            <span className="flex flex-col">
              <span className="text-sm font-medium">{PRESETS[key].label}</span>
              <span id={`${id}-preset-${key}`} className="text-xs text-muted-foreground">
                {PRESETS[key].hint}
              </span>
            </span>
          </label>
        ))}
      </fieldset>
    );
  } else if (step === "identity") {
    body = (
      <>
        {text("name", "Account name", { hint: "A short unique slug, e.g. work or gmail; it names the account in mp:// links." })}
        {text("default_from", form.preset === "graph" ? "Email address" : "From address", {
          hint: form.preset === "graph" ? "The mailbox you sign in to; also the From address." : "Empty uses the SMTP username.",
          placeholder: "Name <you@example.com>",
        })}
      </>
    );
  } else if (step === "servers") {
    body = (
      <>
        {usesServers(form.preset) ? (
          <>
            <div className="grid grid-cols-[1fr_6rem] gap-2">
              {text("smtp_host", "SMTP host", { placeholder: "smtp.example.com" })}
              {text("smtp_port", "SMTP port", { port: true })}
            </div>
            {text("smtp_username", "SMTP username", { placeholder: "you@example.com" })}
            <div className="grid grid-cols-[1fr_6rem] gap-2">
              {text("imap_host", "IMAP host", { hint: "Empty uses the SMTP host." })}
              {text("imap_port", "IMAP port", { port: true })}
            </div>
            {text("imap_username", "IMAP username", { hint: "Empty uses the SMTP username." })}
          </>
        ) : null}
        {signing && hasDefaultApp(form.preset) ? (
          <>
            {text("client_id", "Client ID", {
              hint: "Empty uses the shared mailypoppins app; enter a client ID only for your organisation's own app registration.",
              placeholder: "mailypoppins app",
            })}
            {text("tenant_id", "Tenant ID", {
              hint: `Empty uses ${DEFAULT_TENANT}, for any work, school or personal Microsoft account; or enter your directory (tenant) ID or domain.`,
              placeholder: DEFAULT_TENANT,
            })}
          </>
        ) : signing ? (
          <>
            {text("client_id", "Client ID", { hint: "The Azure Entra ID app registration's application (client) ID." })}
            {text("tenant_id", "Tenant ID", { hint: "Your directory (tenant) ID or domain." })}
          </>
        ) : null}
      </>
    );
  } else if (step === "connection") {
    body = (
      <>
        {signing ? (
          <p className="text-xs text-muted-foreground">
            Microsoft signs this account in with a device code, then the wizard tests it. The daemon keeps the token aside until the account is
            written; cancel the wizard and the account's configuration and secrets stay as they were.
          </p>
        ) : (
          <>
            <p className="text-xs text-muted-foreground">
              The wizard logs in with these passwords to test the servers. They go to the secrets backend once the account is written, never into
              config.toml; cancel the wizard and nothing is stored.
            </p>
            {secret("smtp_password", "SMTP password", "smtp")}
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" data-field="same_imap" checked={secrets.sameImap} disabled={busy} onChange={(e) => setSecret({ sameImap: e.currentTarget.checked })} />
              IMAP uses the SMTP password
            </label>
            {!secrets.sameImap ? secret("imap_password", "IMAP password", "imap") : null}
          </>
        )}
        {signInRuns && run ? (
          <div className="flex flex-col gap-2 rounded-lg border border-border p-3" data-slot="wizard-sign-in">
            {code ? (
              <DeviceCodeView code={code} cancelling={run.cancelling} />
            ) : (
              <p role="status" className="text-sm text-muted-foreground">
                {run.cancelling ? "Cancelling\u2026" : "Asking the provider for a device code\u2026"}
              </p>
            )}
            <div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={run.cancelling}
                onClick={() => {
                  pendingKey.current = null;
                  dispatch({ type: "sign_in_cancelling" });
                }}
              >
                Cancel sign-in
              </Button>
            </div>
          </div>
        ) : null}
        {signing && signedIn === key && !signInRuns ? (
          <p role="status" className="text-sm">
            Signed in. The token waits for the account to be written.
          </p>
        ) : null}
        {busy ? (
          <p role="status" className="text-sm text-muted-foreground">
            Testing the connection…
          </p>
        ) : null}
        {fresh && !busy ? <ConnectionLines result={fresh} sameImap={secrets.sameImap} token={signing} /> : null}
      </>
    );
  } else if (step === "mailboxes") {
    const oid = `${id}-overrides`;
    const listed = fresh?.listed ?? null;
    body = (
      <>
        {listed ? (
          <>
            <p className="text-xs text-muted-foreground">
              The server's mailboxes for the three standard roles; the preselected ones are those the CLI picks. Another name takes one the server did
              not list.
            </p>
            {pick("inbox", "Inbox", listed, false)}
            {pick("archive", "Archive", listed, false)}
            {pick("sent", "Sent", listed, false)}
            <p className="text-xs text-muted-foreground">
              Every other folder on the server is found and synced on its own. The server marks its drafts, trash and junk folders; pick one only if it
              does not.
            </p>
            {pick("drafts", "Drafts", listed, true)}
            {pick("trash", "Trash", listed, true)}
            {pick("junk", "Junk", listed, true)}
          </>
        ) : (
          <>
            <p className="text-xs text-muted-foreground">
              {fresh
                ? "The server's list could not be read, so type the server's names for the three standard mailboxes."
                : "The server's names for the three standard mailboxes; the defaults are the provider's usual ones."}
            </p>
            {text("inbox", "Inbox")}
            {text("archive", "Archive")}
            {text("sent", "Sent")}
            <p className="text-xs text-muted-foreground">
              Every other folder on the server is found and synced on its own. The server marks its drafts, trash and junk folders; name one only if it
              does not.
            </p>
            {text("drafts", "Drafts", { hint: "Empty: the folder the server marks." })}
            {text("trash", "Trash", { hint: "Empty: the folder the server marks." })}
            {text("junk", "Junk", { hint: "Empty: the folder the server marks." })}
          </>
        )}
        <Field id={oid} label="Policy overrides" hint="One per line: a pattern, then sync, status-only or ignore, e.g. Labels/* ignore." error={errors.overrides}>
          {(describedBy) => (
            <Textarea
              id={oid}
              data-field="overrides"
              rows={2}
              value={form.overrides}
              spellCheck={false}
              aria-describedby={describedBy}
              aria-invalid={errors.overrides ? true : undefined}
              onChange={(e) => set("overrides", e.currentTarget.value)}
            />
          )}
        </Field>
      </>
    );
  } else {
    const rows: [string, string][] = [
      ["Provider", p.label],
      ["Name", draft?.name ?? ""],
      ["From", draft?.default_from ?? "not set"],
    ];
    if (draft?.smtp) rows.push(["SMTP", `${draft.smtp.host}:${draft.smtp.port} as ${draft.smtp.username}`]);
    if (draft?.imap) {
      rows.push(["IMAP", `${draft.imap.host ?? draft.smtp?.host}:${draft.imap.port} as ${draft.imap.username ?? draft.smtp?.username}`]);
    }
    if (draft?.oauth2) {
      const client = draft.oauth2.client_id === MAILYPOPPINS_CLIENT_ID ? "the mailypoppins app" : `client ${draft.oauth2.client_id}`;
      rows.push(["App registration", `${client}, tenant ${draft.oauth2.tenant_id}`]);
    }
    if (fresh) rows.push(["Connection", fresh.runs.map((r) => `${CHECK_NAMES[r.check]} ${r.ok ? "passed" : "failed"}`).join(", ")]);
    if (draft?.mailboxes) {
      const m = draft.mailboxes;
      rows.push(["Mailboxes", [m.inbox, m.archive, m.sent].join(", ")]);
      const marked = (["drafts", "trash", "junk"] as const).filter((role) => m[role]).map((role) => `${role} ${m[role]}`);
      if (marked.length > 0) rows.push(["Role folders", marked.join(", ")]);
      if (m.overrides?.length) rows.push(["Overrides", m.overrides.map((o) => `${o.match} ${o.policy}`).join(", ")]);
    }
    body = (
      <>
        <dl className="grid grid-cols-[8rem_1fr] gap-x-2 gap-y-1 text-sm">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd className="min-w-0 break-words">{v}</dd>
            </div>
          ))}
        </dl>
        <p className="text-xs text-muted-foreground">
          {init ? "This writes config.toml with this one account. " : "This appends the account to config.toml. "}
          {signing
            ? "The sign-in made on the Connection step becomes the account's as it is written."
            : "The password then goes to the secrets backend, never into config.toml."}
        </p>
      </>
    );
  }

  const primary =
    step === "review"
      ? init
        ? "Write config.toml"
        : "Add account"
      : step !== "connection"
        ? "Next"
        : signing && signedIn !== key
          ? "Sign in"
          : fresh?.passed
            ? "Next"
            : "Test connection";

  return (
    <form
      aria-label="Account wizard"
      data-slot="account-wizard"
      data-step={step}
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        void next();
      }}
    >
      <ol aria-label="Steps" className="flex flex-wrap gap-1 text-xs">
        {STEPS.map((s, i) => (
          <li
            key={s}
            aria-current={s === step ? "step" : undefined}
            className={
              s === step
                ? "rounded-md bg-accent px-2 py-0.5 font-medium text-accent-foreground"
                : i < at
                  ? "px-2 py-0.5 text-foreground"
                  : "px-2 py-0.5 text-muted-foreground"
            }
          >
            {`${i + 1}. ${STEP_TITLES[s]}`}
          </li>
        ))}
      </ol>
      <div ref={bodyRef} className="flex flex-col gap-2">
        <h3 className="text-sm font-semibold">{STEP_TITLES[step]}</h3>
        {body}
      </div>
      <p role="alert" data-slot="wizard-error" className="min-h-5 text-sm text-destructive">
        {refusal}
      </p>
      <div className="flex flex-wrap justify-end gap-2">
        {onCancel ? (
          <Button type="button" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
        {at > 0 ? (
          <Button type="button" variant="outline" onClick={() => go(STEPS[at - 1])} disabled={busy || signInRuns}>
            Back
          </Button>
        ) : null}
        <Button type="submit" disabled={busy || signInRuns} aria-busy={busy || undefined}>
          {primary}
        </Button>
      </div>
    </form>
  );
}

export type AccountWizardProps = { dialog: AccountWizardModel | null; onOpenChange: (open: boolean) => void };

/** The wizard as the `account_wizard` overlay: Settings' Add account and the palette row open it. */
export function AccountWizard({ dialog, onOpenChange }: AccountWizardProps) {
  const s = useAppState();
  const firstRef = useRef<HTMLInputElement>(null);
  const init = s.config.data?.state === "absent";
  const taken = s.config.data?.config.accounts.map((a) => a.name) ?? accountNames(s);
  return (
    <Dialog open={dialog !== null} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={firstRef} data-slot="account-wizard-dialog" className="max-h-[90svh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{init ? "Set up the first account" : "Add account"}</DialogTitle>
          <DialogDescription>Tests the account, then writes it to config.toml through the daemon; nothing is written before the last step.</DialogDescription>
        </DialogHeader>
        {dialog ? (
          <AccountWizardForm
            key={dialog.preset}
            preset={dialog.preset}
            init={init}
            taken={taken}
            onCancel={() => onOpenChange(false)}
            onDone={() => onOpenChange(false)}
            firstRef={firstRef}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
