// The activity log (OBS-01): every line the window reported, the notice line's
// and the activity area's, plus the daemon events worth a line, in one ring of
// ACTIVITY_LOG_CAP entries (the TUI's `STATUS_LOG_CAPACITY`). Pure; the
// reducer appends, ActivityLogDialog reads (clients/desktop/docs/shell.md,
// "Activity log").

import type { ConfigChanged, ConfigInvalid, HoldStatus, SyncCompleted } from "@/protocol/types";
import type { ActivityKind, ActivityLevel, ActivityLogEntry, ActivityNotice, AppState } from "@/app/state";

/** How many lines the log keeps, the TUI's `STATUS_LOG_CAPACITY`; a newer one drops the oldest. */
export const ACTIVITY_LOG_CAP = 100;

/**
 * How long a notice that needs reading stays: a failure, a send that failed or
 * went to only some recipients, and the reader's blocked-link notice. A
 * pointer resting on the notice holds it (useWindDown). The failure stays in
 * the activity log, and a failed send in the outbox.
 */
export const STICKY_MS = 20_000;

/** Kinds that report a failure: they stay for STICKY_MS or until dismissed, are alerts, and log as errors. */
export const FAILURES: ReadonlySet<ActivityKind> = new Set<ActivityKind>([
  "failed",
  "rolled_back",
  "hold_cancel_failed",
  "sync_failed",
  "compose_failed",
  "send_failed",
  "send_partial",
  "rebuild_refused",
]);

/** A notice kind's level in the log: a failure is an error, anything else information. */
export function noticeLevel(kind: ActivityKind): ActivityLevel {
  return FAILURES.has(kind) ? "error" : "info";
}

/** Append one line, stamped now, dropping the oldest past the cap. */
export function logActivity(s: AppState, level: ActivityLevel, text: string, at: Date = new Date()): AppState {
  const id = s.activityLogSeq + 1;
  const entry: ActivityLogEntry = { id, at: at.toISOString(), level, text };
  const activityLog = [...s.activityLog, entry].slice(-ACTIVITY_LOG_CAP);
  return { ...s, activityLog, activityLogSeq: id };
}

/** A notice of the activity area as its log line: its text, then each failed row with its reason. */
export function noticeLine(notice: Pick<ActivityNotice, "text" | "rows">): string {
  if (notice.rows.length === 0) return notice.text;
  return `${notice.text} (${notice.rows.map((r) => `${r.label}: ${r.reason}`).join("; ")})`;
}

/**
 * The notice line shows `text` and the log keeps it; null clears the line
 * and logs nothing.
 */
export function withNotice(s: AppState, text: string | null, level: ActivityLevel = "info"): AppState {
  if (text === null) return { ...s, notice: null };
  return logActivity({ ...s, notice: text }, level, text);
}

const ZERO_TICK: SyncCompleted = {
  account: "",
  severity: "ok",
  saved: 0,
  skipped: 0,
  flags_updated: 0,
  pruned: 0,
  prunes_deferred: 0,
  uid_rebound: 0,
  uidvalidity_resets: 0,
  bodies_truncated: 0,
  non_converging: [],
  failed_mutations: 0,
  error: null,
  new_inbox_mail: [],
};

/**
 * The TUI's line for a finished tick (`mp_client::format::sync_status_line`),
 * with the account named in the success line too, since the log interleaves
 * every account's ticks.
 */
export function syncLine(event: SyncCompleted): string {
  // Every key is always on the wire; a partial payload still reads as a line.
  const p: SyncCompleted = { ...ZERO_TICK, ...event };
  if (p.error !== null) return `Fetch failed (${p.account}): ${p.error}`;
  let line = `Synced ${p.account}: ${p.saved} new, ${p.skipped} existing`;
  if (p.flags_updated > 0) line += `, ${p.flags_updated} status updated`;
  if (p.uid_rebound > 0) line += `, ${p.uid_rebound} renumbered`;
  if (p.pruned > 0) line += `, ${p.pruned} no longer in this mailbox`;
  if (p.prunes_deferred > 0) line += `, ${p.prunes_deferred} removal(s) held back (incomplete pass, run a full sync)`;
  if (p.bodies_truncated > 0) line += `, ${p.bodies_truncated} mailbox(es) stopped at the fetch deadline (resuming next sync)`;
  if (p.non_converging.length > 0) line += `, fetch not converging on ${p.non_converging.join(", ")} (see the log)`;
  if (p.failed_mutations > 0) line += `; ${p.failed_mutations} mutation(s) failed and were rolled back (see the log)`;
  return line;
}

/** `config.changed`: which accounts the swap added, updated and removed. */
export function configChangedLine(p: ConfigChanged): string {
  const parts = [
    p.added.length > 0 ? `added ${p.added.join(", ")}` : null,
    p.updated.length > 0 ? `updated ${p.updated.join(", ")}` : null,
    p.removed.length > 0 ? `removed ${p.removed.join(", ")}` : null,
  ].filter((x): x is string => x !== null);
  return parts.length > 0 ? `Configuration reloaded: ${parts.join("; ")}` : "Configuration reloaded: no account changed";
}

/** `config.invalid`: the file, the line when there is one, and why. */
export function configInvalidLine(p: ConfigInvalid): string {
  const where = p.line === null ? p.path : `${p.path}, line ${p.line}`;
  return `The configuration was refused (${where}): ${p.message}`;
}

/**
 * `send.hold_fired`: the undo window closed and the send goes on. Logged by
 * the hold's own transition, once per hold; a cancelled hold logs through its
 * notice ("Send of ... cancelled"), whichever of the event and this window's
 * cancel answer came first.
 */
export function holdFiredLine(p: HoldStatus): string {
  return `Hold over, sending ${p.subject ? `"${p.subject}"` : "the message"} from ${p.account}`;
}

/**
 * The log line of a daemon event: `sync.completed`, `config.changed` and
 * `config.invalid`; null for any other kind (the two hold ends log through
 * the hold's transition, {@link holdFiredLine}). These arrive whoever caused
 * them: another client's sync or reload is in this window's log too.
 */
export function eventLine(kind: string, payload: unknown): { level: ActivityLevel; text: string } | null {
  switch (kind) {
    case "sync.completed": {
      const p = payload as SyncCompleted;
      const failed = p.severity === "error" || (p.error !== null && p.error !== undefined);
      const level: ActivityLevel = failed ? "error" : p.severity === "warning" ? "warning" : "info";
      return { level, text: syncLine(p) };
    }
    case "config.changed":
      return { level: "info", text: configChangedLine(payload as ConfigChanged) };
    case "config.invalid":
      return { level: "error", text: configInvalidLine(payload as ConfigInvalid) };
    default:
      return null;
  }
}

/** Log the event's line, when it has one. */
export function logEvent(s: AppState, kind: string, payload: unknown): AppState {
  const line = eventLine(kind, payload);
  return line ? logActivity(s, line.level, line.text) : s;
}

/**
 * The entries a filter keeps, the TUI's rule: a case-insensitive match on the
 * text or the level's name; an empty filter keeps them all.
 */
export function filterLog(entries: readonly ActivityLogEntry[], filter: string): ActivityLogEntry[] {
  const f = filter.trim().toLowerCase();
  if (!f) return [...entries];
  return entries.filter((e) => e.text.toLowerCase().includes(f) || e.level.includes(f));
}

/** An entry's time as the log shows it, local `HH:MM:SS`. */
export function entryTime(at: string): string {
  const d = new Date(at);
  const two = (n: number) => String(n).padStart(2, "0");
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}
