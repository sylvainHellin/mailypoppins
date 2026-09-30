// Protocol-shaped types: what travels on the daemon wire (docs/daemon-protocol.md),
// embedded verbatim in the Tauri layer's results and events.
//
// Hand-written for M1. They are to be replaced by types generated from
// `mp-protocol` (ts-rs), so this module holds wire shapes only; the GUI
// layer's own result types live in src/lib/gui-types.ts.

export type AccountState = "opening" | "ready" | "blocked";
export type SyncHealthState = "unknown" | "ok" | "failed";

export type AccountSnapshot = {
  name: string;
  state: AccountState;
  sync_health: { state: SyncHealthState };
};

export type MailboxRow = {
  role: string;
  slug: string;
  label: string;
  total: number;
  unread: number;
  badge: number;
};

export type DraftRow = {
  id: string;
  path: string;
  to: string | null;
  subject: string;
  status: string;
  valid: boolean;
  ready: boolean;
};

export type OutboxCounts = { queued: number; failed: number };

export type HoldStatus = {
  operation_id: string;
  account: string;
  draft_id: string;
  subject: string;
  hold_secs: number;
  remaining_secs: number;
  fires_at: string;
  origin: string;
};

export type OperationState = "queued" | "running" | "succeeded" | "failed" | "cancelled";
export type CancelScope = "durable" | "client_scoped";

export type RpcError = { code: number; message: string; data?: unknown };

export type OperationStatus = {
  operation_id: string;
  method: string;
  state: OperationState;
  scope: CancelScope;
  progress: { phase: string; done: number; total: number | null; message: string | null } | null;
  result: unknown;
  error: RpcError | null;
};

export type CheckStatus = "ok" | "warn" | "fail";
export type HealthCheck = { name: string; status: CheckStatus; detail: string };

export type Snapshot = {
  accounts: AccountSnapshot[];
  mailboxes: Record<string, MailboxRow[]>;
  drafts: Record<string, DraftRow[]>;
  outbox: Record<string, OutboxCounts>;
  holds: HoldStatus[];
  operations: OperationStatus[];
  diagnostics: HealthCheck[];
};

export type Bootstrap = {
  instance_id: string;
  revision: number;
  capabilities: string[];
  snapshot: Snapshot;
};

export type MessageFlags = {
  seen: boolean;
  answered: boolean;
  forwarded: boolean;
  flagged: boolean;
};

/** `mp_protocol::listing::MessageListRow`. */
export type MessageListRow = {
  id: number;
  uid: number;
  message_id: string;
  from: string;
  to: string;
  cc: string | null;
  reply_to: string | null;
  bcc: string | null;
  subject: string;
  date_sort: string;
  date_display: string;
  flags: MessageFlags;
  has_attachments: boolean;
  is_invite: boolean;
  selector: string;
};

export type DraftEntry = {
  id: string;
  selector: string;
  path: string;
  status: string;
  to: string | null;
  cc: string | null;
  subject: string | null;
  date: string | null;
  valid: boolean;
  ready: boolean;
};

export type DraftListing = {
  account: string;
  drafts: DraftEntry[];
  skipped: { path: string; error: string }[];
  collisions: { id: string; kept: string; shadowed: string }[];
};

/** A `state.event` envelope, verbatim. */
export type EventEnvelope = {
  instance_id: string;
  revision: number;
  kind: string;
  payload: unknown;
};

// Payloads of the event kinds the M1 shell reduces. Every other kind is
// carried through untouched.

export type StateInvalidatePayload = { resource: string; scope: unknown };
export type StateRemovePayload = { resource: string };
export type AccountStateChangedPayload = { account: string; state: AccountState; reason?: string };
export type SyncCompletedPayload = {
  account: string;
  severity: "ok" | "warning" | "error";
  error: string | null;
  saved: number;
  new_inbox_mail: { from: string; subject: string }[];
};
export type DraftChangedPayload = DraftRow & { account: string };

/**
 * `mp_protocol::listing::ServerSearchHit`: `mailbox` is the sidebar label the
 * hit was found under, and `row_id` and `selector` are null for a message the
 * store has never ingested.
 */
export type ServerSearchHit = {
  account: string;
  mailbox: string;
  message_id: string;
  row_id: number | null;
  selector: string | null;
  from: string;
  to: string;
  cc: string | null;
  reply_to: string | null;
  bcc: string | null;
  subject: string;
  date_display: string;
  date_sort: string;
  flags: MessageFlags;
  has_attachments: boolean;
  is_invite: boolean;
  body_text: string | null;
  html_body: string | null;
};

export type ServerHitPayload = { operation_id: string; hit: ServerSearchHit };

/** `operation.finished`: `result` on success, `error` on failure or cancellation. */
export type OperationFinishedPayload = {
  operation_id: string;
  state: OperationState;
  result?: unknown;
  error?: RpcError | null;
};
export type MutationsRolledBackPayload = { account: string; failed: number };
export type ShuttingDownPayload = { grace_secs: number; pending: unknown[] };
