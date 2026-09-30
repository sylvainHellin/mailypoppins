// The Tauri layer's own result and event types, as clients/desktop/docs/rust-layer.md
// defines them. Protocol shapes they embed come from src/protocol/types.ts.

import type {
  AccountState,
  Bootstrap,
  DraftListing,
  EventEnvelope,
  MessageListRow,
  OperationStatus,
  OutboxCounts,
  SyncHealthState,
} from "@/protocol/types";

export type GuiError =
  | { kind: "daemon_unavailable"; message: string; socket: string | null; log: string | null }
  | {
      kind: "version_mismatch";
      message: string;
      daemon_version: string | null;
      client_protocol: { min: number; max: number };
    }
  | { kind: "timeout"; message: string }
  | { kind: "protocol"; message: string; code: number | null }
  | { kind: "not_found"; message: string; code: number | null }
  | { kind: "internal"; message: string };

export type ConnectError = {
  kind: "unavailable" | "version_mismatch" | "identity_mismatch";
  why: string;
  socket: string;
  log: string;
  daemon_version: string | null;
};

export type ConnectionStatus =
  | { state: "connecting" }
  | {
      state: "connected";
      instance_id: string;
      daemon_version: string;
      protocol: number;
      fixture: boolean;
    }
  | { state: "reconnecting"; reason: string; last_error: ConnectError | null }
  | { state: "failed"; error: ConnectError };

export type AccountInfo = {
  name: string;
  default: boolean;
  backend: "imap" | "graph";
  store_state: string;
  runtime_state: AccountState;
  sync_health: SyncHealthState;
  outbox: OutboxCounts;
};

export type MailboxKind = "inbox" | "drafts" | "sent" | "archive" | "extra";

export type MailboxInfo = {
  slug: string;
  label: string;
  role: string;
  kind: MailboxKind;
  total: number;
  unread: number;
  badge: number;
};

export type MailboxListing = {
  account: string;
  mailboxes: MailboxInfo[];
  total: number;
  unread: number;
  runtime_state: AccountState;
  sync_health: SyncHealthState;
};

export type MessageList =
  | { kind: "messages"; account: string; mailbox: string; total: number; rows: MessageListRow[] }
  | { kind: "drafts"; account: string; listing: DraftListing };

export type MessageText = { account: string; row_id: number; body: string | null };

export type MessageMeta = {
  row_id: number;
  html_url: string;
  selector: string;
  account: string;
  mailbox: string;
  message_id: string;
  from: string | null;
  to: string | null;
  cc: string | null;
  subject: string | null;
  date: string | null;
  flags: string[];
  invite: boolean;
  attachments: { name: string; size: number }[];
};

export type LocalSearchParams = {
  account: string;
  query: string;
  mailbox?: string;
  limit?: number;
  from?: string;
  to?: string;
  cc?: string;
  subject?: string;
  body_query?: string;
  filename?: string;
  has_attachment?: boolean;
  after?: string;
  before?: string;
};

export type LocalSearchHit = MessageListRow & { mailbox: string };

export type ServerSearchParams = {
  account: string;
  query: string;
  mailboxes?: string[];
  limit?: number;
  exclude_message_ids?: string[];
};

export type InterceptedUrl = {
  url: string;
  at: number;
  source: "navigation" | "new_window" | "open_external_stub";
};

export type VersionInfo = {
  app_version: string;
  protocol_min: number;
  protocol_max: number;
  fixture: boolean;
  daemon: { daemon_version: string; protocol: number; instance_id: string } | null;
};

export type FixtureSimulation =
  | "disconnect"
  | "reconnect"
  | "restart"
  | "resync"
  | "new_mail"
  | "shutdown";

export type BootstrapCause =
  | "initial"
  | "subscribed"
  | "requested"
  | "resync"
  | "reconnected"
  | "instance_changed";

export type GuiEvent =
  | { type: "event"; event: EventEnvelope }
  | { type: "resync"; instance_id: string; reason: string }
  | { type: "disconnected"; reason: string }
  | { type: "reconnected"; instance_id: string }
  | { type: "rebootstrapped"; cause: BootstrapCause; bootstrap: Bootstrap }
  | {
      type: "operation_settled";
      operation_id: string;
      kind: "server_search";
      status: OperationStatus;
    }
  | { type: "operation_dropped"; operation_id: string; kind: "server_search"; reason: string }
  | { type: "connection"; status: ConnectionStatus }
  | { type: "link_intercepted"; url: InterceptedUrl };

/** Narrow an unknown rejection to a GuiError, or wrap it as `internal`. */
export function asGuiError(e: unknown): GuiError {
  if (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as { kind: unknown }).kind === "string"
  ) {
    return e as GuiError;
  }
  return { kind: "internal", message: e instanceof Error ? e.message : String(e) };
}
