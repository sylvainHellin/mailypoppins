import { useEffect, useId, useState, type ReactNode } from "react";
import { ArrowLeft, KeyRound, LogIn, Monitor, Moon, Plus, RotateCw, SquarePen, Sun } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { openConfig } from "@/app/interop";
import {
  EDITOR_COLORS,
  EDITOR_COLORS_LABELS,
  editorHint,
  kindLabel,
  loadEditorColors,
  reloadConfig,
  saveEditorColors,
  saveEditorSetting,
  usesPassword,
  type EditorColors,
} from "@/app/settings";
import { signInOrShow } from "@/app/signin";
import { saveTheme, THEME_LABELS, THEMES, type Theme } from "@/app/theme";
import { READER_MODE_LABELS, READER_MODES, saveReaderMode } from "@/app/readerMode";
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

function AccountCard({
  account,
  onPassword,
  onSignIn,
}: {
  account: ConfigAccount;
  onPassword: (kind: SecretKind) => void;
  onSignIn: () => void;
}) {
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
          <Button size="xs" variant="outline" onClick={onSignIn} title="Sign in with a device code">
            <LogIn aria-hidden="true" />
            Sign in
          </Button>
        )}
      </div>
    </article>
  );
}

const THEME_ICONS: Record<Theme, typeof Moon> = { dark: Moon, light: Sun, system: Monitor };

/** The theme (`setting_get|set` of `theme`): three buttons, the current one pressed; a press paints at once and stores. */
function ThemeField() {
  const { theme } = useAppState();
  const dispatch = useDispatch();
  const id = useId();
  return (
    <div className="mt-3 flex flex-col gap-1">
      <span id={`${id}-theme`} className="text-sm text-muted-foreground">
        Theme
      </span>
      <div role="group" aria-labelledby={`${id}-theme`} className="flex gap-1">
        {THEMES.map((t) => {
          const Icon = THEME_ICONS[t];
          return (
            <Button
              key={t}
              size="sm"
              variant="outline"
              aria-pressed={theme === t}
              className="aria-pressed:border-framing aria-pressed:bg-selection aria-pressed:text-selection-foreground"
              onClick={() => void saveTheme(dispatch, t)}
            >
              <Icon aria-hidden="true" />
              {THEME_LABELS[t]}
            </Button>
          );
        })}
      </div>
      <p className="text-xs text-muted-foreground">System follows the light or dark appearance of the operating system.</p>
    </div>
  );
}

/** The reader mode (`setting_get|set` of `reader_mode`): two buttons, the current one pressed; `tt` toggles it. */
function ReaderModeField() {
  const { readerMode } = useAppState();
  const dispatch = useDispatch();
  const id = useId();
  return (
    <div className="mt-3 flex flex-col gap-1">
      <span id={`${id}-reader`} className="text-sm text-muted-foreground">
        Reader
      </span>
      <div role="group" aria-labelledby={`${id}-reader`} className="flex gap-1">
        {READER_MODES.map((mode) => (
          <Button
            key={mode}
            size="sm"
            variant="outline"
            aria-pressed={readerMode === mode}
            className="aria-pressed:border-framing aria-pressed:bg-selection aria-pressed:text-selection-foreground"
            onClick={() => void saveReaderMode(dispatch, mode)}
          >
            {READER_MODE_LABELS[mode]}
          </Button>
        ))}
      </div>
      <p className="text-xs text-muted-foreground">Text shows the stored plain text in the app's theme; t t switches from the list or the reader.</p>
    </div>
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
        {editorHint(setting)}
      </p>
      <p role="alert" className="min-h-4 text-xs text-destructive">
        {error}
      </p>
    </form>
  );
}

/**
 * The editor's colours (`setting_get|set` of `editor_colors`): two buttons,
 * the current one pressed; the next embedded editor started takes the choice.
 */
function EditorColorsField() {
  const dispatch = useDispatch();
  const id = useId();
  const [colors, setColors] = useState<EditorColors | null>(null);
  useEffect(() => {
    let live = true;
    void loadEditorColors().then((got) => {
      if (live) setColors(got);
    });
    return () => {
      live = false;
    };
  }, []);
  const choose = async (choice: EditorColors) => {
    const before = colors;
    setColors(choice);
    const stored = await saveEditorColors(dispatch, choice);
    setColors(stored ?? before);
  };
  return (
    <div className="mt-3 flex flex-col gap-1">
      <span id={`${id}-colors`} className="text-sm text-muted-foreground">
        Editor colours
      </span>
      <div role="group" aria-labelledby={`${id}-colors`} className="flex gap-1">
        {EDITOR_COLORS.map((choice) => (
          <Button
            key={choice}
            size="sm"
            variant="outline"
            aria-pressed={colors === choice}
            className="aria-pressed:border-framing aria-pressed:bg-selection aria-pressed:text-selection-foreground"
            onClick={() => void choose(choice)}
          >
            {EDITOR_COLORS_LABELS[choice]}
          </Button>
        ))}
      </div>
      <p className="text-xs text-muted-foreground">
        Neovim and Vim in the embedded editor take the app's light or dark colours, or keep their own colorscheme; the next editor started takes a change.
      </p>
    </div>
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
              <ThemeField />
              <ReaderModeField />
              <EditorSettingField />
              <EditorColorsField />
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
                    onSignIn={() => signInOrShow(s, dispatch, account.name)}
                  />
                ))}
              </div>
              <div className="mt-3">
                <Button size="xs" variant="outline" onClick={() => dispatch({ type: "open_account_wizard" })}>
                  <Plus aria-hidden="true" />
                  Add account
                </Button>
              </div>
            </section>
          </>
        )}
      </div>
    </section>
  );
}
