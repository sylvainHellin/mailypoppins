// The daemon's health checks that are not ok (P6-U8): the bootstrap
// snapshot's `diagnostics` seeds them and each `diagnostic.check_changed`
// replaces, adds or, on `ok`, drops one by name. A dismissed line stays
// hidden until its check's detail changes. Pure; the reducer applies,
// StatusRegion shows (clients/desktop/docs/shell.md, "Health checks").

import type { CheckStatus, HealthCheck } from "@/protocol/types";
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

/** A check's activity log line, the TUI's `"{name}: {detail}"`. */
export function checkLine(check: HealthCheck): string {
  return `${check.name}: ${check.detail}`;
}

/** The check's name as the banner shows it: `config_loaded` is the file the user edits. */
export function checkLabel(name: string): string {
  if (name.startsWith("account:")) return `Account ${name.slice("account:".length)}`;
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

/**
 * The dismissals that still hold after `health` changed: one whose check
 * still carries the detail it was dismissed with. A new detail or a recovery
 * lets it go, so the same sentence coming back later shows again.
 */
export function keptDismissals(dismissed: Readonly<Record<string, string>>, health: readonly HealthCheck[]): Record<string, string> {
  const kept: Record<string, string> = {};
  for (const check of health) {
    if (dismissed[check.name] === check.detail) kept[check.name] = check.detail;
  }
  return kept;
}

/**
 * The checks the banner shows: all of them, but a dismissed one while its
 * detail has not changed, and `config_loaded` while the config.toml banner is
 * up, which says the same with the line and the daemon's sentence.
 */
export function shownChecks(s: Pick<AppState, "health" | "healthDismissed" | "configProblem">): HealthCheck[] {
  return s.health.filter(
    (c) => s.healthDismissed[c.name] !== c.detail && !(s.configProblem && c.name === "config_loaded"),
  );
}

/** Whether `health` already holds `check` as it is, so a re-bootstrap does not log it again. */
export function knownCheck(health: readonly HealthCheck[], check: HealthCheck): boolean {
  return health.some((c) => c.name === check.name && c.status === check.status && c.detail === check.detail);
}
