// Message mutations, send holds, syncs and sends: the reducer's pure transitions
// for them (clients/desktop/docs/shell.md, "Mutations and pending state").
//
// A mutation applies at once, as the TUI's does, and waits in `pending`
// until its command answers: a confirmed row keeps the optimistic state, a
// refused one is put back from what `pending` kept, axis by axis (the flag,
// the read state, leaving the list). The daemon has no undo, and a later
// server refusal arrives only as `mutations.rolled_back`.

import type {
  ApprovedOutcome,
  DraftInvalid,
  HoldStatus,
  MessageListRow,
  MutationsRolledBackPayload,
  OperationStatus,
  RecipientOutcome,
  SendOutcome,
} from "@/protocol/types";
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
  type PendingFlag,
  type PendingLeave,
  type SendResult,
  type SendRun,
  type Target,
} from "@/app/state";

/** The notices `state.activity` keeps; a newer one drops the oldest, failure or not. */
export const ACTIVITY_CAP = 20;
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

/** The two in-place axes of a message; leaving the list is the third, a draft's status the fourth. */
type FlagAxis = "flag" | "read";
type Axis = FlagAxis | "leave" | "status";

/** The order a rollback puts the axes back in: the row first, then its flags and status. */
const AXES: readonly Axis[] = ["leave", "status", "read", "flag"];

/** Approve and demote change a draft's status in place. */
export function changesStatus(kind: MutationKind): boolean {
  return kind === "approve" || kind === "demote";
}

function flagAxis(kind: MutationKind): FlagAxis {
  return kind === "flag" ? "flag" : "read";
}

function flagName(axis: FlagAxis): "flagged" | "seen" {
  return axis === "flag" ? "flagged" : "seen";
}

/** The reader's `flags` words, with one of them set or cleared. */
function readerFlags(flags: string[], axis: FlagAxis, on: boolean): string[] {
  const word = axis === "flag" ? "flagged" : "read";
  const rest = flags.filter((f) => f !== word);
  return on ? [...rest, word] : rest;
}

/** Set a flag on a row wherever the model shows it: the list, the search, the reader. */
function setFlagEverywhere(s: AppState, account: string, rowId: number, axis: FlagAxis, on: boolean): AppState {
  const flag = flagName(axis);
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
    s = { ...s, reader: { ...s.reader, meta: { ...s.reader.meta, flags: readerFlags(s.reader.meta.flags, axis, on) } } };
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
  approve: "Approved",
  demote: "Put back to draft",
};

function noun(kind: MutationKind): string {
  return kind === "discard" || changesStatus(kind) ? "draft" : "message";
}

function appliedText(s: AppState, kind: MutationKind, account: string, n: number, value: boolean | null, movedTo: MovedTo | null): string {
  const what = plural(n, noun(kind));
  if (kind === "demote") return `Put ${what} back to draft`;
  if (kind === "flag" && value === false) return `Unflagged ${what}`;
  if (kind === "read") return `Marked ${what} ${value === false ? "unread" : "read"}`;
  if (kind === "move" && movedTo) {
    const label = findMailbox(s, account, movedTo.mailbox)?.label ?? movedTo.mailbox;
    return `Moved ${what} to ${label}`;
  }
  return `${VERB[kind]} ${what}`;
}

function failedText(kind: MutationKind, n: number): string {
  const what = plural(n, noun(kind));
  const verb = {
    archive: "archive",
    delete: "delete",
    move: "move",
    flag: "flag",
    read: "mark",
    discard: "discard",
    approve: "approve",
    demote: "demote",
  }[kind];
  const keeps = `${n === 1 ? "it keeps its" : "they keep their"} status`;
  if (kind === "demote") return `Could not put ${what} back to draft; ${keeps}`;
  if (kind === "approve") return `Could not approve ${what}; ${keeps}`;
  return `Could not ${verb} ${what}; ${n === 1 ? "it is" : "they are"} back in the list`;
}

function labelOf(e: PendingChange | undefined, t: Target): string {
  if (e?.subject) return e.subject;
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
  if (old?.leave && leaves(kind)) return s;

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

  // Each axis keeps its own saved state and batch. A second change of the
  // same axis restores to before the first; the other axes stay as they are.
  const base: PendingChange = old ?? { target: t, source, subject: null, flag: null, read: null, leave: null, status: null };
  let entry: PendingChange = { ...base, source, subject: base.subject ?? listRow?.subject ?? hit?.subject ?? null };
  if (leaves(kind)) {
    const prevRow = listRow && s.messages.key ? { key: s.messages.key, row: listRow, index: inList } : null;
    const prevHit = hit && s.search ? { hit, index: hitIdx, seq: s.search.seq } : null;
    entry = { ...entry, leave: { batch, kind, destination: dest, prevRow, prevHit, prevDraft: null, counts } };
  } else {
    const axis = flagAxis(kind);
    const was = base[axis];
    const saved: PendingFlag = {
      batch,
      value,
      prev: was ? was.prev : flags ? flags[flagName(axis)] : null,
      counts: was ? [...was.counts, ...counts] : counts,
    };
    entry = { ...entry, [axis]: saved };
  }

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
    next = setFlagEverywhere(next, t.account, t.row_id, flagAxis(kind), value);
  }
  next = adjustCounts(next, counts);
  return bumpList(next, [listRow ? s.messages.key : null, source ? listKey(t.account, source) : null]);
}

function applyDraft(s: AppState, batch: number, t: { account: string; draft: string }): AppState {
  const key = targetKey(t);
  const old = s.pending[key];
  if (old?.leave) return s;
  const list = s.messages.data;
  const idx = list?.kind === "drafts" && list.account === t.account ? list.listing.drafts.findIndex((d) => d.id === t.draft) : -1;
  const entry = idx >= 0 && list?.kind === "drafts" ? list.listing.drafts[idx] : null;
  const source = slugByRole(s, t.account, "drafts");
  const counts: CountDelta[] = source ? [{ account: t.account, mailbox: source, total: -1, unread: 0 }] : [];
  const change: PendingChange = {
    target: t,
    source,
    subject: old?.subject ?? entry?.subject ?? null,
    flag: null,
    read: null,
    status: old?.status ?? null,
    leave: {
      batch,
      kind: "discard",
      destination: null,
      prevRow: null,
      prevHit: null,
      prevDraft: entry && s.messages.key ? { key: s.messages.key, entry, index: idx } : null,
      counts,
    },
  };
  let next: AppState = { ...s, pending: { ...s.pending, [key]: change } };
  if (entry && list?.kind === "drafts") {
    const drafts = list.listing.drafts.filter((d) => d.id !== t.draft);
    next = { ...next, messages: { ...next.messages, data: { ...list, listing: { ...list.listing, drafts } } } };
  }
  next = adjustCounts(next, counts);
  return bumpList(next, [entry ? s.messages.key : null, source ? listKey(t.account, source) : null]);
}

/** Set a draft's status in the shown Drafts list. */
function setDraftStatus(s: AppState, account: string, id: string, status: string): AppState {
  const list = s.messages.data;
  if (list?.kind !== "drafts" || list.account !== account) return s;
  if (!list.listing.drafts.some((d) => d.id === id && d.status !== status)) return s;
  const drafts = list.listing.drafts.map((d) => (d.id === id ? { ...d, status } : d));
  return { ...s, messages: { ...s.messages, data: { ...list, listing: { ...list.listing, drafts } } } };
}

/** Approve or demote one draft in place; a second change takes the axis over and keeps the first one's `prev`. */
function applyDraftStatus(s: AppState, batch: number, kind: MutationKind, t: { account: string; draft: string }): AppState {
  const key = targetKey(t);
  const old = s.pending[key];
  if (old?.leave) return s;
  const list = s.messages.data;
  const entry = list?.kind === "drafts" && list.account === t.account ? list.listing.drafts.find((d) => d.id === t.draft) : undefined;
  const value = kind === "approve" ? "approved" : "draft";
  const base: PendingChange = old ?? {
    target: t,
    source: slugByRole(s, t.account, "drafts"),
    subject: entry?.subject ?? null,
    flag: null,
    read: null,
    leave: null,
    status: null,
  };
  const status = { batch, value, prev: old?.status ? old.status.prev : (entry?.status ?? null) };
  const next: AppState = { ...s, pending: { ...s.pending, [key]: { ...base, status } } };
  return bumpList(setDraftStatus(next, t.account, t.draft, value), [entry ? s.messages.key : null]);
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
    if ("row_id" in t) next = applyRow(next, batch, kind, t, destination, value);
    else next = changesStatus(kind) ? applyDraftStatus(next, batch, kind, t) : applyDraft(next, batch, t);
  }
  if (leaves(kind) && next.marked.keys.size > 0) {
    const gone = new Set(targets.map(targetKey));
    const keys = new Set([...next.marked.keys].filter((k) => !gone.has(k)));
    const anchor = next.marked.anchor !== null && gone.has(next.marked.anchor) ? null : next.marked.anchor;
    next = { ...next, marked: { keys, anchor } };
  }
  return next;
}

/** The axis of a row's entry that `batch` owns, if any. */
function axisOf(e: PendingChange, batch: number): Axis | null {
  return AXES.find((a) => e[a]?.batch === batch) ?? null;
}

/** The entry less one axis, or null when nothing of it is left pending. */
function withoutAxis(e: PendingChange, axis: Axis): PendingChange | null {
  const rest: PendingChange = { ...e, [axis]: null };
  return rest.flag || rest.read || rest.leave || rest.status ? rest : null;
}

/** The list keys an entry touches: the list it left, and its source mailbox's. */
function entryKeys(e: PendingChange): (string | null)[] {
  const shown = e.leave?.prevRow?.key ?? e.leave?.prevDraft?.key ?? null;
  return [shown, e.source ? listKey(e.target.account, e.source) : null];
}

/** A saved row and hit with one flag set, so a row still out of its list comes back with it. */
function withSavedFlag(l: PendingLeave, axis: FlagAxis, on: boolean): PendingLeave {
  const flag = flagName(axis);
  const prevRow = l.prevRow && { ...l.prevRow, row: { ...l.prevRow.row, flags: { ...l.prevRow.row.flags, [flag]: on } } };
  const prevHit = l.prevHit && { ...l.prevHit, hit: { ...l.prevHit.hit, flags: { ...l.prevHit.hit.flags, [flag]: on } } };
  return { ...l, prevRow, prevHit };
}

/**
 * Put a row back at its saved index. The index is a guess: another change of
 * the list since the apply moves it, which is why a restore re-reads the list.
 */
function restoreLeave(s: AppState, l: PendingLeave): AppState {
  const list = s.messages.data;
  if (l.prevRow && list?.kind === "messages" && s.messages.key === l.prevRow.key && !list.rows.some((r) => r.id === l.prevRow!.row.id)) {
    const rows = [...list.rows];
    rows.splice(Math.min(l.prevRow.index, rows.length), 0, l.prevRow.row);
    s = { ...s, messages: { ...s.messages, data: { ...list, rows, total: list.total + 1 } } };
  }
  const search = s.search;
  if (l.prevHit && search && search.seq === l.prevHit.seq && !search.hits.some((h) => h.key === l.prevHit!.hit.key)) {
    const hits = [...search.hits];
    hits.splice(Math.min(l.prevHit.index, hits.length), 0, l.prevHit.hit);
    s = { ...s, search: { ...search, hits } };
  }
  if (l.prevDraft && list?.kind === "drafts" && s.messages.key === l.prevDraft.key && !list.listing.drafts.some((d) => d.id === l.prevDraft!.entry.id)) {
    const drafts = [...list.listing.drafts];
    drafts.splice(Math.min(l.prevDraft.index, drafts.length), 0, l.prevDraft.entry);
    s = { ...s, messages: { ...s.messages, data: { ...list, listing: { ...list.listing, drafts } } } };
  }
  return adjustCounts(s, negate(l.counts));
}

/**
 * Put one axis of a pending row back the way the model had it before that
 * axis changed; the other axes stay pending. Returns what is left of the entry.
 */
function restoreAxis(s: AppState, e: PendingChange, axis: Axis): { s: AppState; rest: PendingChange | null } {
  const t = e.target;
  let rest = withoutAxis(e, axis);
  if (axis === "status") {
    const prev = e.status!.prev;
    if (prev !== null && "draft" in t) s = setDraftStatus(s, t.account, t.draft, prev);
    return { s, rest };
  }
  if (axis === "leave") {
    s = restoreLeave(s, e.leave!);
    // The row is back: a flag or read change still pending on it shows again.
    if (rest && "row_id" in t) {
      for (const a of ["flag", "read"] as const) {
        const v = rest[a]?.value;
        if (v !== null && v !== undefined) s = setFlagEverywhere(s, t.account, t.row_id, a, v);
      }
    }
    return { s, rest };
  }
  const f = e[axis]!;
  if (f.prev !== null && "row_id" in t) {
    s = setFlagEverywhere(s, t.account, t.row_id, axis, f.prev);
    // Still out of its list: it comes back with the flag put back.
    if (rest?.leave) rest = { ...rest, leave: withSavedFlag(rest.leave, axis, f.prev) };
  }
  return { s: adjustCounts(s, negate(f.counts)), rest };
}

function staleCounts(s: AppState, account: string): AppState {
  const l = s.mailboxes[account];
  return l ? { ...s, mailboxes: { ...s.mailboxes, [account]: markStale(l) } } : s;
}

/** Re-read the shown list when it is one of `keys`. */
function staleList(s: AppState, keys: ReadonlySet<string>): AppState {
  return s.messages.key !== null && keys.has(s.messages.key)
    ? { ...s, messages: { ...markStale(s.messages), key: s.messages.key } }
    : s;
}

/**
 * A batch's command answered: every done row keeps its optimistic state and
 * its axis leaves `pending`, every refused row has its axis put back, and
 * the activity area says what happened. An axis a newer batch took over is
 * that batch's; a refused row this batch owns no axis of any more is re-read.
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
  const keep = (key: string, rest: PendingChange | null) => {
    if (rest) pending[key] = rest;
    else delete pending[key];
  };
  const touched: (string | null)[] = [];
  let next = s;
  for (const t of done) {
    const key = targetKey(t);
    const e = pending[key];
    const axis = e ? axisOf(e, batch) : null;
    if (!e || !axis) continue;
    touched.push(...entryKeys(e));
    keep(key, withoutAxis(e, axis));
  }
  const rows: ActivityNotice["rows"] = failed.map((f) => {
    const key = targetKey(f.target);
    return { key, label: labelOf(pending[key], f.target), reason: f.reason };
  });
  // Last applied, first put back: each row's index was taken after the
  // batch's earlier rows had left, so the reverse order lands every one
  // where it was, as long as nothing else changed the list meanwhile.
  const reread = new Set<string>();
  for (const f of [...failed].reverse()) {
    const key = targetKey(f.target);
    const e = pending[key];
    if (!e) continue;
    const keys = entryKeys(e);
    touched.push(...keys);
    for (const k of keys) if (k !== null) reread.add(k);
    // A newer change of the same axis took it over, and what this batch set
    // is not the model's to put back: the daemon's rows and counts say.
    const axis = axisOf(e, batch);
    if (!axis) continue;
    const r = restoreAxis(next, e, axis);
    next = r.s;
    keep(key, r.rest);
  }
  next = bumpList({ ...next, pending }, touched);
  // What was put back is a guess: a saved index predates any other change of
  // the list, and the counts are the model's. The daemon's replace both.
  if (reread.size > 0) next = staleList(staleCounts(next, account), reread);
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
    if (e.target.account !== payload.account) continue;
    let rest: PendingChange | null = e;
    for (const axis of AXES) {
      if (!rest?.[axis]) continue;
      ({ s: next, rest } = restoreAxis(next, rest, axis));
    }
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
    const byId = new Map(entries.flatMap((e) => ("draft" in e.target ? [[e.target.draft, e] as const] : [])));
    let changed = false;
    const drafts = list.listing.drafts.flatMap((d) => {
      const e = byId.get(d.id);
      if (!e) return [d];
      changed = true;
      if (e.leave) return [];
      return e.status && e.status.value !== d.status ? [{ ...d, status: e.status.value }] : [d];
    });
    return changed ? { ...list, listing: { ...list.listing, drafts } } : list;
  }
  const here = entries.filter((e) => "row_id" in e.target && e.source === list.mailbox);
  if (here.length === 0) return list;
  const byId = new Map(here.map((e) => [(e.target as { row_id: number }).row_id, e]));
  let removed = 0;
  const rows = list.rows.flatMap((r) => {
    const e = byId.get(r.id);
    if (!e) return [r];
    if (e.leave) {
      removed += 1;
      return [];
    }
    let flags = r.flags;
    for (const axis of ["flag", "read"] as const) {
      const v = e[axis]?.value;
      if (v !== null && v !== undefined) flags = { ...flags, [flagName(axis)]: v };
    }
    return flags === r.flags ? [r] : [{ ...r, flags }];
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
  return { operation_id: operationId, state: status.state, error: status.error?.message ?? null, result: status.result };
}

// ---------------------------------------------------------------------------
// Sends
// ---------------------------------------------------------------------------

const SEND_EARLY_CAP = 16;

/** What a dropped send says: the daemon restarted, and only the outbox knows. */
export const SEND_INTERRUPTED = "The send was interrupted; check the outbox";

function refusedList(rs: RecipientOutcome[]): string {
  return rs.map((r) => (r.error ? `${r.address} (${r.error})` : r.address)).join(", ");
}

/**
 * One draft's settled `SendOutcome`. A message some recipients refused is
 * "Partly delivered" and names them (SND-08), never a plain failure; one
 * every recipient refused failed.
 */
export function sendResult(outcome: SendOutcome): SendResult {
  const refused = outcome.recipients.filter((r) => !r.delivered);
  if (refused.length === 0) {
    const text = outcome.settle_error ? `Sent; the draft file was not retired: ${outcome.settle_error}` : "Sent";
    return { tone: "sent", text, sticky: false };
  }
  if (refused.length < outcome.recipients.length) {
    return { tone: "partial", text: `Partly delivered: ${refusedList(refused)}`, sticky: true };
  }
  return { tone: "failed", text: `Failed: every recipient was refused: ${refusedList(refused)}`, sticky: true };
}

/** The draft id a batch result names, from its selector. */
function draftOf(selector: string | null): string {
  return selector?.split("/").pop() ?? "(unknown draft)";
}

/**
 * A settled `send.approved`: "Sent N, failed M" (the TUI's "No approved
 * emails found" for an empty batch), and every draft that failed or went
 * to only some of its recipients as a row of the failure notice.
 */
export function approvedResult(outcome: ApprovedOutcome, account: string): { result: SendResult; rows: ActivityNotice["rows"] } {
  if (outcome.sent === 0 && outcome.failed === 0) {
    return { result: { tone: "sent", text: "No approved emails found", sticky: false }, rows: [] };
  }
  const rows = outcome.results.flatMap((r) => {
    const refused = r.recipients.filter((x) => !x.delivered);
    const delivered = r.recipients.length - refused.length;
    if (refused.length === 0 && delivered > 0) return [];
    const reason = delivered > 0 ? `Partly delivered: ${refusedList(refused)}` : `Failed: ${r.status_line}`;
    const id = draftOf(r.selector);
    return [{ key: `${account}#draft:${id}`, label: id, reason }];
  });
  const tone = outcome.failed > 0 ? "failed" : rows.length > 0 ? "partial" : "sent";
  return { result: { tone, text: `Sent ${outcome.sent}, failed ${outcome.failed}`, sticky: false }, rows };
}

/** Why `send_draft` did not start: the daemon's refusal, or where the file does not parse. */
export function sendRefusalText(message: string, invalid: DraftInvalid | null): string {
  if (!invalid) return `Send failed: ${message}`;
  const why = invalid.diagnostics.map((d) => (d.line !== null ? `line ${d.line}: ${d.message}` : d.message)).join("; ");
  return `Send failed: the draft does not parse: ${why} (${invalid.path})`;
}

/** Re-read the Drafts list and the counts of `account`. */
function staleDrafts(s: AppState, account: string): AppState {
  const next = staleCounts(s, account);
  return staleList(next, new Set([listKey(account, slugByRole(s, account, "drafts") ?? "drafts")]));
}

function runLabel(run: SendRun): string {
  return run.subject ? `"${run.subject}"` : run.kind === "approved" ? "the approved drafts" : "the draft";
}

/**
 * A send of this window ended. Its hold card, when it still shows one,
 * takes the outcome; otherwise a notice says it: sent and cancelled as
 * applied notices, a failure or a partial delivery as an alert. A dropped
 * send (the daemon restarted) is always an alert, and its drafts are
 * re-read, since only the outbox knows whether it went.
 */
function sendEnded(s: AppState, run: SendRun, end: OperationEnd): AppState {
  let next: AppState = { ...s, sends: s.sends.filter((r) => r !== run) };
  if ("dropped" in end) {
    next = staleDrafts(next, run.account);
    return pushNotice(next, { kind: "send_failed", account: run.account, text: SEND_INTERRUPTED });
  }
  let result: SendResult;
  let rows: ActivityNotice["rows"] = [];
  if (end.state === "cancelled") {
    result = { tone: "cancelled", text: "Send cancelled", sticky: false };
  } else if (end.state === "failed") {
    result = { tone: "failed", text: `Failed: ${end.error ?? "the send failed"}`, sticky: true };
  } else if (run.kind === "draft") {
    result = sendResult(end.result as SendOutcome);
  } else {
    ({ result, rows } = approvedResult(end.result as ApprovedOutcome, run.account));
  }
  // The send moved the drafts: the answer may predate it, so read again.
  next = staleDrafts(next, run.account);
  const hold = run.operation_id ? next.holds[run.operation_id] : undefined;
  if (hold) {
    next = { ...next, holds: { ...next.holds, [hold.operation_id]: { ...hold, outcome: result, cancelling: false } } };
    if (rows.length > 0) next = pushNotice(next, { kind: "send_failed", account: run.account, text: result.text, rows });
    return next;
  }
  // A held send's cancel already said so on its card.
  if (result.tone === "cancelled" && run.held) return next;
  const kind = result.tone === "failed" ? "send_failed" : result.tone === "partial" ? "send_partial" : "applied";
  const text = result.tone === "cancelled" ? `Send of ${runLabel(run)} cancelled` : result.text;
  return pushNotice(next, { kind, account: run.account, text, rows });
}

/**
 * An operation ended. A send this window awaits settles; while a send is
 * unanswered, an end of an unknown id is held for it.
 */
export function sendSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.sends.find((r) => r.operation_id === end.operation_id);
  if (run) return sendEnded(s, run, end);
  if (s.sends.some((r) => r.operation_id === null)) return { ...s, sendEarly: [...s.sendEarly, end].slice(-SEND_EARLY_CAP) };
  return s;
}

export function isSendOperation(s: AppState, operationId: string): boolean {
  return s.sends.some((r) => r.operation_id === operationId);
}

/** Is a `send_draft` or `send_approved` unanswered? */
export function sendStarting(s: AppState): boolean {
  return s.sends.some((r) => r.operation_id === null);
}

/** The confirm ran: the drafts are sending from now on. */
export function sendRequested(s: AppState, run: Omit<SendRun, "operation_id" | "held">): AppState {
  return { ...s, sends: [...s.sends, { ...run, operation_id: null, held: null }] };
}

/** The command answered: await the id, or settle it now if its end came first. */
export function sendStarted(s: AppState, token: number, operationId: string, held: boolean): AppState {
  const run = s.sends.find((r) => r.token === token);
  if (!run) return s;
  const started: SendRun = { ...run, operation_id: operationId, held };
  let next: AppState = { ...s, sends: s.sends.map((r) => (r === run ? started : r)) };
  const early = next.sendEarly.find((e) => e.operation_id === operationId);
  const waiting = sendStarting(next);
  next = { ...next, sendEarly: waiting ? next.sendEarly.filter((e) => e !== early) : [] };
  return early ? sendEnded(next, started, early) : next;
}

/** The command was refused: nothing was sent, and the drafts are free again. */
export function sendStartFailed(s: AppState, token: number, message: string, invalid: DraftInvalid | null): AppState {
  const run = s.sends.find((r) => r.token === token);
  if (!run) return s;
  let next: AppState = { ...s, sends: s.sends.filter((r) => r !== run) };
  if (!sendStarting(next)) next = { ...next, sendEarly: [] };
  // An approve may have gone through before the send was refused.
  next = staleDrafts(next, run.account);
  return pushNotice(next, { kind: "send_failed", account: run.account, text: sendRefusalText(message, invalid) });
}
