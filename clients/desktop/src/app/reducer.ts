// The one reducer: GuiEvents, fetched answers and user intents.

import type {
  AccountStateChangedPayload,
  Bootstrap,
  DraftInvalid,
  HoldStatus,
  MutationsRolledBackPayload,
  OperationProgressPayload,
  Progress,
  StateInvalidatePayload,
  StateRemovePayload,
  SyncCompletedPayload,
} from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  FetchOutcome,
  GuiError,
  GuiEvent,
  InterceptedUrl,
  LocalSearchHit,
  MailboxListing,
  MessageList,
  MessageMeta,
  MovedTo,
  SyncMode,
  VersionInfo,
} from "@/lib/gui-types";
import {
  finishedSignal,
  hitFetched,
  isOpenable,
  localHit,
  serverHitSignal,
  serverStarted,
  settledSignal,
  signal,
  startSearch,
} from "@/app/search";
import {
  applyMutation,
  dismissHold,
  dropListAnswer,
  holdCancelAnswered,
  holdCancelFailed,
  holdCancelRequested,
  holdEvent,
  isSendOperation,
  isSyncOperation,
  leaves,
  listAnswerIsStale,
  overlayPending,
  rolledBack,
  seedHolds,
  settledEnd,
  settleMutation,
  pushNotice,
  sendRequested,
  sendSignal,
  sendStarted,
  sendStartFailed,
  sendStarting,
  syncRequested,
  syncSignal,
  syncStarted,
  syncStartFailed,
  type Refusal,
} from "@/app/pending";
import {
  accountNames,
  draftItems,
  emptyLoadable,
  emptyReader,
  filteredDrafts,
  filteredRows,
  isStale,
  LIST_WIDTH_MAX,
  LIST_WIDTH_MIN,
  listKey,
  mailboxSlugs,
  markStale,
  NO_MARKS,
  readerKey,
  targetKey,
  type AppState,
  type ActivityKind,
  type AttachmentDialog,
  type ComposeDialog,
  type Layout,
  type Loadable,
  type MessageRef,
  type MutationDialog,
  type MutationKind,
  type Overlay,
  type Pane,
  type Selection,
  type Target,
  type View,
} from "@/app/state";
import { viewPanes } from "@/app/views";
import {
  closeOutbox,
  isOutboxOperation,
  moveOutboxCursor,
  openOutbox,
  outboxActionFailed,
  outboxActionRequested,
  outboxDiscarded,
  outboxFailed,
  outboxLoaded,
  outboxRetryStarted,
  outboxSignal,
  retryStarting,
  selectOutboxRow,
  staleAllOutboxes,
  staleOutbox,
} from "@/app/outbox";
import type { AgendaEvent, OutboxListing } from "@/protocol/types";
import {
  calendarFailed,
  calendarLoaded,
  dropCalendar,
  followCalendar,
  moveCalendarCursor,
  refreshCalendar,
  selectCalendarRow,
  staleAllCalendars,
  staleCalendar,
  toggleCalendarPast,
} from "@/app/calendar";

export type Action =
  | { type: "gui_event"; event: GuiEvent }
  | { type: "connection_status"; status: ConnectionStatus }
  | { type: "version_info"; info: VersionInfo }
  | { type: "accounts_loaded"; gen: number; accounts: AccountInfo[] }
  | { type: "accounts_failed"; gen: number; error: GuiError }
  | { type: "mailboxes_loaded"; account: string; gen: number; listing: MailboxListing }
  | { type: "mailboxes_failed"; account: string; gen: number; error: GuiError }
  /**
   * `lgen` is `listGen[key]` when the list was requested; an answer from
   * before the list's last optimistic change is dropped. Absent, the answer
   * is taken as current.
   */
  | { type: "messages_loaded"; key: string; gen: number; lgen?: number; list: MessageList }
  | { type: "messages_failed"; key: string; gen: number; error: GuiError }
  | { type: "reader_loaded"; key: string; gen: number; meta: MessageMeta }
  | { type: "reader_failed"; key: string; gen: number; error: GuiError }
  | { type: "select_account"; account: string }
  | { type: "select_mailbox"; account: string; slug: string; focus?: Pane }
  | { type: "select_message"; message: Omit<MessageRef, "verified">; focus?: Pane }
  | { type: "select_draft"; id: string; focus?: Pane }
  /** A server-only search hit, by its key: it has no row to open. */
  | { type: "select_hit"; key: string; focus?: Pane }
  | { type: "move_selection"; to: number | "first" | "last"; relative: boolean }
  | { type: "move_sidebar_cursor"; delta: number }
  | { type: "sidebar_enter" }
  | { type: "jump_mailbox"; index: number }
  | { type: "next_account" }
  | { type: "focus"; pane: Pane }
  | { type: "pane_focused"; pane: Pane }
  | { type: "cycle_focus"; dir: 1 | -1 }
  | { type: "back" }
  | { type: "up" }
  | { type: "clear_selection" }
  | { type: "toggle_zoom" }
  | { type: "toggle_sidebar" }
  | { type: "set_sidebar_open"; open: boolean }
  | { type: "set_list_width"; px: number }
  | { type: "set_layout"; layout: Layout }
  /** Show a view (app/views.ts); focus goes to its pane. */
  | { type: "switch_view"; view: View }
  | { type: "overlay"; overlay: Overlay }
  | { type: "open_dialog"; dialog: MutationDialog }
  | { type: "open_compose"; dialog: ComposeDialog }
  | { type: "open_attachments"; dialog: AttachmentDialog }
  /** A save went to `dir`, which the next Save dialog offers. */
  | { type: "save_dir"; dir: string }
  // `f` on a server-only hit (app/attachments.ts dispatches these around `message_fetch`).
  | { type: "hit_fetch_started"; key: string }
  | { type: "hit_fetched"; key: string; outcome: FetchOutcome }
  | { type: "hit_fetch_failed"; key: string; error: GuiError }
  // The external editor sessions (app/compose.ts dispatches these around `editor_open`).
  | { type: "compose_opening"; account: string; draftId: string; path: string }
  | { type: "compose_editing"; account: string; draftId: string; editor: string }
  | { type: "compose_failed"; account: string; draftId: string; error: GuiError }
  | { type: "compose_done"; account: string; draftId: string }
  /** A notice of the activity area from outside a mutation batch. */
  | { type: "activity"; kind: ActivityKind; account: string | null; text: string; rows?: { key: string; label: string; reason: string }[] }
  | { type: "filter"; text: string }
  | { type: "notice"; text: string | null }
  | { type: "error"; error: GuiError | null }
  | { type: "search_local"; query: string }
  | { type: "search_local_loaded"; seq: number; hits: LocalSearchHit[] }
  | { type: "search_local_failed"; seq: number; error: GuiError }
  | { type: "search_server"; query: string }
  | { type: "search_server_started"; seq: number; operation_id: string }
  | { type: "search_server_failed"; seq: number; error: GuiError }
  | { type: "search_server_cancelled"; operation_id: string; outcome: "cancelled" | "already_settled" }
  | { type: "exit_search" }
  | { type: "intercepted_fetched"; urls: InterceptedUrl[] }
  | { type: "dismiss_intercept" }
  // Mutations (app/mutations.ts dispatches these around each command).
  | {
      type: "mutation_apply";
      batch: number;
      kind: MutationKind;
      targets: Target[];
      destination?: string | null;
      value?: boolean | null;
    }
  | {
      type: "mutation_settled";
      batch: number;
      kind: MutationKind;
      account: string;
      done: Target[];
      failed: Refusal[];
      value?: boolean | null;
      moved_to?: MovedTo | null;
    }
  | { type: "mutation_failed"; batch: number; kind: MutationKind; account: string; targets: Target[]; error: GuiError }
  | { type: "hold_cancel_requested"; operation_id: string }
  | { type: "hold_cancel_answered"; operation_id: string; cancelled: boolean }
  | { type: "hold_cancel_failed"; operation_id: string; error: GuiError }
  | { type: "dismiss_hold"; operation_id: string }
  | { type: "sync_requested" }
  | { type: "sync_started"; operation_id: string; account: string; mode: SyncMode }
  | { type: "sync_failed"; account: string; error: GuiError }
  // A send this window started (app/mutations.ts dispatches these around `send_draft` and `send_approved`).
  | { type: "send_requested"; token: number; kind: "draft" | "approved"; account: string; drafts: string[]; subject: string | null }
  | { type: "send_started"; token: number; operation_id: string; held: boolean }
  | { type: "send_failed"; token: number; error: GuiError; invalid: DraftInvalid | null }
  | { type: "dismiss_notice"; id: number }
  | { type: "dismiss_all_notices" }
  /** One `operation.progress` of an operation this window awaits (the Rust layer drops the others). */
  | { type: "operation_progress"; operation_id: string; progress: Progress }
  // The outbox (app/outbox.ts; app/mutations.ts dispatches the row actions around their commands).
  | { type: "open_outbox"; account: string }
  | { type: "close_outbox" }
  | { type: "outbox_loaded"; account: string; gen: number; listing: OutboxListing }
  | { type: "outbox_failed"; account: string; gen: number; error: GuiError }
  | { type: "outbox_select"; row_id: number }
  | { type: "outbox_action_requested"; token: number; kind: "retry" | "discard"; account: string; row_id: number }
  | { type: "outbox_retry_started"; token: number; operation_id: string }
  | { type: "outbox_discarded"; token: number; message_id: string }
  | { type: "outbox_action_failed"; token: number; error: GuiError }
  // The Calendar view (app/calendar.ts).
  | { type: "calendar_loaded"; account: string; gen: number; events: AgendaEvent[] }
  | { type: "calendar_failed"; account: string; gen: number; error: GuiError }
  | { type: "calendar_select"; row_id: number }
  | { type: "calendar_toggle_past" }
  | { type: "calendar_refresh" }
  // The list's multi-select, by `targetKey`.
  | { type: "mark_toggle"; key: string }
  | { type: "mark_set"; keys: string[]; on: boolean }
  | { type: "mark_range"; key: string }
  | { type: "mark_all" }
  | { type: "mark_clear" };

const HISTORY_CAP = 32;
const INTERCEPT_CAP = 100;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function withFocus(s: AppState, pane: Pane): AppState {
  if (s.focus === pane) return { ...s, focusSeq: s.focusSeq + 1 };
  const history = [...s.history, s.focus].slice(-HISTORY_CAP);
  return { ...s, focus: pane, history, focusSeq: s.focusSeq + 1 };
}

function defaultMailbox(s: AppState, account: string): string | null {
  const listing = s.mailboxes[account]?.data;
  const rows =
    listing?.mailboxes.map((m) => ({ slug: m.slug, role: m.role })) ??
    s.bootstrap?.snapshot.mailboxes[account]?.map((m) => ({ slug: m.slug, role: m.role })) ??
    [];
  return rows.find((r) => r.role === "inbox")?.slug ?? rows[0]?.slug ?? null;
}

function defaultAccount(s: AppState, names: string[]): string | null {
  const flagged = s.accounts.data?.find((a) => a.default && names.includes(a.name));
  return flagged?.name ?? names[0] ?? null;
}

/** Point the message list at a (possibly new) account/mailbox. */
function retarget(s: AppState, sel: Selection): AppState {
  const key = sel.account && sel.mailbox ? listKey(sel.account, sel.mailbox) : null;
  const messages = key === s.messages.key ? s.messages : { ...emptyLoadable<MessageList>(), key };
  const cursor =
    sel.account && sel.mailbox ? { account: sel.account, slug: sel.mailbox } : s.sidebarCursor;
  const same = key === s.messages.key;
  return { ...s, selection: sel, messages, sidebarCursor: cursor, filter: same ? s.filter : "", marked: same ? s.marked : NO_MARKS };
}

/**
 * The selection the mailbox list owns: the live one, or while a search shows
 * its hits, the one the search will restore.
 */
function listSelection(s: AppState): Selection {
  return s.search ? s.search.restore.selection : s.selection;
}

function setListSelection(s: AppState, sel: Selection): AppState {
  if (!s.search) return { ...s, selection: sel };
  return { ...s, search: { ...s.search, restore: { ...s.search.restore, selection: sel } } };
}

/** Leave the search: the mailbox list and its selection come back. */
function endSearch(s: AppState): AppState {
  const search = s.search;
  if (!search) return s;
  let next: AppState = { ...s, search: null, filter: "", selection: search.restore.selection, marked: NO_MARKS };
  const list = next.messages.data;
  if (next.selection.message && !next.selection.message.verified && list && !isStale(next.messages)) {
    next = reverify(next, list);
  }
  if (!next.selection.message) next = { ...next, reader: emptyReader() };
  return next;
}

function selectMailbox(s: AppState, account: string, slug: string): AppState {
  s = endSearch(s);
  if (s.selection.account === account && s.selection.mailbox === slug) {
    return { ...s, sidebarCursor: { account, slug } };
  }
  return retarget(s, { account, mailbox: slug, message: null, draft: null, hit: null });
}

type ListItem =
  | { kind: "message"; ref: Omit<MessageRef, "verified"> }
  | { kind: "draft"; id: string }
  | { kind: "hit"; key: string };

/**
 * The rows the list pane shows, after the local filter, or the search's
 * hits; a server-only hit is an item of its own, which has no row.
 */
export function visibleItems(s: AppState): ListItem[] {
  if (s.search) {
    return s.search.hits.map((h): ListItem =>
      isOpenable(h)
        ? { kind: "message", ref: { row_id: h.row_id!, message_id: h.message_id!, selector: h.selector! } }
        : { kind: "hit", key: h.key },
    );
  }
  const list = s.messages.data;
  if (list?.kind === "drafts") {
    return filteredDrafts(list, s.filter).map((d) => ({ kind: "draft", id: d.id }));
  }
  return filteredRows(list, s.filter).map((r) => ({
    kind: "message",
    ref: { row_id: r.id, message_id: r.message_id, selector: r.selector },
  }));
}

function currentIndex(s: AppState, items: ListItem[]): number {
  const sel = s.selection;
  return items.findIndex((it) =>
    it.kind === "draft"
      ? sel.draft === it.id
      : it.kind === "hit"
        ? sel.hit === it.key
        : sel.message !== null && sel.message.message_id === it.ref.message_id &&
          sel.message.selector === it.ref.selector,
  );
}

/** The account the shown list or search belongs to. */
function shownAccount(s: AppState): string {
  return s.search?.account ?? s.selection.account ?? "";
}

/** A list item's `targetKey`. */
function itemKey(account: string, it: ListItem): string {
  if (it.kind === "hit") return `${account}#hit:${it.key}`;
  return it.kind === "draft" ? targetKey({ account, draft: it.id }) : targetKey({ account, row_id: it.ref.row_id });
}

/** What a mutation names an item by; a server-only hit has nothing to name. */
function itemTarget(account: string, it: ListItem): Target | null {
  if (it.kind === "hit") return null;
  return it.kind === "draft" ? { account, draft: it.id } : { account, row_id: it.ref.row_id };
}

/**
 * What a mutation acts on: the marked rows in list order, or else the
 * selected one. A mark the list no longer shows is not acted on.
 */
export function actionTargets(s: AppState): Target[] {
  const account = shownAccount(s);
  const items = visibleItems(s);
  const targets = (its: ListItem[]) => its.flatMap((it) => itemTarget(account, it) ?? []);
  if (s.marked.keys.size > 0) return targets(items.filter((it) => s.marked.keys.has(itemKey(account, it))));
  const cur = currentIndex(s, items);
  return cur >= 0 ? targets([items[cur]]) : [];
}

/**
 * Where the cursor goes when the rows it sits on leave the list: the next
 * row that stays, else the previous one, else nowhere (the TUI's rule).
 */
function cursorAfterLeave(s: AppState, before: ListItem[], gone: ReadonlySet<string>): AppState {
  const account = shownAccount(s);
  const cur = currentIndex(s, before);
  const m = s.selection.message;
  const hidden = cur < 0 && m !== null && gone.has(targetKey({ account, row_id: m.row_id }));
  if (!hidden && (cur < 0 || !gone.has(itemKey(account, before[cur])))) return s;
  const stays = (it: ListItem) => !gone.has(itemKey(account, it));
  const to = cur < 0 ? undefined : (before.slice(cur + 1).find(stays) ?? before.slice(0, cur).reverse().find(stays));
  const next: AppState = to
    ? selectItem(s, to)
    : { ...s, selection: { ...s.selection, message: null, draft: null, hit: null }, reader: emptyReader() };
  return { ...next, focusSeq: next.focusSeq + 1 };
}

/** Drop a list's marks that its reloaded rows no longer have. */
function pruneMarks(s: AppState, list: MessageList): AppState {
  if (s.marked.keys.size === 0 || s.search) return s;
  const present = new Set(
    list.kind === "drafts"
      ? draftItems(list).map((d) => targetKey({ account: list.account, draft: d.id }))
      : list.rows.map((r) => targetKey({ account: list.account, row_id: r.id })),
  );
  const keys = new Set([...s.marked.keys].filter((k) => present.has(k)));
  if (keys.size === s.marked.keys.size) return s;
  const anchor = s.marked.anchor !== null && present.has(s.marked.anchor) ? s.marked.anchor : null;
  return { ...s, marked: { keys, anchor } };
}

function marks(s: AppState, a: Extract<Action, { type: `mark_${string}` }>): AppState {
  const account = shownAccount(s);
  const keys = new Set(s.marked.keys);
  switch (a.type) {
    case "mark_toggle":
      if (!keys.delete(a.key)) keys.add(a.key);
      return { ...s, marked: { keys, anchor: a.key } };
    case "mark_set":
      for (const k of a.keys) {
        if (a.on) keys.add(k);
        else keys.delete(k);
      }
      return { ...s, marked: { keys, anchor: a.on && a.keys.length > 0 ? a.keys[a.keys.length - 1] : s.marked.anchor } };
    case "mark_range": {
      // From the anchor (else the cursor) to the key, both included, added
      // to what is marked; the anchor stays for the next range.
      const items = visibleItems(s);
      const order = items.map((it) => itemKey(account, it));
      const cur = currentIndex(s, items);
      const anchor = s.marked.anchor ?? (cur >= 0 ? order[cur] : a.key);
      const i = order.indexOf(anchor);
      const j = order.indexOf(a.key);
      if (i < 0 || j < 0) {
        keys.add(a.key);
        return { ...s, marked: { keys, anchor: a.key } };
      }
      for (const k of order.slice(Math.min(i, j), Math.max(i, j) + 1)) keys.add(k);
      return { ...s, marked: { keys, anchor } };
    }
    case "mark_all":
      // A server-only hit cannot be acted on, so it is not marked.
      for (const it of visibleItems(s)) if (it.kind !== "hit") keys.add(itemKey(account, it));
      return { ...s, marked: { keys, anchor: s.marked.anchor } };
    case "mark_clear":
      return s.marked.keys.size === 0 && s.marked.anchor === null ? s : { ...s, marked: NO_MARKS };
  }
}

function selectItem(s: AppState, it: ListItem): AppState {
  if (it.kind === "draft") {
    return { ...s, selection: { ...s.selection, message: null, hit: null, draft: it.id } };
  }
  if (it.kind === "hit") {
    return { ...s, selection: { ...s.selection, message: null, draft: null, hit: it.key }, reader: emptyReader() };
  }
  return { ...s, selection: { ...s.selection, draft: null, hit: null, message: { ...it.ref, verified: true } } };
}

/** Flattened sidebar entries, for the cursor. */
function sidebarEntries(s: AppState): { account: string; slug: string }[] {
  return accountNames(s).flatMap((account) =>
    mailboxSlugs(s, account).map((slug) => ({ account, slug })),
  );
}

function parseResource(resource: string): { family: string; parts: string[] } {
  const i = resource.indexOf(":");
  if (i < 0) return { family: resource, parts: [] };
  return { family: resource.slice(0, i), parts: resource.slice(i + 1).split("/") };
}

function staleMailboxes(s: AppState, account: string): AppState {
  const l = s.mailboxes[account];
  if (!l) return s;
  return { ...s, mailboxes: { ...s.mailboxes, [account]: markStale(l) } };
}

function staleListIf(s: AppState, match: (account: string, mailbox: string) => boolean): AppState {
  const sel = listSelection(s);
  const { account, mailbox } = sel;
  if (!account || !mailbox || !match(account, mailbox)) return s;
  // A listing reload re-verifies the list's selected message by message_id;
  // an open search hit is the search's, not the list's, and stays open.
  const message = sel.message ? { ...sel.message, verified: false } : null;
  return setListSelection({ ...s, messages: { ...markStale(s.messages), key: s.messages.key } }, { ...sel, message });
}

function patchAccount(s: AppState, name: string, patch: Partial<AccountInfo>): AppState {
  if (!s.accounts.data) return s;
  return {
    ...s,
    accounts: {
      ...s.accounts,
      data: s.accounts.data.map((a) => (a.name === name ? { ...a, ...patch } : a)),
    },
  };
}

function removeAccount(s: AppState, name: string): AppState {
  if (s.search?.account === name) s = endSearch(s);
  if (s.outboxView?.account === name) s = closeOutbox(s);
  s = dropCalendar(s, name);
  const mailboxes = { ...s.mailboxes };
  delete mailboxes[name];
  let next: AppState = {
    ...s,
    mailboxes,
    accounts: s.accounts.data
      ? { ...s.accounts, data: s.accounts.data.filter((a) => a.name !== name) }
      : s.accounts,
    bootstrap: s.bootstrap
      ? {
          ...s.bootstrap,
          snapshot: {
            ...s.bootstrap.snapshot,
            accounts: s.bootstrap.snapshot.accounts.filter((a) => a.name !== name),
          },
        }
      : null,
  };
  if (next.selection.account === name) {
    const account = defaultAccount(next, accountNames(next));
    const mailbox = account ? defaultMailbox(next, account) : null;
    next = retarget(next, { account, mailbox, message: null, draft: null, hit: null });
  }
  return next;
}

/** The file name of a path, what the editing banner calls a draft. */
export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function composeOpening(s: AppState, account: string, draftId: string, path: string): AppState {
  const key = targetKey({ account, draft: draftId });
  const session = { account, draftId, path, name: fileName(path), editor: s.compose[key]?.editor ?? null, status: "opening" as const, message: null };
  return { ...s, compose: { ...s.compose, [key]: session } };
}

function composeUpdate(s: AppState, account: string, draftId: string, patch: Partial<AppState["compose"][string]>): AppState {
  const key = targetKey({ account, draft: draftId });
  const prev = s.compose[key];
  if (!prev) return s;
  return { ...s, compose: { ...s.compose, [key]: { ...prev, ...patch } } };
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/**
 * A fresh bootstrap replaces the model: selection survives by stable
 * identifier (account name, mailbox slug, message_id), and a reference to a
 * resource the snapshot no longer has is cleared.
 */
function bootstrapModel(s: AppState, bootstrap: Bootstrap): AppState {
  const names = bootstrap.snapshot.accounts.map((a) => a.name);
  const mailboxes: AppState["mailboxes"] = {};
  for (const name of names) {
    const prev = s.mailboxes[name];
    if (prev) mailboxes[name] = markStale(prev);
  }
  let next: AppState = {
    ...s,
    bootstrap,
    resync: null,
    disconnected: null,
    shuttingDown: false,
    mailboxes,
    accounts: s.accounts.data
      ? markStale({ ...s.accounts, data: s.accounts.data.filter((a) => names.includes(a.name)) })
      : markStale(s.accounts),
  };

  const prev = s.selection;
  const kept = prev.account !== null && names.includes(prev.account);
  const account = kept ? prev.account : defaultAccount(next, names);
  next = { ...next, selectionAuto: kept ? s.selectionAuto : account !== null };
  const slugs = account ? (bootstrap.snapshot.mailboxes[account] ?? []).map((m) => m.slug) : [];
  const keepMailbox = account === prev.account && prev.mailbox !== null && slugs.includes(prev.mailbox);
  const mailbox = keepMailbox ? prev.mailbox : account ? defaultMailbox(next, account) : null;
  const same = keepMailbox && account === prev.account;
  const message = same && prev.message ? { ...prev.message, verified: false } : null;
  const draft = same ? prev.draft : null;

  next = retarget(next, { account, mailbox, message, draft, hit: null });
  if (same) next = { ...next, messages: { ...markStale(next.messages), key: next.messages.key } };
  // The reader refetches once the list re-verifies the row.
  next = { ...next, reader: { ...next.reader, load: markStale(next.reader.load) } };
  if (!message) next = { ...next, reader: emptyReader() };
  const cursor = next.sidebarCursor;
  if (cursor && !(names.includes(cursor.account) && (bootstrap.snapshot.mailboxes[cursor.account] ?? []).some((m) => m.slug === cursor.slug))) {
    next = { ...next, sidebarCursor: account && mailbox ? { account, slug: mailbox } : null };
  }
  return next;
}

/**
 * A fresh bootstrap replaces the model: selection survives by stable
 * identifier (account name, mailbox slug, message_id), and a reference to a
 * resource the snapshot no longer has is cleared. A search survives with its
 * hits: the list selection it restores goes through the same rules, a local
 * search runs again (row ids are per daemon instance), and a server search
 * waits for the `operation_settled` or `operation_dropped` the Rust layer's
 * re-query sends.
 */
export function applyBootstrap(s: AppState, bootstrap: Bootstrap): AppState {
  const search = s.search;
  if (!search) return bootstrapModel(s, bootstrap);
  const names = bootstrap.snapshot.accounts.map((a) => a.name);
  const base = bootstrapModel({ ...s, search: null, selection: search.restore.selection }, bootstrap);
  if (!names.includes(search.account)) return base;
  const sameInstance = s.bootstrap?.instance_id === bootstrap.instance_id;
  const hit = sameInstance ? s.selection.message : null;
  const serverHit = sameInstance ? s.selection.hit : null;
  const rerun = search.mode === "local" && !sameInstance;
  return {
    ...base,
    search: {
      ...search,
      restore: { ...search.restore, selection: base.selection },
      ...(rerun ? { status: "searching" as const, seq: search.seq + 1, error: null } : {}),
    },
    selection: { ...base.selection, message: hit, draft: null, hit: serverHit },
    reader: hit ? { ...s.reader, load: markStale(s.reader.load) } : emptyReader(),
  };
}

/** An `operation.progress` payload as the typed action, or null when it is not one. */
export function progressAction(payload: unknown): Extract<Action, { type: "operation_progress" }> | null {
  const p = payload as Partial<OperationProgressPayload> | null;
  if (!p || typeof p.operation_id !== "string" || typeof p.phase !== "string" || typeof p.done !== "number") return null;
  const total = typeof p.total === "number" ? p.total : null;
  const message = typeof p.message === "string" ? p.message : null;
  return { type: "operation_progress", operation_id: p.operation_id, progress: { phase: p.phase, done: p.done, total, message } };
}

/** The operation ended: its last report goes. */
function endProgress(s: AppState, operationId: string): AppState {
  if (!(operationId in s.progress)) return s;
  const progress = { ...s.progress };
  delete progress[operationId];
  return { ...s, progress };
}

function applyEnvelope(s: AppState, kind: string, payload: unknown): AppState {
  switch (kind) {
    case "state.invalidate": {
      const { resource } = payload as StateInvalidatePayload;
      const { family, parts } = parseResource(resource);
      const account = parts[0] ?? "";
      // No resource names the agenda: it is a fold over the account's mail.
      if (family === "mailbox" || family === "message") s = staleCalendar(s, account);
      if (family === "mailbox") {
        const slug = parts[1] ?? "";
        return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === slug);
      }
      if (family === "outbox") return { ...staleOutbox(s, account), accounts: markStale(s.accounts) };
      if (family === "draft") {
        return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === "drafts");
      }
      if (family === "message") {
        const slug = parts[1] ?? "";
        return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === slug);
      }
      return s;
    }
    case "state.remove": {
      const { resource } = payload as StateRemovePayload;
      const { family, parts } = parseResource(resource);
      const account = parts[0] ?? "";
      if (family === "mailbox" || family === "message") s = staleCalendar(s, account);
      if (family === "account") return removeAccount(s, account);
      if (family === "mailbox") {
        const slug = parts[1] ?? "";
        const sel = listSelection(s);
        let next = staleMailboxes(sel.account === account && sel.mailbox === slug ? endSearch(s) : s, account);
        if (next.selection.account === account && next.selection.mailbox === slug) {
          const mailbox = next.mailboxes[account]?.data?.mailboxes.find((m) => m.slug !== slug && m.role === "inbox")?.slug ?? null;
          next = retarget(next, { account, mailbox, message: null, draft: null, hit: null });
          next = { ...next, reader: emptyReader() };
        }
        return next;
      }
      if (family === "draft") {
        const id = parts[1] ?? "";
        let next = staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === "drafts");
        if (next.selection.account === account && next.selection.draft === id) {
          next = { ...next, selection: { ...next.selection, draft: null } };
        }
        // The file is gone: nothing is left to edit.
        const key = targetKey({ account, draft: id });
        if (key in next.compose) {
          const compose = { ...next.compose };
          delete compose[key];
          next = { ...next, compose };
        }
        return next;
      }
      if (family === "message") {
        const slug = parts[1] ?? "";
        return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === slug);
      }
      return s;
    }
    case "account.state_changed": {
      const p = payload as AccountStateChangedPayload;
      const next = staleMailboxes(patchAccount(s, p.account, { runtime_state: p.state }), p.account);
      return staleListIf(next, (a) => a === p.account);
    }
    case "sync.completed": {
      const p = payload as SyncCompletedPayload;
      const health = p.error === null ? "ok" : "failed";
      const next = staleMailboxes(patchAccount(staleCalendar(s, p.account), p.account, { sync_health: health }), p.account);
      return staleListIf(next, (a) => a === p.account);
    }
    case "draft.changed":
    case "draft.invalid": {
      const account = (payload as { account: string }).account;
      return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === "drafts");
    }
    case "mutations.rolled_back": {
      const p = payload as MutationsRolledBackPayload;
      return staleListIf(staleMailboxes(rolledBack(s, p), p.account), (a) => a === p.account);
    }
    case "send.hold_started":
    case "send.hold_tick":
    case "send.hold_cancelled":
    case "send.hold_fired":
      return holdEvent(s, kind, payload as HoldStatus);
    case "daemon.shutting_down":
      return { ...s, shuttingDown: true };
    case "message.server_hit":
      return signal(s, serverHitSignal(payload));
    case "operation.progress": {
      const a = progressAction(payload);
      return a ? reducer(s, a) : s;
    }
    case "operation.finished": {
      const sig = finishedSignal(payload);
      if (sig.kind !== "finish") return s;
      s = endProgress(s, sig.operation_id);
      const end = { operation_id: sig.operation_id, state: sig.state, error: sig.error, result: sig.result };
      if (isSyncOperation(s, sig.operation_id)) return syncSignal(s, end);
      if (isSendOperation(s, sig.operation_id)) return sendSignal(s, end);
      if (isOutboxOperation(s, sig.operation_id)) return outboxSignal(s, end);
      // An unknown id may be a sync, a send or a retry whose start has not
      // answered yet, or the search's: each holds it until its id is known.
      let next = s.syncStarting > 0 ? syncSignal(s, end) : s;
      if (sendStarting(next)) next = sendSignal(next, end);
      if (retryStarting(next)) next = outboxSignal(next, end);
      return signal(next, sig);
    }
    default:
      return s;
  }
}

export function applyGuiEvent(s: AppState, e: GuiEvent): AppState {
  switch (e.type) {
    case "connection":
      return { ...s, connection: e.status };
    case "disconnected":
      return { ...s, disconnected: e.reason };
    case "reconnected":
      // The rebootstrapped that follows clears the banner; until then the
      // model is known to be behind.
      return { ...s, disconnected: null, resync: s.resync ?? "reconnected" };
    case "resync":
      return { ...s, resync: e.reason };
    case "rebootstrapped": {
      // Row ids are per daemon instance, and the reloaded lists are the
      // truth: nothing stays pending, and marks survive only the same instance.
      const sameInstance = s.bootstrap?.instance_id === e.bootstrap.instance_id;
      const next = staleAllCalendars(staleAllOutboxes(applyBootstrap(s, e.bootstrap)));
      // A confirmation or a picker names rows by id: another instance closes
      // it, and the forward wizard too; a draft keeps its id and file.
      const closeDialog = !sameInstance && next.dialog !== null;
      const closeCompose = !sameInstance && next.composeDialog?.kind === "forward";
      const closeOverlay = (closeDialog && next.overlay === "mutation") || (closeCompose && next.overlay === "compose");
      // The same daemon keeps the cards of holds that ended, so a settle
      // still finds the card it reports on; the snapshot's are the live ones.
      const ended = sameInstance ? Object.fromEntries(Object.entries(s.holds).filter(([, h]) => h.state === "fired" || h.state === "cancelled")) : {};
      return {
        ...next,
        pending: {},
        holds: { ...ended, ...seedHolds(e.bootstrap.snapshot.holds) },
        marked: sameInstance ? next.marked : NO_MARKS,
        // Operation ids are per daemon instance.
        progress: sameInstance ? next.progress : {},
        dialog: closeDialog ? null : next.dialog,
        composeDialog: closeCompose ? null : next.composeDialog,
        overlay: closeOverlay ? null : next.overlay,
      };
    }
    case "event":
      if (s.bootstrap && e.event.instance_id !== s.bootstrap.instance_id) return s;
      return applyEnvelope(s, e.event.kind, e.event.payload);
    case "link_intercepted":
      return {
        ...s,
        intercepted: [...s.intercepted, e.url].slice(-INTERCEPT_CAP),
        interceptNotice: e.url.source === "open_external_stub" ? s.interceptNotice : e.url,
      };
    case "operation_settled":
      s = endProgress(s, e.operation_id);
      if (e.kind === "sync") return syncSignal(s, settledEnd(e.operation_id, e.status));
      if (e.kind === "send" || e.kind === "send_approved") return sendSignal(s, settledEnd(e.operation_id, e.status));
      if (e.kind === "outbox_retry") return outboxSignal(s, settledEnd(e.operation_id, e.status));
      return signal(s, settledSignal(e.operation_id, e.status));
    case "operation_dropped":
      s = endProgress(s, e.operation_id);
      if (e.kind === "sync") return syncSignal(s, { operation_id: e.operation_id, dropped: e.reason });
      if (e.kind === "send" || e.kind === "send_approved") return sendSignal(s, { operation_id: e.operation_id, dropped: e.reason });
      if (e.kind === "outbox_retry") return outboxSignal(s, { operation_id: e.operation_id, dropped: e.reason });
      return signal(s, { kind: "dropped", operation_id: e.operation_id, reason: e.reason });
  }
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

function loaded<T>(l: Loadable<T>, gen: number, data: T): Loadable<T> {
  return { ...l, data, loadedGen: gen, error: null };
}

function failed<T>(l: Loadable<T>, gen: number, error: GuiError): Loadable<T> {
  return { ...l, loadedGen: gen, error };
}

/** After a listing reload, confirm the list's selected message by message_id or drop it. */
function reverify(s: AppState, list: MessageList): AppState {
  const sel = listSelection(s);
  if (list.kind === "drafts") {
    if (sel.draft && !draftItems(list).some((d) => d.id === sel.draft)) {
      return setListSelection(s, { ...sel, draft: null });
    }
    return s;
  }
  if (!sel.message || sel.message.verified) return s;
  const want = sel.message;
  const row =
    list.rows.find((r) => r.message_id === want.message_id && r.selector === want.selector) ??
    list.rows.find((r) => r.selector === want.selector);
  if (!row) {
    const next = setListSelection(s, { ...sel, message: null });
    return s.search ? next : { ...next, reader: emptyReader() };
  }
  return setListSelection(s, { ...sel, message: { ...want, row_id: row.id, verified: true } });
}

// ---------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------

/** The intents by which the user chooses what is selected. */
const USER_SELECTION: ReadonlySet<Action["type"]> = new Set<Action["type"]>([
  "select_account",
  "select_mailbox",
  "select_message",
  "select_draft",
  "move_selection",
  "sidebar_enter",
  "jump_mailbox",
  "next_account",
  "clear_selection",
  "search_local",
  "search_server",
  "exit_search",
]);

/**
 * The intents that bring Mail back over a full-pane view: each picks a
 * mailbox, searches it or opens an outbox. Another account keeps the view,
 * which follows the selection's account.
 */
const LEAVES_VIEW: ReadonlySet<Action["type"]> = new Set<Action["type"]>([
  "select_mailbox",
  "sidebar_enter",
  "jump_mailbox",
  "search_local",
  "search_server",
  "open_outbox",
]);

/** A full-pane view is left for Mail: the list pane shows what it showed before. */
function toMail(s: AppState): AppState {
  return s.view === "mail" ? s : { ...s, view: "mail" };
}

/** The intents that bring the mailbox list back over the outbox view. */
const LEAVES_OUTBOX: ReadonlySet<Action["type"]> = new Set<Action["type"]>([
  "select_account",
  "select_mailbox",
  "sidebar_enter",
  "jump_mailbox",
  "search_local",
  "search_server",
]);

/** The reducer; the Calendar view follows the selection's account after every action. */
export function reducer(s: AppState, a: Action): AppState {
  return followCalendar(reduce(s, a));
}

function reduce(s: AppState, a: Action): AppState {
  // A move inside a full-pane view moves the view's cursor, not the mail selection.
  const viewMove = a.type === "move_selection" && s.view !== "mail";
  if (s.selectionAuto && USER_SELECTION.has(a.type) && !viewMove) s = { ...s, selectionAuto: false };
  if (s.view !== "mail" && LEAVES_VIEW.has(a.type)) s = toMail(s);
  if (s.outboxView && LEAVES_OUTBOX.has(a.type)) s = closeOutbox(s);
  switch (a.type) {
    case "gui_event":
      return applyGuiEvent(s, a.event);
    case "connection_status":
      return { ...s, connection: a.status };
    case "version_info":
      return { ...s, version: a.info };

    case "accounts_loaded": {
      let next: AppState = { ...s, accounts: loaded(s.accounts, a.gen, a.accounts) };
      // The answer names the default account; honour it while the selection
      // is still the one a bootstrap picked (the snapshot's first name), and
      // never over a choice the user made, even one made before this answer
      // or between a failed fetch and its retry.
      if (next.selectionAuto) {
        const def = a.accounts.find((x) => x.default)?.name;
        if (def && def !== next.selection.account) {
          const mailbox = defaultMailbox(next, def);
          next = retarget(next, { account: def, mailbox, message: null, draft: null, hit: null });
        }
      }
      return next;
    }
    case "accounts_failed":
      return { ...s, accounts: failed(s.accounts, a.gen, a.error) };
    case "mailboxes_loaded": {
      const prev = s.mailboxes[a.account] ?? emptyLoadable<MailboxListing>();
      return { ...s, mailboxes: { ...s.mailboxes, [a.account]: loaded(prev, a.gen, a.listing) } };
    }
    case "mailboxes_failed": {
      const prev = s.mailboxes[a.account] ?? emptyLoadable<MailboxListing>();
      return { ...s, mailboxes: { ...s.mailboxes, [a.account]: failed(prev, a.gen, a.error) } };
    }
    case "messages_loaded": {
      if (a.key !== s.messages.key) return s;
      if (listAnswerIsStale(s, a.key, a.lgen, a.gen)) return dropListAnswer(s, a.gen);
      const list = overlayPending(s, a.list);
      const next = { ...s, messages: { ...loaded(s.messages, a.gen, list), key: s.messages.key } };
      return pruneMarks(reverify(next, list), list);
    }
    case "messages_failed":
      if (a.key !== s.messages.key) return s;
      return { ...s, messages: { ...failed(s.messages, a.gen, a.error), key: s.messages.key } };
    case "reader_loaded": {
      const m = s.selection.message;
      if (!m || !s.selection.account || readerKey(s.selection.account, m.row_id) !== a.key) return s;
      return { ...s, reader: { key: a.key, meta: a.meta, load: loaded(s.reader.load, a.gen, true) } };
    }
    case "reader_failed": {
      const m = s.selection.message;
      if (!m || !s.selection.account || readerKey(s.selection.account, m.row_id) !== a.key) return s;
      return { ...s, reader: { ...s.reader, key: a.key, load: failed(s.reader.load, a.gen, a.error) } };
    }

    case "select_account": {
      if (a.account === s.selection.account && !s.search) return s;
      s = endSearch(s);
      if (a.account === s.selection.account) return s;
      return retarget(s, { account: a.account, mailbox: defaultMailbox(s, a.account), message: null, draft: null, hit: null });
    }
    case "select_mailbox": {
      const next = selectMailbox(s, a.account, a.slug);
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "select_message": {
      const next = { ...s, selection: { ...s.selection, draft: null, hit: null, message: { ...a.message, verified: true } } };
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "select_draft": {
      const next = { ...s, selection: { ...s.selection, message: null, hit: null, draft: a.id } };
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "select_hit": {
      if (!s.search?.hits.some((h) => h.key === a.key)) return s;
      const next = selectItem(s, { kind: "hit", key: a.key });
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "move_selection": {
      // A full-pane view moves its own cursor, once its unit gives it one.
      if (s.view === "calendar") return moveCalendarCursor(s, a.to, a.relative);
      if (s.view !== "mail") return s;
      if (s.outboxView) return moveOutboxCursor(s, a.to, a.relative);
      const items = visibleItems(s);
      if (items.length === 0) return s;
      const cur = currentIndex(s, items);
      let idx: number;
      if (a.to === "first") idx = 0;
      else if (a.to === "last") idx = items.length - 1;
      else if (a.relative) idx = cur < 0 ? (a.to > 0 ? 0 : items.length - 1) : cur + a.to;
      else idx = a.to;
      idx = Math.max(0, Math.min(items.length - 1, idx));
      const next = selectItem(s, items[idx]);
      return { ...next, focusSeq: next.focusSeq + 1 };
    }
    case "move_sidebar_cursor": {
      const entries = sidebarEntries(s);
      if (entries.length === 0) return s;
      const c = s.sidebarCursor ?? (s.selection.account && s.selection.mailbox ? { account: s.selection.account, slug: s.selection.mailbox } : null);
      const cur = c ? entries.findIndex((e) => e.account === c.account && e.slug === c.slug) : -1;
      const idx = Math.max(0, Math.min(entries.length - 1, cur < 0 ? 0 : cur + a.delta));
      return { ...s, sidebarCursor: entries[idx], focusSeq: s.focusSeq + 1 };
    }
    case "sidebar_enter": {
      const c = s.sidebarCursor;
      if (!c) return withFocus(s, "list");
      return withFocus(selectMailbox(s, c.account, c.slug), "list");
    }
    case "jump_mailbox": {
      const account = s.selection.account ?? accountNames(s)[0];
      if (!account) return s;
      const slug = mailboxSlugs(s, account)[a.index];
      if (!slug) return s;
      return withFocus(selectMailbox(s, account, slug), "list");
    }
    case "next_account": {
      const names = accountNames(s);
      if (names.length === 0) return s;
      const i = s.selection.account ? names.indexOf(s.selection.account) : -1;
      const account = names[(i + 1) % names.length];
      return reducer(s, { type: "select_account", account });
    }

    case "focus":
      return withFocus(s, a.pane);
    case "pane_focused": {
      // The DOM focus already moved (a click, a native Tab): record it
      // without asking the DOM to follow again.
      if (s.focus === a.pane) return s;
      return { ...s, focus: a.pane, history: [...s.history, s.focus].slice(-HISTORY_CAP) };
    }
    case "cycle_focus": {
      const panes = viewPanes(s);
      const i = panes.indexOf(s.focus);
      const pane = panes[(i + a.dir + panes.length) % panes.length];
      return { ...withFocus(s, pane), zoomed: false };
    }
    case "back": {
      const history = [...s.history];
      let pane = history.pop();
      // A full-pane view has no reader to go back to.
      while (pane === "reader" && s.view !== "mail") pane = history.pop();
      if (!pane) return reducer(s, { type: "up" });
      return { ...s, focus: pane, history, focusSeq: s.focusSeq + 1 };
    }
    case "up": {
      // The hierarchical parent: reader to list, list to sidebar.
      if (s.focus === "reader") return withFocus(s, "list");
      if (s.focus === "list") return withFocus(s, "sidebar");
      return s;
    }
    case "clear_selection":
      if (s.view !== "mail") return withFocus(toMail(s), "list");
      if (s.focus === "reader") return withFocus(s, "list");
      if (s.outboxView) return withFocus(closeOutbox(s), "list");
      if (s.search) return withFocus(endSearch(s), "list");
      return {
        ...s,
        selection: { ...s.selection, message: null, draft: null, hit: null },
        reader: emptyReader(),
      };
    case "toggle_zoom":
      if (s.focus === "sidebar") return s;
      return { ...s, zoomed: !s.zoomed };
    case "toggle_sidebar":
      return { ...s, prefs: { ...s.prefs, sidebarCollapsed: !s.prefs.sidebarCollapsed } };
    case "set_sidebar_open":
      return { ...s, prefs: { ...s.prefs, sidebarCollapsed: !a.open } };
    case "set_list_width": {
      const px = Math.round(Math.max(LIST_WIDTH_MIN, Math.min(LIST_WIDTH_MAX, a.px)));
      return px === s.prefs.listWidth ? s : { ...s, prefs: { ...s.prefs, listWidth: px } };
    }
    case "set_layout":
      return a.layout === s.layout ? s : { ...s, layout: a.layout };
    case "switch_view": {
      // Leaving Mail ends a search, as choosing a mailbox does; the
      // selection, the marks and the outbox view wait for Mail's return.
      const next = a.view === "mail" ? toMail(s) : { ...endSearch(s), view: a.view, zoomed: false };
      return withFocus(next, "list");
    }
    case "overlay":
      return {
        ...s,
        overlay: a.overlay,
        dialog: a.overlay === "mutation" ? s.dialog : null,
        composeDialog: a.overlay === "compose" ? s.composeDialog : null,
        attachDialog: a.overlay === "attachments" ? s.attachDialog : null,
      };
    case "open_dialog":
      return { ...s, overlay: "mutation", dialog: a.dialog, composeDialog: null, attachDialog: null };
    case "open_compose":
      return { ...s, overlay: "compose", composeDialog: a.dialog, dialog: null, attachDialog: null };
    case "open_attachments":
      return { ...s, overlay: "attachments", attachDialog: a.dialog, dialog: null, composeDialog: null };
    case "save_dir":
      return a.dir.trim() ? { ...s, saveDir: a.dir.trim() } : s;
    case "hit_fetch_started":
      return s.fetching.includes(a.key) ? s : { ...s, fetching: [...s.fetching, a.key] };
    case "hit_fetched":
      return hitFetched({ ...s, fetching: s.fetching.filter((k) => k !== a.key) }, a.key, a.outcome);
    case "hit_fetch_failed": {
      const next = { ...s, fetching: s.fetching.filter((k) => k !== a.key) };
      return pushNotice(next, { kind: "failed", account: s.search?.account ?? null, text: `Fetch failed: ${a.error.message}`, rows: [] });
    }
    case "compose_opening":
      return composeOpening(s, a.account, a.draftId, a.path);
    case "compose_editing":
      return composeUpdate(s, a.account, a.draftId, { status: "editing", editor: a.editor, message: null });
    case "compose_failed": {
      const key = targetKey({ account: a.account, draft: a.draftId });
      const name = s.compose[key]?.name ?? a.draftId;
      const next = composeUpdate(s, a.account, a.draftId, { status: "error", message: a.error.message });
      return pushNotice(next, { kind: "compose_failed", account: a.account, text: `The editor did not open ${name}: ${a.error.message}` });
    }
    case "compose_done": {
      const key = targetKey({ account: a.account, draft: a.draftId });
      if (!(key in s.compose)) return s;
      const compose = { ...s.compose };
      delete compose[key];
      return { ...s, compose };
    }
    case "activity":
      return pushNotice(s, { kind: a.kind, account: a.account, text: a.text, rows: a.rows ?? [] });
    case "filter":
      return { ...s, filter: a.text };
    case "notice":
      return { ...s, notice: a.text };
    case "error":
      return { ...s, lastError: a.error };

    case "search_local":
      return withFocus({ ...startSearch(s, "local", a.query), marked: NO_MARKS }, "list");
    case "search_local_loaded": {
      const search = s.search;
      if (!search || search.seq !== a.seq || search.mode !== "local") return s;
      // A hit whose row is on its way out of its mailbox is not shown again.
      const hits = a.hits
        .map((h) => localHit(search.account, h))
        .filter((h) => {
          const e = h.row_id === null ? undefined : s.pending[targetKey({ account: h.account, row_id: h.row_id })];
          return !e?.leave;
        });
      return { ...s, search: { ...search, status: "done", hits } };
    }
    case "search_local_failed": {
      const search = s.search;
      if (!search || search.seq !== a.seq || search.mode !== "local") return s;
      return { ...s, search: { ...search, status: "failed", error: a.error.message } };
    }
    case "search_server":
      return withFocus({ ...startSearch(s, "server", a.query), marked: NO_MARKS }, "list");
    case "search_server_started":
      return serverStarted(s, a.seq, a.operation_id);
    case "search_server_failed": {
      const search = s.search;
      if (!search || search.seq !== a.seq || search.mode !== "server" || search.status !== "searching") return s;
      return { ...s, search: { ...search, status: "failed", error: a.error.message, early: [] } };
    }
    case "search_server_cancelled": {
      const search = s.search;
      if (!search || search.operationId !== a.operation_id || search.status !== "running") return s;
      // The Rust layer stops awaiting a cancelled id, so no finish follows.
      return { ...s, search: { ...search, status: a.outcome === "cancelled" ? "cancelled" : "done" } };
    }
    case "exit_search":
      return s.search ? withFocus(endSearch(s), "list") : s;
    case "intercepted_fetched": {
      const seen = new Set(s.intercepted.map((u) => `${u.at}|${u.source}|${u.url}`));
      const fresh = a.urls.filter((u) => !seen.has(`${u.at}|${u.source}|${u.url}`));
      if (fresh.length === 0) return s;
      return { ...s, intercepted: [...s.intercepted, ...fresh].slice(-INTERCEPT_CAP) };
    }
    case "dismiss_intercept":
      return { ...s, interceptNotice: null };

    case "mutation_apply": {
      const before = visibleItems(s);
      let next = applyMutation(s, a.batch, a.kind, a.targets, a.destination ?? null, a.value ?? null);
      if (!leaves(a.kind)) return next;
      const gone = new Set(a.targets.map(targetKey));
      next = cursorAfterLeave(next, before, gone);
      // The mailbox list's own selection, kept under a search, lets go too.
      const kept = next.search?.restore.selection;
      if (kept?.message && kept.account && gone.has(targetKey({ account: kept.account, row_id: kept.message.row_id }))) {
        next = setListSelection(next, { ...kept, message: null });
      }
      return next;
    }
    case "mutation_settled":
      return settleMutation(s, a.batch, a.kind, a.account, a.done, a.failed, a.value ?? null, a.moved_to ?? null);
    case "mutation_failed": {
      const failed = a.targets.map((target) => ({ target, reason: a.error.message }));
      return settleMutation(s, a.batch, a.kind, a.account, [], failed, null, null);
    }
    case "hold_cancel_requested":
      return holdCancelRequested(s, a.operation_id);
    case "hold_cancel_answered":
      return holdCancelAnswered(s, a.operation_id, a.cancelled);
    case "hold_cancel_failed":
      return holdCancelFailed(s, a.operation_id, a.error.message);
    case "dismiss_hold":
      return dismissHold(s, a.operation_id);
    case "sync_requested":
      return syncRequested(s);
    case "sync_started":
      return syncStarted(s, a.operation_id, a.account, a.mode);
    case "sync_failed":
      return syncStartFailed(s, a.account, a.error.message);
    case "send_requested":
      return sendRequested(s, { token: a.token, kind: a.kind, account: a.account, drafts: a.drafts, subject: a.subject });
    case "send_started":
      return sendStarted(s, a.token, a.operation_id, a.held);
    case "send_failed":
      return sendStartFailed(s, a.token, a.error.message, a.invalid);
    case "dismiss_notice":
      return s.activity.some((n) => n.id === a.id) ? { ...s, activity: s.activity.filter((n) => n.id !== a.id) } : s;
    case "operation_progress":
      return { ...s, progress: { ...s.progress, [a.operation_id]: a.progress } };
    case "dismiss_all_notices":
      return s.activity.length > 0 ? { ...s, activity: [] } : s;
    case "open_outbox":
      return withFocus(openOutbox(endSearch(s), a.account), "list");
    case "close_outbox":
      return s.outboxView ? withFocus(closeOutbox(s), "list") : s;
    case "outbox_loaded":
      return outboxLoaded(s, a.account, a.gen, a.listing);
    case "outbox_failed":
      return outboxFailed(s, a.account, a.gen, a.error);
    case "outbox_select":
      return selectOutboxRow(s, a.row_id);
    case "outbox_action_requested":
      return outboxActionRequested(s, { token: a.token, kind: a.kind, account: a.account, row_id: a.row_id });
    case "outbox_retry_started":
      return outboxRetryStarted(s, a.token, a.operation_id);
    case "outbox_discarded":
      return outboxDiscarded(s, a.token, a.message_id);
    case "outbox_action_failed":
      return outboxActionFailed(s, a.token, a.error.message);
    case "calendar_loaded":
      return calendarLoaded(s, a.account, a.gen, a.events);
    case "calendar_failed":
      return calendarFailed(s, a.account, a.gen, a.error);
    case "calendar_select":
      return selectCalendarRow(s, a.row_id);
    case "calendar_toggle_past":
      return toggleCalendarPast(s);
    case "calendar_refresh":
      return refreshCalendar(s);
    case "mark_toggle":
    case "mark_set":
    case "mark_range":
    case "mark_all":
    case "mark_clear":
      return marks(s, a);
  }
}

export { isStale };
