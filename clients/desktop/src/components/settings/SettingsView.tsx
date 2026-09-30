import { useEffect, useId, useState, type ReactNode } from "react";
import { ArrowLeft, KeyRound, LogIn, Plus, RotateCw, SquarePen } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { openConfig } from "@/app/interop";
import { kindLabel, LATER_IN_M4, reloadConfig, saveEditorSetting, usesPassword } from "@/app/settings";
import { useAppState, useDispatch } from "@/app/store";
import * as cmd from "@/lib/commands";
import type { ConfigAccount, ConfigServer, EditorSetting, SecretKind } from "@/lib/gui-types";

const AUTH_LABELS: Record<string, string> = {
  password: "Password",
  oauth2: "Microsoft 365 (OAuth2)",
  graph: "Microsoft 365 (Graph)",
};

const STATE_LABELS: Record<string, string> = {
  ok: "Loaded",
  absent: "No config.toml yet",
  invalid: "Did not load when the daemon started",
};

function server(s: ConfigServer): string {
  if (!s.host) return "not set";
  const at = s.port ? `${s.host}:${s.port}` : s.host;
  return s.username ? `${at} as ${s.username}` : at;
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[9rem_1fr] gap-2 py-0.5">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/** A control the account wizard brings (U7b): shown, disabled, with why. */
function Later({ icon, label, hint }: { icon: ReactNode; label: string; hint: string }) {
  const id = useId();
  return (
    <span className="flex flex-wrap items-center gap-2">
      <Button size="xs" variant="outline" disabled aria-describedby={id}>
        {icon}
        {label}
      </Button>
      <span id={id} className="text-xs text-muted-foreground">
        {hint}
      </span>
    </span>
  );
}

function AccountCard({ account, onPassword }: { account: ConfigAccount; onPassword: (kind: SecretKind) => void }) {
  const password = usesPassword(account);
  const headingId = useId();
  return (
    <article aria-labelledby={headingId} data-account={account.name} className="rounded-lg border border-border px-3 py-2">
      <h4 id={headingId} className="text-sm font-semibold">
        {account.name}
      </h4>
      <dl className="mt-1 text-sm">
        <Field label="Signs in with">{AUTH_LABELS[account.auth_method] ?? (account.auth_method || "Password")}</Field>
        <Field label="From">{account.default_from || "not set"}</Field>
        {account.auth_method === "graph" ? null : (
          <>
            <Field label="SMTP">{server(account.smtp)}</Field>
            <Field label="IMAP">{server(account.imap)}</Field>
          </>
        )}
        {account.oauth2 ? (
          <Field label="App registration">{`client ${account.oauth2.client_id}, tenant ${account.oauth2.tenant_id}`}</Field>
        ) : null}
      </dl>
      <div className="mt-2 flex flex-wrap gap-2">
        {password ? (
          (["smtp", "imap"] as const).map((kind) => (
            <Button key={kind} size="xs" variant="outline" onClick={() => onPassword(kind)}>
              <KeyRound aria-hidden="true" />
              {`Set ${kindLabel(kind)} password`}
            </Button>
          ))
        ) : (
          <Later icon={<LogIn aria-hidden="true" />} label="Sign in" hint={LATER_IN_M4} />
        )}
      </div>
    </article>
  );
}

/** The M3 editor setting (`editor_setting_get|set`): the template, what it resolves to now, and where it came from. */
function EditorSettingField() {
  const dispatch = useDispatch();
  const id = useId();
  const [setting, setSetting] = useState<EditorSetting | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    cmd
      .editorSettingGet()
      .then((got) => {
        if (!live) return;
        setSetting(got);
        setText(got.editor ?? "");
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  const save = async () => {
    const refused = await saveEditorSetting(dispatch, text);
    setError(refused);
    if (!refused) cmd.editorSettingGet().then(setSetting).catch(() => {});
  };
  return (
    <form
      aria-label="Editor"
      className="mt-3 flex flex-col gap-1"
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <label htmlFor={`${id}-editor`} className="text-sm text-muted-foreground">
        Editor command
      </label>
      <div className="flex gap-2">
        <Input
          id={`${id}-editor`}
          aria-describedby={`${id}-editor-hint`}
          value={text}
          placeholder={setting?.effective ?? ""}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => setText(e.currentTarget.value)}
        />
        <Button type="submit" size="sm" variant="outline">
          Save
        </Button>
      </div>
      <p id={`${id}-editor-hint`} className="text-xs text-muted-foreground">
        {setting?.env_override
          ? `MP_DESKTOP_EDITOR is set and wins: ${setting.env_override}`
          : `Opens drafts, config.toml and the log; {path} stands for the file. Empty uses ${setting?.effective ?? "the first editor found"}.`}
      </p>
      <p role="alert" className="min-h-4 text-xs text-destructive">
        {error}
      </p>
    </form>
  );
}

/**
 * The Settings view (clients/desktop/docs/shell.md, "Settings"): the
 * daemon's configuration as `config_get` reads it, read on every open and
 * after each `config.changed`. Settings change in config.toml, then Reload;
 * passwords are stored through the password dialog.
 */
export function SettingsView() {
  const s = useAppState();
  const dispatch = useDispatch();
  const l = s.config;
  const snapshot = l.data;
  const [reloading, setReloading] = useState(false);
  const reload = async () => {
    setReloading(true);
    await reloadConfig(dispatch);
    setReloading(false);
  };

  return (
    <section
      aria-label="Settings"
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-view="settings"
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 items-baseline justify-between gap-2 border-b border-border px-3 py-2">
        <h2 className="truncate text-sm font-semibold">Settings</h2>
        <Button size="xs" variant="ghost" onClick={() => dispatch({ type: "switch_view", view: "mail" })} title="Back to Mail (Esc)">
          <ArrowLeft aria-hidden="true" />
          Mail
        </Button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3 text-sm">
        {l.error && !snapshot ? (
          <p role="alert" className="text-destructive">
            The configuration did not load: {l.error.message}
          </p>
        ) : !snapshot ? (
          <div aria-busy="true" aria-label="Loading the configuration" className="flex flex-col gap-2">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-6 w-full" />
            ))}
          </div>
        ) : (
          <>
            <section aria-labelledby="settings-general" className="mb-5">
              <h3 id="settings-general" className="mb-1 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
                General
              </h3>
              <dl>
                <Field label="Config file">
                  <span className="font-mono text-xs">{snapshot.path}</span>
                </Field>
                <Field label="State">{STATE_LABELS[snapshot.state] ?? snapshot.state}</Field>
                <Field label="Secrets backend">{snapshot.config.secrets_backend || "not reported"}</Field>
                <Field label="Send hold">
                  {snapshot.config.email.send_hold_secs === 0 ? "none, a send leaves at once" : `${snapshot.config.email.send_hold_secs} seconds`}
                </Field>
              </dl>
              <div className="mt-2 flex flex-wrap gap-2">
                <Button size="xs" variant="outline" onClick={() => void openConfig(dispatch)} title="Open config.toml in $EDITOR (sc)">
                  <SquarePen aria-hidden="true" />
                  Open config.toml
                </Button>
                <Button size="xs" variant="outline" onClick={() => void reload()} disabled={reloading} aria-busy={reloading || undefined}>
                  <RotateCw aria-hidden="true" className={reloading ? "animate-spin" : undefined} />
                  Reload
                </Button>
              </div>
              <EditorSettingField />
            </section>
            <section aria-labelledby="settings-accounts">
              <h3 id="settings-accounts" className="mb-1 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
                Accounts
              </h3>
              {snapshot.config.accounts.length === 0 ? <p className="mb-2 text-muted-foreground">No account is configured.</p> : null}
              <div className="flex flex-col gap-2">
                {snapshot.config.accounts.map((account) => (
                  <AccountCard
                    key={account.name}
                    account={account}
                    onPassword={(kind) => dispatch({ type: "open_password", account: account.name, kind })}
                  />
                ))}
              </div>
              <div className="mt-3">
                <Later icon={<Plus aria-hidden="true" />} label="Add account" hint={LATER_IN_M4} />
              </div>
            </section>
          </>
        )}
      </div>
    </section>
  );
}
