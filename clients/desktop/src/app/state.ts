// The desktop shell's model: connection, bootstrap, the fetched lists, the
// selection and the presentation state. Pure; the reducer is in reducer.ts.

import type { Bootstrap, DraftEntry, MessageListRow } from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  GuiError,
  InterceptedUrl,
  MailboxListing,
  MessageList,
  MessageMeta,
  MessageText,
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

export type Overlay = "palette" | "help" | "restart" | null;

export type ReaderState = {
  key: string | null;
  meta: MessageMeta | null;
  text: MessageText | null;
  load: Loadable<true>;
};

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
  sidebarCursor: { account: string; slug: string } | null;
  focus: Pane;
  /** Moves whenever the DOM focus should follow `focus` (keyboard-driven). */
  focusSeq: number;
  history: Pane[];
  zoomed: boolean;
  layout: Layout;
  prefs: Prefs;
  overlay: Overlay;
  filter: string;
  notice: string | null;
  lastError: GuiError | null;
  intercepted: InterceptedUrl[];
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
    reader: { key: null, meta: null, text: null, load: emptyLoadable() },
    selection: { account: null, mailbox: null, message: null, draft: null },
    sidebarCursor: null,
    focus: "list",
    focusSeq: 0,
    history: [],
    zoomed: false,
    layout: "wide",
    prefs,
    overlay: null,
    filter: "",
    notice: null,
    lastError: null,
    intercepted: [],
  };
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
