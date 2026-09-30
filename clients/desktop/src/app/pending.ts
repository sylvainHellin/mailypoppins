// Message mutations, send holds and syncs: the reducer's pure transitions
// for them (clients/desktop/docs/shell.md, "Mutations and pending state").
//
// A mutation applies at once, as the TUI's does, and waits in `pending`
// until its command answers: a confirmed row keeps the optimistic state, a
// refused one is put back from what `pending` kept. The daemon has no undo,
// and a later server refusal arrives only as `mutations.rolled_back`.

import type { HoldStatus, MessageListRow, MutationsRolledBackPayload, OperationStatus } from "@/protocol/types";
import type { MailboxListing, MessageList, MovedTo, SyncMode } from "@/lib/gui-types";
import {
  listKey,
  markStale,
  readerKey,
  targetKey,
  type ActivityNotice,
  type AppState,
  type CountDelta,
  type HoldEntry,
  type HoldPhase,
  type MutationKind,
  type OperationEnd,
  type PendingChange,
  type Target,
} from "@/app/state";

const ACTIVITY_CAP = 50;
const SYNC_EARLY_CAP = 16;

/** Archive, delete, move and discard take the row out of the list. */
export function leaves(kind: MutationKind): boolean {
  return kind === "archive" || kind === "delete" || kind === "move" || kind === "discard";
}

/** One failed row of a settled batch, with the daemon's reason. */
export type Refusal = { target: Target; reason: string };

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type Mailbox = MailboxListing["mailboxes"][number];

function findMailbox(s: AppState, account: string, name: string): Mailbox | null {
  const rows = s.mailboxes[account]?.data?.mailboxes ?? [];
  return rows.find((m) => m.slug === name) ?? rows.find((m) => m.label === name) ?? null;
}

/** A mailbox's slug from its slug or its sidebar label (a server hit names the label). */
function slugOf(s: AppState, account: string, name: string): string {
  return findMailbox(s, account, name)?.slug ?? name;
}

function slugByRole(s: AppState, account: string, role: string): string | null {
  const listing = s.mailboxes[account]?.data?.mailboxes;
  if (listing) return listing.find((m) => m.role === role || m.kind === role)?.slug ?? null;
  return s.bootstrap?.snapshot.mailboxes[account]?.find((m) => m.role === role)?.slug ?? null;
}

/**
 * Move the sidebar counts. The badge follows unread when it showed unread,
 * else total when it showed total, and otherwise waits for the daemon.
 */
function adjustCounts(s: AppState, deltas: CountDelta[]): AppState {
  let mailboxes = s.mailboxes;
  for (const d of deltas) {
    const l = mailboxes[d.account];
    const listing = l?.data;
    if (!l || !listing) continue;
    let unreadMoved = 0;
    let totalMoved = 0;
    const rows = listing.mailboxes.map((m) => {
      if (m.slug !== d.mailbox) return m;
      const total = Math.max(0, m.total + d.total);
      const unread = Math.max(0, m.unread + d.unread);
      totalMoved = total - m.total;
      unreadMoved = m.kind === "drafts" ? 0 : unread - m.unread;
      const badge = m.badge === m.unread ? unread : m.badge === m.total ? total : m.badge;
      return { ...m, total, unread, badge };
    });
    const next: MailboxListing = {
      ...listing,
      mailboxes: rows,
      total: Math.max(0, listing.total + totalMoved),
      unread: Math.max(0, listing.unread + unreadMoved),
    };
    mailboxes = { ...mailboxes, [d.account]: { ...l, data: next } };
  }
  return mailboxes === s.mailboxes ? s : { ...s, mailboxes };
}

function negate(deltas: CountDelta[]): CountDelta[] {
  return deltas.map((d) => ({ ...d, total: -d.total, unread: -d.unread }));
}

function bumpList(s: AppState, keys: Iterable<string | null>): AppState {
  const listGen = { ...s.listGen };
  let moved = false;
  for (const k of new Set(keys)) {
    if (k === null) continue;
    listGen[k] = (listGen[k] ?? 0) + 1;
    moved = true;
  }
  return moved ? { ...s, listGen } : s;
}

function flagName(kind: MutationKind): "flagged" | "seen" {
  return kind === "flag" ? "flagged" : "seen";
}

/** The reader's `flags` words, with one of them set or cleared. */
function readerFlags(flags: string[], kind: MutationKind, on: boolean): string[] {
  const word = kind === "flag" ? "flagged" : "read";
  const rest = flags.filter((f) => f !== word);
  return on ? [...rest, word] : rest;
}

/** Set a flag on a row wherever the model shows it: the list, the search, the reader. */
function setFlagEverywhere(s: AppState, account: string, rowId: number, kind: MutationKind, on: boolean): AppState {
  const flag = flagName(kind);
  const list = s.messages.data;
  if (list?.kind === "messages" && list.account === account && list.rows.some((r) => r.id === rowId)) {
    const rows = list.rows.map((r) => (r.id === rowId ? { ...r, flags: { ...r.flags, [flag]: on } } : r));
    s = { ...s, messages: { ...s.messages, data: { ...list, rows } } };
  }
  if (s.search && s.search.hits.some((h) => h.account === account && h.row_id === rowId)) {
    const hits = s.search.hits.map((h) =>
      h.account === account && h.row_id === rowId ? { ...h, flags: { ...h.flags, [flag]: on } } : h,
    );
    s = { ...s, search: { ...s.search, hits } };
  }
  if (s.reader.meta && s.reader.key === readerKey(account, rowId)) {
    s = { ...s, reader: { ...s.reader, meta: { ...s.reader.meta, flags: readerFlags(s.reader.meta.flags, kind, on) } } };
  }
  return s;
}

export function pushNotice(s: AppState, notice: Omit<ActivityNotice, "id" | "rows"> & { rows?: ActivityNotice["rows"] }): AppState {
  const id = s.activitySeq + 1;
  const activity = [...s.activity, { rows: [], ...notice, id }].slice(-ACTIVITY_CAP);
  return { ...s, activity, activitySeq: id };
}

function plural(n: number, one: string): string {
  return `${n} ${one}${n === 1 ? "" : "s"}`;
}

const VERB: Record<MutationKind, string> = {
  archive: "Archived",
  delete: "Deleted",
  move: "Moved",
  flag: "Flagged",
  read: "Marked read",
  discard: "Discarded",
};

function appliedText(s: AppState, kind: MutationKind, account: string, n: number, value: boolean | null, movedTo: MovedTo | null): string {
  const what = plural(n, kind === "discard" ? "draft" : "message");
  if (kind === "flag" && value === false) return `Unflagged ${what}`;
  if (kind === "read") return `Marked ${what} ${value === false ? "unread" : "read"}`;
  if (kind === "move" && movedTo) {
    const label = findMailbox(s, account, movedTo.mailbox)?.label ?? movedTo.mailbox;
    return `Moved ${what} to ${label}`;
  }
  return `${VERB[kind]} ${what}`;
}

function failedText(kind: MutationKind, n: number): string {
  const what = plural(n, kind === "discard" ? "draft" : "message");
  const verb = { archive: "archive", delete: "delete", move: "move", flag: "flag", read: "mark", discard: "discard" }[kind];
  return `Could not ${verb} ${what}; ${n === 1 ? "it is" : "they are"} back in the list`;
}

function labelOf(e: PendingChange | undefined, t: Target): string {
  const subject = e?.prevRow?.row.subject ?? e?.prevHit?.hit.subject ?? e?.prevDraft?.entry.subject ?? null;
  if (subject) return subject;
  return "row_id" in t ? `(no subject) #${t.row_id}` : t.draft;
}

// ---------------------------------------------------------------------------
// Apply, settle, restore
// ---------------------------------------------------------------------------

function applyRow(
  s: AppState,
  batch: number,
  kind: MutationKind,
  t: { account: string; row_id: number },
  destination: string | null,
  value: boolean | null,
): AppState {
  const key = targetKey(t);
  const old = s.pending[key];
  // Already on its way out: a second archive of a row the list no longer shows.
  if (old && leaves(old.kind) && leaves(kind)) return s;

  const list = s.messages.data;
  const inList = list?.kind === "messages" && list.account === t.account ? list.rows.findIndex((r) => r.id === t.row_id) : -1;
  const listRow: MessageListRow | null = inList >= 0 && list?.kind === "messages" ? list.rows[inList] : null;
  const hitIdx = s.search ? s.search.hits.findIndex((h) => h.account === t.account && h.row_id === t.row_id) : -1;
  const hit = hitIdx >= 0 && s.search ? s.search.hits[hitIdx] : null;
  const flags = listRow?.flags ?? hit?.flags ?? null;
  const source =
    listRow && list?.kind === "messages" ? list.mailbox : hit ? slugOf(s, t.account, hit.mailbox) : old?.source ?? null;
  const dest =
    kind === "archive" ? slugByRole(s, t.account, "archive") : destination === null ? null : slugOf(s, t.account, destination);

  const counts: CountDelta[] = [];
  if (flags && source) {
    const unseen = flags.seen ? 0 : 1;
    if (leaves(kind)) {
      counts.push({ account: t.account, mailbox: source, total: -1, unread: -unseen });
      if (dest && dest !== source) counts.push({ account: t.account, mailbox: dest, total: 1, unread: unseen });
    } else if (kind === "read" && value !== null && flags.seen !== value) {
      counts.push({ account: t.account, mailbox: source, total: 0, unread: value ? -1 : 1 });
    }
  }

  // A second change of the same kind restores to before the first; a change
  // of another kind starts from what the model shows now.
  const same = old?.kind === kind;
  const nowRow = listRow && s.messages.key ? { key: s.messages.key, row: listRow, index: inList } : null;
  const nowHit = hit && s.search ? { hit, index: hitIdx, seq: s.search.seq } : null;
  const entry: PendingChange = {
    batch,
    kind,
    target: t,
    destination: dest,
    value,
    prevRow: same ? (old.prevRow ?? nowRow) : (nowRow ?? old?.prevRow ?? null),
    prevHit: same ? (old.prevHit ?? nowHit) : (nowHit ?? old?.prevHit ?? null),
    prevDraft: null,
    prevValue: same ? old.prevValue : flags ? flags[flagName(kind)] : null,
    source,
    counts: same ? [...old.counts, ...counts] : counts,
  };

  let next: AppState = { ...s, pending: { ...s.pending, [key]: entry } };
  if (leaves(kind)) {
    if (listRow && list?.kind === "messages") {
      const rows = list.rows.filter((r) => r.id !== t.row_id);
      next = { ...next, messages: { ...next.messages, data: { ...list, rows, total: Math.max(0, list.total - 1) } } };
    }
    if (hit && next.search) {
      next = { ...next, search: { ...next.search, hits: next.search.hits.filter((_, i) => i !== hitIdx) } };
    }
  } else if (value !== null) {
    next = setFlagEverywhere(next, t.account, t.row_id, kind, value);
  }
  next = adjustCounts(next, counts);
  return bumpList(next, [listRow ? s.messages.key : null, source ? listKey(t.account, source) : null]);
}

function applyDraft(s: AppState, batch: number, t: { account: string; draft: string }): AppState {
  const key = targetKey(t);
  if (s.pending[key]) return s;
  const list = s.messages.data;
  const idx = list?.kind === "drafts" && list.account === t.account ? list.listing.drafts.findIndex((d) => d.id === t.draft) : -1;
  const entry = idx >= 0 && list?.kind === "drafts" ? list.listing.drafts[idx] : null;
  const source = slugByRole(s, t.account, "drafts");
  const counts: CountDelta[] = source ? [{ account: t.account, mailbox: source, total: -1, unread: 0 }] : [];
  const change: PendingChange = {
    batch,
    kind: "discard",
    target: t,
    destination: null,
    value: null,
    prevRow: null,
    prevHit: null,
    prevDraft: entry && s.messages.key ? { key: s.messages.key, entry, index: idx } : null,
    prevValue: null,
    source,
    counts,
  };
  let next: AppState = { ...s, pending: { ...s.pending, [key]: change } };
  if (entry && list?.kind === "drafts") {
    const drafts = list.listing.drafts.filter((d) => d.id !== t.draft);
    next = { ...next, messages: { ...next.messages, data: { ...list, listing: { ...list.listing, drafts } } } };
  }
  next = adjustCounts(next, counts);
  return bumpList(next, [entry ? s.messages.key : null, source ? listKey(t.account, source) : null]);
}

/**
 * Apply one batch at once: rows leave the list and the search, or change a
 * flag in place, the sidebar counts move, and each row waits in `pending`.
 * The cursor rule is the reducer's (`mutation_apply`).
 */
export function applyMutation(
  s: AppState,
  batch: number,
  kind: MutationKind,
  targets: Target[],
  destination: string | null,
  value: boolean | null,
): AppState {
  let next = s;
  for (const t of targets) {
    next = "row_id" in t ? applyRow(next, batch, kind, t, destination, value) : applyDraft(next, batch, t);
  }
  if (leaves(kind) && next.marked.keys.size > 0) {
    const gone = new Set(targets.map(targetKey));
    const keys = new Set([...next.marked.keys].filter((k) => !gone.has(k)));
    const anchor = next.marked.anchor !== null && gone.has(next.marked.anchor) ? null : next.marked.anchor;
    next = { ...next, marked: { keys, anchor } };
  }
  return next;
}

/** Put one pending row back the way the model had it before the change. */
function restore(s: AppState, e: PendingChange): AppState {
  const t = e.target;
  if (leaves(e.kind)) {
    const list = s.messages.data;
    if (e.prevRow && list?.kind === "messages" && s.messages.key === e.prevRow.key && !list.rows.some((r) => r.id === e.prevRow!.row.id)) {
      const rows = [...list.rows];
      rows.splice(Math.min(e.prevRow.index, rows.length), 0, e.prevRow.row);
      s = { ...s, messages: { ...s.messages, data: { ...list, rows, total: list.total + 1 } } };
    }
    const search = s.search;
    if (e.prevHit && search && search.seq === e.prevHit.seq && !search.hits.some((h) => h.key === e.prevHit!.hit.key)) {
      const hits = [...search.hits];
      hits.splice(Math.min(e.prevHit.index, hits.length), 0, e.prevHit.hit);
      s = { ...s, search: { ...search, hits } };
    }
    if (e.prevDraft && list?.kind === "drafts" && s.messages.key === e.prevDraft.key && !list.listing.drafts.some((d) => d.id === e.prevDraft!.entry.id)) {
      const drafts = [...list.listing.drafts];
      drafts.splice(Math.min(e.prevDraft.index, drafts.length), 0, e.prevDraft.entry);
      s = { ...s, messages: { ...s.messages, data: { ...list, listing: { ...list.listing, drafts } } } };
    }
  } else if (e.prevValue !== null && "row_id" in t) {
    s = setFlagEverywhere(s, t.account, t.row_id, e.kind, e.prevValue);
  }
  return adjustCounts(s, negate(e.counts));
}

function staleCounts(s: AppState, account: string): AppState {
  const l = s.mailboxes[account];
  return l ? { ...s, mailboxes: { ...s.mailboxes, [account]: markStale(l) } } : s;
}

/**
 * A batch's command answered: every done row keeps its optimistic state and
 * leaves `pending`, every refused row is put back, and the activity area
 * says what happened. Rows a newer batch took over are that batch's.
 */
export function settleMutation(
  s: AppState,
  batch: number,
  kind: MutationKind,
  account: string,
  done: Target[],
  failed: Refusal[],
  value: boolean | null,
  movedTo: MovedTo | null,
): AppState {
  const pending = { ...s.pending };
  const touched: (string | null)[] = [];
  let next = s;
  for (const t of done) {
    const key = targetKey(t);
    const e = pending[key];
    if (e?.batch !== batch) continue;
    touched.push(e.prevRow?.key ?? e.prevDraft?.key ?? null, e.source ? listKey(account, e.source) : null);
    delete pending[key];
  }
  const rows: ActivityNotice["rows"] = failed.map((f) => {
    const key = targetKey(f.target);
    return { key, label: labelOf(pending[key], f.target), reason: f.reason };
  });
  // Last applied, first put back: each row's index was taken after the
  // batch's earlier rows had left, so the reverse order lands every one where it was.
  let restored = false;
  for (const f of [...failed].reverse()) {
    const key = targetKey(f.target);
    const e = pending[key];
    if (e?.batch !== batch) continue;
    touched.push(e.prevRow?.key ?? e.prevDraft?.key ?? null, e.source ? listKey(account, e.source) : null);
    delete pending[key];
    next = restore(next, e);
    restored = true;
  }
  next = bumpList({ ...next, pending }, touched);
  // The counts put back are a guess; the daemon's are the truth.
  if (restored) next = staleCounts(next, account);
  if (done.length > 0) {
    next = pushNotice(next, { kind: "applied", account, text: appliedText(next, kind, account, done.length, value, movedTo) });
  }
  if (failed.length > 0) {
    next = pushNotice(next, { kind: "failed", account, text: failedText(kind, failed.length), rows });
  }
  return next;
}

/**
 * `mutations.rolled_back`: the server refused queued changes of an account
 * after the commands answered. The event names no row: every row of that
 * account still pending is put back, and the reducer marks its lists stale
 * so the daemon's rows replace the rest.
 */
export function rolledBack(s: AppState, payload: MutationsRolledBackPayload): AppState {
  const entries = Object.entries(s.pending);
  let next = s;
  // Newest first, for the reason `settleMutation` gives.
  for (const [, e] of [...entries].reverse()) {
    if (e.target.account === payload.account) next = restore(next, e);
  }
  const pending = Object.fromEntries(entries.filter(([, e]) => e.target.account !== payload.account));
  next = { ...next, pending };
  // The words of mp-client's `mutations_rolled_back_line`, which the TUI shows.
  const text = `${payload.account}: ${payload.failed} mutation(s) failed and were rolled back (see the log)`;
  return pushNotice(next, { kind: "rolled_back", account: payload.account, text });
}

/** Lay the rows still pending over a list answer, which may predate their commit. */
export function overlayPending(s: AppState, list: MessageList): MessageList {
  const entries = Object.values(s.pending).filter((e) => e.target.account === list.account);
  if (entries.length === 0) return list;
  if (list.kind === "drafts") {
    const gone = new Set(entries.flatMap((e) => ("draft" in e.target ? [e.target.draft] : [])));
    const drafts = list.listing.drafts.filter((d) => !gone.has(d.id));
    return drafts.length === list.listing.drafts.length ? list : { ...list, listing: { ...list.listing, drafts } };
  }
  const here = entries.filter((e) => "row_id" in e.target && e.source === list.mailbox);
  if (here.length === 0) return list;
  const byId = new Map(here.map((e) => [(e.target as { row_id: number }).row_id, e]));
  let removed = 0;
  const rows = list.rows.flatMap((r) => {
    const e = byId.get(r.id);
    if (!e) return [r];
    if (leaves(e.kind)) {
      removed += 1;
      return [];
    }
    return e.value === null ? [r] : [{ ...r, flags: { ...r.flags, [flagName(e.kind)]: e.value } }];
  });
  return { ...list, rows, total: Math.max(0, list.total - removed) };
}

/** Is this list answer older than the list's last optimistic change? */
export function listAnswerIsStale(s: AppState, key: string, lgen: number | undefined, gen: number): boolean {
  if (lgen !== undefined && lgen !== (s.listGen[key] ?? 0)) return true;
  // Of two reloads of one list, the older answer never replaces the newer.
  return gen < s.messages.loadedGen;
}

/** Drop a list answer; ask for a fresh one when no newer request is out. */
export function dropListAnswer(s: AppState, gen: number): AppState {
  if (gen !== s.messages.gen) return s;
  return { ...s, messages: { ...s.messages, gen: s.messages.gen + 1 } };
}

// ---------------------------------------------------------------------------
// Holds
// ---------------------------------------------------------------------------

export function seedHolds(holds: HoldStatus[]): Record<string, HoldEntry> {
  const out: Record<string, HoldEntry> = {};
  for (const h of holds) out[h.operation_id] = { ...h, state: "started", cancelling: false };
  return out;
}

function terminal(state: HoldPhase): boolean {
  return state === "cancelled" || state === "fired";
}

function holdCancelledNotice(s: AppState, h: HoldStatus): AppState {
  const subject = h.subject ? `"${h.subject}"` : "the message";
  return pushNotice(s, { kind: "hold_cancelled", account: h.account, text: `Send of ${subject} cancelled` });
}

const HOLD_PHASE: Record<string, HoldPhase> = {
  "send.hold_started": "started",
  "send.hold_tick": "tick",
  "send.hold_cancelled": "cancelled",
  "send.hold_fired": "fired",
};

/** One `send.hold_*` event. A hold that fired or was cancelled stays so. */
export function holdEvent(s: AppState, kind: string, status: HoldStatus): AppState {
  const phase = HOLD_PHASE[kind];
  if (!phase) return s;
  const prev = s.holds[status.operation_id];
  if (prev && terminal(prev.state)) return s;
  const entry: HoldEntry = { ...status, state: phase, cancelling: terminal(phase) ? false : (prev?.cancelling ?? false) };
  const next = { ...s, holds: { ...s.holds, [status.operation_id]: entry } };
  return phase === "cancelled" ? holdCancelledNotice(next, status) : next;
}

export function holdCancelRequested(s: AppState, operationId: string): AppState {
  const h = s.holds[operationId];
  if (!h || terminal(h.state)) return s;
  return { ...s, holds: { ...s.holds, [operationId]: { ...h, cancelling: true } } };
}

/** `send_cancel_hold` answered; the event may have landed first. */
export function holdCancelAnswered(s: AppState, operationId: string, cancelled: boolean): AppState {
  const h = s.holds[operationId];
  if (!h || terminal(h.state)) return s;
  if (!cancelled) return { ...s, holds: { ...s.holds, [operationId]: { ...h, cancelling: false } } };
  const next = { ...s, holds: { ...s.holds, [operationId]: { ...h, state: "cancelled" as const, remaining_secs: 0, cancelling: false } } };
  return holdCancelledNotice(next, h);
}

/** A refused cancel: most often the hold fired first, and its event says so. */
export function holdCancelFailed(s: AppState, operationId: string, reason: string): AppState {
  const h = s.holds[operationId];
  // Already over, as this window knows: nothing is left to report.
  if (h && terminal(h.state)) return s;
  const next = h ? { ...s, holds: { ...s.holds, [operationId]: { ...h, cancelling: false } } } : s;
  const subject = h?.subject ? `"${h.subject}"` : "the message";
  return pushNotice(next, { kind: "hold_cancel_failed", account: h?.account ?? null, text: `Could not cancel the send of ${subject}: ${reason}` });
}

export function dismissHold(s: AppState, operationId: string): AppState {
  if (!(operationId in s.holds)) return s;
  const holds = { ...s.holds };
  delete holds[operationId];
  return { ...s, holds };
}

// ---------------------------------------------------------------------------
// Syncs
// ---------------------------------------------------------------------------

function syncEnded(s: AppState, end: OperationEnd): AppState {
  const run = s.syncs[end.operation_id];
  if (!run) return s;
  const syncs = { ...s.syncs };
  delete syncs[end.operation_id];
  const next = { ...s, syncs };
  if ("dropped" in end) {
    return pushNotice(next, { kind: "sync_failed", account: run.account, text: `Sync of ${run.account} was dropped: ${end.dropped}` });
  }
  if (end.state === "failed" || end.state === "cancelled") {
    const why = end.error ?? end.state;
    return pushNotice(next, { kind: "sync_failed", account: run.account, text: `Sync of ${run.account} failed: ${why}` });
  }
  return next;
}

/**
 * An operation ended. A sync this window awaits settles; while a
 * `sync_trigger` is unanswered, an end of an unknown id is held for it.
 */
export function syncSignal(s: AppState, end: OperationEnd): AppState {
  if (s.syncs[end.operation_id]) return syncEnded(s, end);
  if (s.syncStarting > 0) return { ...s, syncEarly: [...s.syncEarly, end].slice(-SYNC_EARLY_CAP) };
  return s;
}

export function isSyncOperation(s: AppState, operationId: string): boolean {
  return operationId in s.syncs;
}

export function syncRequested(s: AppState): AppState {
  return { ...s, syncStarting: s.syncStarting + 1 };
}

/** `sync_trigger` answered: await the id, or settle it now if its end came first. */
export function syncStarted(s: AppState, operationId: string, account: string, mode: SyncMode): AppState {
  const syncStarting = Math.max(0, s.syncStarting - 1);
  let next: AppState = { ...s, syncStarting, syncs: { ...s.syncs, [operationId]: { account, mode } } };
  const early = next.syncEarly.find((e) => e.operation_id === operationId);
  next = { ...next, syncEarly: syncStarting === 0 ? [] : next.syncEarly.filter((e) => e !== early) };
  return early ? syncEnded(next, early) : next;
}

export function syncStartFailed(s: AppState, account: string, reason: string): AppState {
  const syncStarting = Math.max(0, s.syncStarting - 1);
  const next = { ...s, syncStarting, syncEarly: syncStarting === 0 ? [] : s.syncEarly };
  return pushNotice(next, { kind: "sync_failed", account, text: `Sync of ${account} did not start: ${reason}` });
}

export function settledEnd(operationId: string, status: OperationStatus): OperationEnd {
  return { operation_id: operationId, state: status.state, error: status.error?.message ?? null };
}
