// The daemon's health checks that are not ok (P6-U8): the bootstrap
// snapshot's `diagnostics` seeds them and each `diagnostic.check_changed`
// replaces, adds or, on `ok`, drops one by name. A dismissed line stays
// hidden until its check's status or detail changes. Pure; the reducer applies,
// StatusRegion shows (clients/desktop/docs/shell.md, "Health checks").

import type { CheckStatus, HealthCheck, RejectedLogin } from "@/protocol/types";
import type { ActivityLevel, AppState } from "@/app/state";

const STATUSES: ReadonlySet<string> = new Set<CheckStatus>(["ok", "warn", "fail"]);

/**
 * Whether a payload is a check: a `diagnostic.check_changed` that is not one
 * is skipped, as the TUI skips it, rather than shown as half a banner.
 */
export function isHealthCheck(payload: unknown): payload is HealthCheck {
  const p = payload as Partial<HealthCheck> | null;
  return !!p && typeof p.name === "string" && typeof p.detail === "string" && typeof p.status === "string" && STATUSES.has(p.status);
}

/** A check's level in the activity log: a recovery informs, the TUI's success line. */
export function checkLevel(status: CheckStatus): ActivityLevel {
  return status === "fail" ? "error" : status === "warn" ? "warning" : "info";
}

/**
 * A check's activity log line, the TUI's `"{name}: {detail}"`; a
 * `calendar:<account>` check, whose detail already names the calendar, opens on
 * the account alone.
 */
export function checkLine(check: HealthCheck): string {
  const subject = check.name.startsWith("calendar:") ? check.name.slice("calendar:".length) : check.name;
  return `${subject}: ${check.detail}`;
}

/** The check's name as the banner shows it: `config_loaded` is the file the user edits. */
export function checkLabel(name: string): string {
  if (name.startsWith("account:")) return `Account ${name.slice("account:".length)}`;
  // PERSO-200: the account whose sign-in lacks the calendar scope; the
  // detail is the re-login prompt.
  if (name.startsWith("calendar:")) return `Account ${name.slice("calendar:".length)}`;
  switch (name) {
    case "config_loaded":
      return "config.toml";
    case "store_open":
      return "Account stores";
    case "socket_owner":
      return "Daemon socket";
    case "log_writable":
      return "Daemon log";
    default:
      return name;
  }
}

/** The account an `account:<name>` check is about, null for any other check. */
export function checkAccount(name: string): string | null {
  return name.startsWith("account:") ? name.slice("account:".length) : null;
}

/**
 * The login of `account` the server rejected (PERSO-296), as
 * `account.credentials` last answered, null when none: the first of
 * `rejected_logins`, the one the daemon's sentence names. The daemon holds
 * that password and says so in the account's check, whose line then offers
 * the dialog that sets it, and the account picker names it.
 */
export function loginRejected(s: Pick<AppState, "credentials">, account: string): RejectedLogin | null {
  return s.credentials.data?.accounts.find((c) => c.account === account)?.rejected_logins[0] ?? null;
}

/** Whether `account.credentials` last said any account's login is rejected. */
function anyLoginRejected(s: Pick<AppState, "credentials">): boolean {
  return s.credentials.data?.accounts.some((c) => c.rejected_logins.length > 0) ?? false;
}

/**
 * Whether `account.credentials` is read: while the Settings view shows them,
 * while an account's check is up, since it may be a rejected login
 * (PERSO-296), and while they say a login is rejected, so the check's
 * recovery, which leaves no account check up, still reads them again. Each
 * change of an account's check marks them stale.
 */
export function credentialsWanted(s: Pick<AppState, "view" | "health" | "credentials">): boolean {
  return (
    s.view === "settings" ||
    s.health.some((c) => checkAccount(c.name) !== null) ||
    anyLoginRejected(s)
  );
}

/**
 * Whether a bootstrap whose checks are `health` may have moved a rejected
 * login (PERSO-296): it carries an account check this window does not hold
 * as it is, or the credentials say a login is rejected, which a check that
 * cleared while away, or a restarted daemon, no longer holds.
 */
export function loginMayHaveMoved(s: Pick<AppState, "health" | "credentials">, health: readonly HealthCheck[]): boolean {
  return (
    health.some((c) => checkAccount(c.name) !== null && !knownCheck(s.health, c)) ||
    anyLoginRejected(s)
  );
}

/**
 * The checks after one changed: a check that is not ok replaces its entry,
 * or joins the end; an ok one leaves.
 */
export function withCheck(health: readonly HealthCheck[], check: HealthCheck): HealthCheck[] {
  if (check.status === "ok") return health.filter((c) => c.name !== check.name);
  const at = health.findIndex((c) => c.name === check.name);
  if (at < 0) return [...health, check];
  return health.map((c, i) => (i === at ? check : c));
}

/** What a dismissal remembers of the line it hid: its status and its sentence. */
export type Dismissal = Pick<HealthCheck, "status" | "detail">;

/** Whether `dismissal` still hides `check`: the same status and the same sentence. */
export function hides(dismissal: Dismissal | undefined, check: HealthCheck): boolean {
  return !!dismissal && dismissal.status === check.status && dismissal.detail === check.detail;
}

/**
 * The dismissals that still hold after `health` changed: one whose check
 * still carries the status and the detail it was dismissed with. A new
 * status, a new detail or a recovery lets it go, so a warning that turns
 * into a failure with the same sentence shows again, and so does the same
 * line coming back later.
 */
export function keptDismissals(dismissed: Readonly<Record<string, Dismissal>>, health: readonly HealthCheck[]): Record<string, Dismissal> {
  const kept: Record<string, Dismissal> = {};
  for (const check of health) {
    if (hides(dismissed[check.name], check)) kept[check.name] = dismissed[check.name];
  }
  return kept;
}

/**
 * The checks the banner shows: all of them, but a dismissed one while its
 * status and detail have not changed, and `config_loaded` while the config.toml banner is
 * up, which says the same with the line and the daemon's sentence.
 */
export function shownChecks(s: Pick<AppState, "health" | "healthDismissed" | "configProblem">): HealthCheck[] {
  return s.health.filter(
    (c) => !hides(s.healthDismissed[c.name], c) && !(s.configProblem && c.name === "config_loaded"),
  );
}

/** Whether `health` already holds `check` as it is, so a re-bootstrap does not log it again. */
export function knownCheck(health: readonly HealthCheck[], check: HealthCheck): boolean {
  return health.some((c) => c.name === check.name && c.status === check.status && c.detail === check.detail);
}
