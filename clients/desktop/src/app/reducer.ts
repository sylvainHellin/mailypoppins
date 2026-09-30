// The one reducer: GuiEvents, fetched answers and user intents.

import type {
  AccountStateChangedPayload,
  Bootstrap,
  StateInvalidatePayload,
  StateRemovePayload,
  SyncCompletedPayload,
} from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  GuiError,
  GuiEvent,
  InterceptedUrl,
  LocalSearchHit,
  MailboxListing,
  MessageList,
  MessageMeta,
  VersionInfo,
} from "@/lib/gui-types";
import {
  finishedSignal,
  localHit,
  openableHits,
  serverHitSignal,
  serverStarted,
  settledSignal,
  signal,
  startSearch,
} from "@/app/search";
import {
  accountNames,
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
  PANES,
  readerKey,
  type AppState,
  type Layout,
  type Loadable,
  type MessageRef,
  type Overlay,
  type Pane,
  type Selection,
} from "@/app/state";

export type Action =
  | { type: "gui_event"; event: GuiEvent }
  | { type: "connection_status"; status: ConnectionStatus }
  | { type: "version_info"; info: VersionInfo }
  | { type: "accounts_loaded"; gen: number; accounts: AccountInfo[] }
  | { type: "accounts_failed"; gen: number; error: GuiError }
  | { type: "mailboxes_loaded"; account: string; gen: number; listing: MailboxListing }
  | { type: "mailboxes_failed"; account: string; gen: number; error: GuiError }
  | { type: "messages_loaded"; key: string; gen: number; list: MessageList }
  | { type: "messages_failed"; key: string; gen: number; error: GuiError }
  | { type: "reader_loaded"; key: string; gen: number; meta: MessageMeta }
  | { type: "reader_failed"; key: string; gen: number; error: GuiError }
  | { type: "select_account"; account: string }
  | { type: "select_mailbox"; account: string; slug: string; focus?: Pane }
  | { type: "select_message"; message: Omit<MessageRef, "verified">; focus?: Pane }
  | { type: "select_draft"; id: string; focus?: Pane }
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
  | { type: "overlay"; overlay: Overlay }
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
  | { type: "dismiss_intercept" };

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
  return { ...s, selection: sel, messages, sidebarCursor: cursor, filter: key === s.messages.key ? s.filter : "" };
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
  let next: AppState = { ...s, search: null, filter: "", selection: search.restore.selection };
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
  return retarget(s, { account, mailbox: slug, message: null, draft: null });
}

type ListItem = { kind: "message"; ref: Omit<MessageRef, "verified"> } | { kind: "draft"; id: string };

/** The rows the list pane shows, after the local filter, or the search's hits. */
export function visibleItems(s: AppState): ListItem[] {
  if (s.search) return openableHits(s.search).map((ref) => ({ kind: "message", ref }));
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
      : sel.message !== null && sel.message.message_id === it.ref.message_id &&
        sel.message.selector === it.ref.selector,
  );
}

function selectItem(s: AppState, it: ListItem): AppState {
  if (it.kind === "draft") {
    return { ...s, selection: { ...s.selection, message: null, draft: it.id } };
  }
  return { ...s, selection: { ...s.selection, draft: null, message: { ...it.ref, verified: true } } };
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
    next = retarget(next, { account, mailbox, message: null, draft: null });
  }
  return next;
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

  next = retarget(next, { account, mailbox, message, draft });
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
  const rerun = search.mode === "local" && !sameInstance;
  return {
    ...base,
    search: {
      ...search,
      restore: { ...search.restore, selection: base.selection },
      ...(rerun ? { status: "searching" as const, seq: search.seq + 1, error: null } : {}),
    },
    selection: { ...base.selection, message: hit, draft: null },
    reader: hit ? { ...s.reader, load: markStale(s.reader.load) } : emptyReader(),
  };
}

function applyEnvelope(s: AppState, kind: string, payload: unknown): AppState {
  switch (kind) {
    case "state.invalidate": {
      const { resource } = payload as StateInvalidatePayload;
      const { family, parts } = parseResource(resource);
      const account = parts[0] ?? "";
      if (family === "mailbox") {
        const slug = parts[1] ?? "";
        return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === slug);
      }
      if (family === "outbox") return { ...s, accounts: markStale(s.accounts) };
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
      if (family === "account") return removeAccount(s, account);
      if (family === "mailbox") {
        const slug = parts[1] ?? "";
        const sel = listSelection(s);
        let next = staleMailboxes(sel.account === account && sel.mailbox === slug ? endSearch(s) : s, account);
        if (next.selection.account === account && next.selection.mailbox === slug) {
          const mailbox = next.mailboxes[account]?.data?.mailboxes.find((m) => m.slug !== slug && m.role === "inbox")?.slug ?? null;
          next = retarget(next, { account, mailbox, message: null, draft: null });
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
      const next = staleMailboxes(patchAccount(s, p.account, { sync_health: health }), p.account);
      return staleListIf(next, (a) => a === p.account);
    }
    case "draft.changed":
    case "draft.invalid": {
      const account = (payload as { account: string }).account;
      return staleListIf(staleMailboxes(s, account), (a, m) => a === account && m === "drafts");
    }
    case "mutations.rolled_back": {
      const account = (payload as { account: string }).account;
      return staleListIf(staleMailboxes(s, account), (a) => a === account);
    }
    case "daemon.shutting_down":
      return { ...s, shuttingDown: true };
    case "message.server_hit":
      return signal(s, serverHitSignal(payload));
    case "operation.finished":
      return signal(s, finishedSignal(payload));
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
    case "rebootstrapped":
      return applyBootstrap(s, e.bootstrap);
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
      return signal(s, settledSignal(e.operation_id, e.status));
    case "operation_dropped":
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
    if (sel.draft && !list.listing.drafts.some((d) => d.id === sel.draft)) {
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

export function reducer(s: AppState, a: Action): AppState {
  if (s.selectionAuto && USER_SELECTION.has(a.type)) s = { ...s, selectionAuto: false };
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
          next = retarget(next, { account: def, mailbox, message: null, draft: null });
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
      const next = { ...s, messages: { ...loaded(s.messages, a.gen, a.list), key: s.messages.key } };
      return reverify(next, a.list);
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
      return retarget(s, { account: a.account, mailbox: defaultMailbox(s, a.account), message: null, draft: null });
    }
    case "select_mailbox": {
      const next = selectMailbox(s, a.account, a.slug);
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "select_message": {
      const next = { ...s, selection: { ...s.selection, draft: null, message: { ...a.message, verified: true } } };
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "select_draft": {
      const next = { ...s, selection: { ...s.selection, message: null, draft: a.id } };
      return a.focus ? withFocus(next, a.focus) : next;
    }
    case "move_selection": {
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
      const i = PANES.indexOf(s.focus);
      const pane = PANES[(i + a.dir + PANES.length) % PANES.length];
      return { ...withFocus(s, pane), zoomed: false };
    }
    case "back": {
      const history = [...s.history];
      const pane = history.pop();
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
      if (s.focus === "reader") return withFocus(s, "list");
      if (s.search) return withFocus(endSearch(s), "list");
      return {
        ...s,
        selection: { ...s.selection, message: null, draft: null },
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
    case "overlay":
      return { ...s, overlay: a.overlay };
    case "filter":
      return { ...s, filter: a.text };
    case "notice":
      return { ...s, notice: a.text };
    case "error":
      return { ...s, lastError: a.error };

    case "search_local":
      return withFocus(startSearch(s, "local", a.query), "list");
    case "search_local_loaded": {
      const search = s.search;
      if (!search || search.seq !== a.seq || search.mode !== "local") return s;
      return { ...s, search: { ...search, status: "done", hits: a.hits.map((h) => localHit(search.account, h)) } };
    }
    case "search_local_failed": {
      const search = s.search;
      if (!search || search.seq !== a.seq || search.mode !== "local") return s;
      return { ...s, search: { ...search, status: "failed", error: a.error.message } };
    }
    case "search_server":
      return withFocus(startSearch(s, "server", a.query), "list");
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
  }
}

export { isStale };
