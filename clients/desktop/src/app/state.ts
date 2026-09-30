// The desktop shell's model: connection, bootstrap, the fetched lists, the
// selection and the presentation state. Pure; the reducer is in reducer.ts.

import type { Bootstrap, DraftEntry, HoldStatus, MessageFlags, MessageListRow } from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  GuiError,
  InterceptedUrl,
  MailboxListing,
  MessageList,
  MessageMeta,
  SyncMode,
  VersionInfo,
} from "@/lib/gui-types";

export type Pane = "sidebar" | "list" | "reader";
export const PANES: readonly Pane[] = ["sidebar", "list", "reader"];

export type Layout = "wide" | "medium" | "narrow";

/**
 * A fetched answer. `gen` moves when the answer goes stale, `loadedGen` is
 * the generation the data (or the error) was fetched at; the answer is stale
 * while they differ, so an event arriving during a fetch keeps it stale and
 * the loader fetches again once the first answer lands.
 */
export type Loadable<T> = {
  data: T | null;
  gen: number;
  loadedGen: number;
  error: GuiError | null;
};

export function emptyLoadable<T>(): Loadable<T> {
  return { data: null, gen: 1, loadedGen: 0, error: null };
}

export function isStale<T>(l: Loadable<T>): boolean {
  return l.gen !== l.loadedGen;
}

export function markStale<T>(l: Loadable<T>): Loadable<T> {
  return isStale(l) ? l : { ...l, gen: l.gen + 1 };
}

/**
 * The selected message, by its stable identifiers. `row_id` is per store and
 * per daemon instance, so after a re-bootstrap it is not trusted
 * (`verified: false`) until the reloaded list confirms it by `message_id`.
 */
export type MessageRef = {
  row_id: number;
  message_id: string;
  selector: string;
  verified: boolean;
};

export type Selection = {
  account: string | null;
  mailbox: string | null;
  message: MessageRef | null;
  /** A draft id when the Drafts mailbox is selected. */
  draft: string | null;
};

export type Prefs = {
  sidebarCollapsed: boolean;
  /** The list pane's width in px, wide and medium layouts. */
  listWidth: number;
};

export const DEFAULT_PREFS: Prefs = { sidebarCollapsed: false, listWidth: 420 };
export const LIST_WIDTH_MIN = 260;
export const LIST_WIDTH_MAX = 720;
/** The reader keeps at least this much of the pane row, whatever the list width. */
export const READER_MIN = 320;
/** What "Widen list" and "Narrow list" move the splitter by. */
export const LIST_WIDTH_STEP = 40;

/** `mutation` is the confirmation or the move picker `dialog` describes. */
export type Overlay = "palette" | "help" | "restart" | "intercepted" | "mutation" | null;

/**
 * The reader's headers. The body is the `mpmsg` document the iframe loads
 * itself, so the model holds no body.
 */
export type ReaderState = {
  key: string | null;
  meta: MessageMeta | null;
  load: Loadable<true>;
};

export function emptyReader(): ReaderState {
  return { key: null, meta: null, load: emptyLoadable() };
}

/** One search result, local or from the server, in the list's shape. */
export type SearchHit = {
  /** Unique within one search: the mailbox and the message_id, or the hit's position when it has none. */
  key: string;
  account: string;
  /** The mailbox slug (local) or sidebar label (server) it was found under. */
  mailbox: string;
  /** Null for a server hit whose server returned no `Message-ID:`, which can be listed and not opened. */
  message_id: string | null;
  /** Null for a server-only hit the store has never ingested. */
  row_id: number | null;
  selector: string | null;
  from: string;
  subject: string;
  date_display: string;
  date_sort: string;
  flags: MessageFlags;
  has_attachments: boolean;
  is_invite: boolean;
  origin: "local" | "server";
};

/** What the event stream says about a server search, by operation id. */
export type SearchSignal =
  | { kind: "hit"; operation_id: string; hit: SearchHit }
  | { kind: "finish"; operation_id: string; state: string; result: unknown; error: string | null }
  | { kind: "dropped"; operation_id: string; reason: string };

/**
 * `searching`: the local query or the server start is in flight;
 * `running`: the server operation streams hits; the rest are terminal.
 */
export type SearchStatus = "searching" | "running" | "done" | "cancelled" | "failed" | "dropped";

export type SearchState = {
  query: string;
  account: string;
  mode: "local" | "server";
  hits: SearchHit[];
  /** The server operation, once `search_server_start` answered. */
  operationId: string | null;
  status: SearchStatus;
  /** Moves with every run, so a late answer to an older run is dropped. */
  seq: number;
  error: string | null;
  /** The settled summary: hits the server counted, mailboxes it could not reach. */
  summary: { hits: number; unreachable: number } | null;
  /**
   * Server signals that arrived before `search_server_start` answered with
   * their id; the Rust layer registers the id before it answers, so its
   * first hits can overtake the answer on the way to the webview.
   */
  early: SearchSignal[];
  /** The mailbox list's selection and focus, restored when the search ends. */
  restore: { selection: Selection; focus: Pane };
};

/** What a mutation does to a row: leave the list, or change a flag in place. */
export type MutationKind = "archive" | "delete" | "move" | "flag" | "read" | "discard";

/** A message row or a local draft a mutation names. */
export type Target = { account: string; row_id: number } | { account: string; draft: string };

/** A message row a mutation names. */
export type MessageTarget = { account: string; row_id: number };

/**
 * The key a row goes by in `pending` and `marked`: `<account>#<row_id>`, or
 * `<account>#draft:<id>` for a draft. Row ids are per daemon instance, so a
 * key does not outlive a daemon restart.
 */
export function targetKey(t: Target): string {
  return "row_id" in t ? `${t.account}#${t.row_id}` : `${t.account}#draft:${t.draft}`;
}

/** A count change a mutation made to one mailbox of the sidebar, to reverse on a restore. */
export type CountDelta = { account: string; mailbox: string; total: number; unread: number };

/**
 * A flag or read change of one row. `batch` is the dispatch that set it last:
 * a second change of the same axis takes it over and keeps the first one's
 * `prev`, so the restore is to what the daemon held before either.
 */
export type PendingFlag = {
  batch: number;
  /** The state the change set. */
  value: boolean | null;
  /** The state before the change, when it was known. */
  prev: boolean | null;
  counts: CountDelta[];
};

/** An archive, delete, move or discard: the row is out of its list. */
export type PendingLeave = {
  batch: number;
  kind: MutationKind;
  /** The mailbox slug an archive or a move puts the row in. */
  destination: string | null;
  /** The row as the shown list had it, and where. */
  prevRow: { key: string; row: MessageListRow; index: number } | null;
  /** The search hit as the shown search (`seq`) had it, and where. */
  prevHit: { hit: SearchHit; index: number; seq: number } | null;
  /** The draft as the shown Drafts list had it, and where. */
  prevDraft: { key: string; entry: DraftEntry; index: number } | null;
  counts: CountDelta[];
};

/**
 * One row's optimistic changes, from the moment the user acted until the
 * commands' answers confirm or refuse them. Each axis (the flag, the read
 * state, leaving the list) keeps its own saved state and the batch that owns
 * it, so the answer of one batch settles or restores its own axis only.
 */
export type PendingChange = {
  target: Target;
  /** The mailbox slug the row was in, when it was known. */
  source: string | null;
  /** The subject, for a failure notice. */
  subject: string | null;
  flag: PendingFlag | null;
  read: PendingFlag | null;
  leave: PendingLeave | null;
};

/** Where a hold stands, from the `send.hold_*` event that last moved it. */
export type HoldPhase = "started" | "tick" | "cancelled" | "fired";

/**
 * A send waiting out its undo window. `remaining_secs` is always the
 * daemon's, never counted down locally. `cancelling` is set while this
 * window's `send_cancel_hold` is in flight.
 */
export type HoldEntry = HoldStatus & { state: HoldPhase; cancelling: boolean };

export type ActivityKind =
  | "applied"
  | "failed"
  | "rolled_back"
  | "hold_cancelled"
  | "hold_cancel_failed"
  | "sync_failed";

/** One line of the activity area, dismissed by `id`. */
export type ActivityNotice = {
  id: number;
  kind: ActivityKind;
  account: string | null;
  text: string;
  /** The rows a failed batch put back, each with the daemon's reason. */
  rows: { key: string; label: string; reason: string }[];
};

/** The list's multi-select: row keys (`targetKey`) and the range anchor. */
export type Marked = { keys: ReadonlySet<string>; anchor: string | null };

export const NO_MARKS: Marked = { keys: new Set<string>(), anchor: null };

/**
 * A mutation that waits for the user: the confirmation archive and delete
 * ask for (the TUI's), or the mailbox picker a move opens. The targets are
 * taken when it opens, so what runs is what the dialog named.
 */
export type MutationDialog =
  | { kind: "archive" | "delete"; targets: Target[]; title: string; detail: string }
  | { kind: "move"; targets: MessageTarget[]; account: string; source: string | null };

/** A sync `sync_trigger` started, until it finishes, settles or is dropped. */
export type RunningSync = { account: string; mode: SyncMode };

/** How an operation ended, as `operation.finished` or `operation_settled` says. */
export type OperationEnd = { operation_id: string; state: string; error: string | null } | { operation_id: string; dropped: string };

export type AppState = {
  connection: ConnectionStatus;
  /** The reason of the last `disconnected`, until `reconnected` or a bootstrap. */
  disconnected: string | null;
  /** The reason of a pending `resync`, until the `rebootstrapped` that answers it. */
  resync: string | null;
  shuttingDown: boolean;
  version: VersionInfo | null;
  bootstrap: Bootstrap | null;
  accounts: Loadable<AccountInfo[]>;
  mailboxes: Record<string, Loadable<MailboxListing>>;
  messages: Loadable<MessageList> & { key: string | null };
  reader: ReaderState;
  selection: Selection;
  /**
   * The selection's account was picked by a bootstrap, not by the user: the
   * first `list_accounts` answer may still move it to the default account.
   * Any user selection clears it.
   */
  selectionAuto: boolean;
  /** Search results replace the mailbox list while this is set. */
  search: SearchState | null;
  sidebarCursor: { account: string; slug: string } | null;
  focus: Pane;
  /** Moves whenever the DOM focus should follow `focus` (keyboard-driven). */
  focusSeq: number;
  history: Pane[];
  zoomed: boolean;
  layout: Layout;
  prefs: Prefs;
  overlay: Overlay;
  /** What the `mutation` overlay shows; null whenever another overlay or none is open. */
  dialog: MutationDialog | null;
  filter: string;
  notice: string | null;
  lastError: GuiError | null;
  /** Every refused URL this window heard of, oldest first. */
  intercepted: InterceptedUrl[];
  /** The last refused link, shown in the reader footer until dismissed. */
  interceptNotice: InterceptedUrl | null;
  /** Optimistic changes awaiting their command's answer, by `targetKey`. */
  pending: Record<string, PendingChange>;
  /**
   * Per list key, moved by every optimistic change and every answer that
   * settles one: a list answer requested at an older value is dropped, so a
   * reload that started before a mutation cannot bring its row back.
   */
  listGen: Record<string, number>;
  /** Send holds by `operation_id`, from the bootstrap and the `send.hold_*` events. */
  holds: Record<string, HoldEntry>;
  marked: Marked;
  activity: ActivityNotice[];
  activitySeq: number;
  /** Syncs this window started, by `operation_id`. */
  syncs: Record<string, RunningSync>;
  /** `sync_trigger` calls not answered yet. */
  syncStarting: number;
  /** Operation ends that arrived while a `sync_trigger` was unanswered, for its id. */
  syncEarly: OperationEnd[];
};

export function initialState(prefs: Prefs = DEFAULT_PREFS): AppState {
  return {
    connection: { state: "connecting" },
    disconnected: null,
    resync: null,
    shuttingDown: false,
    version: null,
    bootstrap: null,
    accounts: emptyLoadable(),
    mailboxes: {},
    messages: { ...emptyLoadable<MessageList>(), key: null },
    reader: emptyReader(),
    selection: { account: null, mailbox: null, message: null, draft: null },
    selectionAuto: false,
    search: null,
    sidebarCursor: null,
    focus: "list",
    focusSeq: 0,
    history: [],
    zoomed: false,
    layout: "wide",
    prefs,
    overlay: null,
    dialog: null,
    filter: "",
    notice: null,
    lastError: null,
    intercepted: [],
    interceptNotice: null,
    pending: {},
    listGen: {},
    holds: {},
    marked: NO_MARKS,
    activity: [],
    activitySeq: 0,
    syncs: {},
    syncStarting: 0,
    syncEarly: [],
  };
}

/** The holds still counting down, in arm order. */
export function liveHolds(s: AppState): HoldEntry[] {
  return Object.values(s.holds).filter((h) => h.state === "started" || h.state === "tick");
}

export const listKey = (account: string, mailbox: string): string => `${account}/${mailbox}`;
export const readerKey = (account: string, rowId: number): string => `${account}#${rowId}`;

/** The accounts in display order: `list_accounts` once loaded, else the snapshot's. */
export function accountNames(s: AppState): string[] {
  if (s.accounts.data) return s.accounts.data.map((a) => a.name);
  return s.bootstrap?.snapshot.accounts.map((a) => a.name) ?? [];
}

/** An account's mailboxes: the listing once loaded, else the snapshot's rows. */
export function mailboxSlugs(s: AppState, account: string): string[] {
  const listing = s.mailboxes[account]?.data;
  if (listing) return listing.mailboxes.map((m) => m.slug);
  return s.bootstrap?.snapshot.mailboxes[account]?.map((m) => m.slug) ?? [];
}

export type Screen = "connecting" | "unavailable" | "version_mismatch" | "shell";

export function screenFor(s: AppState): Screen {
  if (s.connection.state === "failed") {
    return s.connection.error.kind === "version_mismatch" ? "version_mismatch" : "unavailable";
  }
  return s.bootstrap ? "shell" : "connecting";
}

export type Banner =
  | { kind: "reconnecting"; reason: string }
  | { kind: "resync"; reason: string }
  | { kind: "shutting_down" }
  | null;

export function bannerFor(s: AppState): Banner {
  if (s.connection.state === "reconnecting") {
    return { kind: "reconnecting", reason: s.connection.reason };
  }
  if (s.disconnected !== null) return { kind: "reconnecting", reason: s.disconnected };
  if (s.resync !== null) return { kind: "resync", reason: s.resync };
  if (s.shuttingDown) return { kind: "shutting_down" };
  return null;
}

/** The list pane's local filter, shared by the pane and the keyboard moves. */
export function filteredRows(list: MessageList | null, filter: string): MessageListRow[] {
  if (!list || list.kind !== "messages") return [];
  const f = filter.trim().toLowerCase();
  return f ? list.rows.filter((r) => `${r.subject} ${r.from}`.toLowerCase().includes(f)) : list.rows;
}

export function filteredDrafts(list: MessageList | null, filter: string): DraftEntry[] {
  if (!list || list.kind !== "drafts") return [];
  const f = filter.trim().toLowerCase();
  return f
    ? list.listing.drafts.filter((d) => `${d.subject ?? ""} ${d.to ?? ""}`.toLowerCase().includes(f))
    : list.listing.drafts;
}
