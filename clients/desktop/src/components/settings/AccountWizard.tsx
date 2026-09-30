// The account wizard (ACC-01, ACC-02): provider, identity, servers,
// mailboxes, review, then a write through `config_add_account` (or
// `config_init` on a daemon with no config.toml) and the hand-over to the
// password dialog or the device-code sign-in. The form is the component's
// own state; it never holds a password. The same form runs in the
// `account_wizard` dialog and inline on the first-run screen.

import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { accountNames, type AccountWizard as AccountWizardModel } from "@/app/state";
import { useAppState, useDispatch } from "@/app/store";
import {
  PRESET_ORDER,
  PRESETS,
  STEP_TITLES,
  STEPS,
  accountDraft,
  presetForm,
  signsIn,
  stepErrors,
  stepOf,
  usesServers,
  withPreset,
  writeAccount,
  type Preset,
  type WizardField,
  type WizardForm,
  type WizardStep,
} from "@/app/wizard";

type Errors = Partial<Record<WizardField, string>>;

export type AccountWizardFormProps = {
  preset: Preset;
  /** Write the first config.toml (`config_init`) rather than append. */
  init: boolean;
  /** Every configured account name. */
  taken: readonly string[];
  /** Cancel: absent on the first-run screen, which has nowhere to go back to. */
  onCancel?: () => void;
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

/**
 * Next checks the step and moves on, Back returns, Enter in a field is
 * Next; the review writes. A refusal of the daemon (a taken name, a block
 * that does not load) shows in the alert and the wizard stays open.
 */
export function AccountWizardForm({ preset, init, taken, onCancel, firstRef }: AccountWizardFormProps) {
  const dispatch = useDispatch();
  const id = useId();
  const [form, setForm] = useState<WizardForm>(() => presetForm(preset));
  const [step, setStep] = useState<WizardStep>("provider");
  const [errors, setErrors] = useState<Errors>({});
  const [refusal, setRefusal] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
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

  // A step's first field takes the focus once the user moved to it.
  useEffect(() => {
    if (!moved.current) return;
    bodyRef.current?.querySelector<HTMLElement>("input, textarea, button[data-review-fix]")?.focus();
  }, [step]);

  const set = (field: WizardField, value: string) => {
    setForm((f) => ({ ...f, [field]: value }));
    setErrors((e) => {
      if (!(field in e)) return e;
      const next = { ...e };
      delete next[field];
      return next;
    });
  };
  const at = STEPS.indexOf(step);
  const go = (to: WizardStep) => {
    moved.current = true;
    setRefusal(null);
    setStep(to);
  };

  const next = async () => {
    if (busy) return;
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
    setBusy(true);
    const refused = await writeAccount(dispatch, form, init, () => !onCancel || mounted.current);
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
        {signsIn(form.preset) ? (
          <>
            {text("client_id", "Client ID", { hint: "The Azure Entra ID app registration's application (client) ID." })}
            {text("tenant_id", "Tenant ID", { hint: "Your directory (tenant) ID or domain." })}
          </>
        ) : null}
      </>
    );
  } else if (step === "mailboxes") {
    const eid = `${id}-extra`;
    body = (
      <>
        <p className="text-xs text-muted-foreground">The server's names for the three standard mailboxes; the defaults are the provider's usual ones.</p>
        {text("inbox", "Inbox")}
        {text("archive", "Archive")}
        {text("sent", "Sent")}
        <Field id={eid} label="Extra mailboxes to sync" hint="One server name per line; empty syncs only the three above.">
          {(describedBy) => (
            <Textarea
              id={eid}
              data-field="extra"
              rows={2}
              value={form.extra}
              spellCheck={false}
              aria-describedby={describedBy}
              onChange={(e) => set("extra", e.currentTarget.value)}
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
    if (draft?.oauth2) rows.push(["App registration", `client ${draft.oauth2.client_id}, tenant ${draft.oauth2.tenant_id}`]);
    if (draft?.mailboxes) {
      const m = draft.mailboxes;
      rows.push(["Mailboxes", [m.inbox, m.archive, m.sent, ...(m.extra ?? [])].join(", ")]);
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
          {signsIn(form.preset)
            ? "The sign-in with a device code follows."
            : "The password follows; it goes to the secrets backend, never into config.toml."}{" "}
          No connection is tested first: a wrong host shows as the first sync's failure.
        </p>
      </>
    );
  }

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
          <Button type="button" variant="outline" onClick={() => go(STEPS[at - 1])} disabled={busy}>
            Back
          </Button>
        ) : null}
        <Button type="submit" disabled={busy} aria-busy={busy || undefined}>
          {step !== "review" ? "Next" : init ? "Write config.toml" : "Add account"}
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
          <DialogDescription>Writes the account to config.toml through the daemon; the password or the sign-in comes after.</DialogDescription>
        </DialogHeader>
        {dialog ? (
          <AccountWizardForm key={dialog.preset} preset={dialog.preset} init={init} taken={taken} onCancel={() => onOpenChange(false)} firstRef={firstRef} />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
