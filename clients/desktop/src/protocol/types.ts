// Protocol-shaped types: what travels on the daemon wire (docs/daemon-protocol.md),
// embedded verbatim in the Tauri layer's results and events.
//
// Every type with a Rust counterpart in `mp-protocol` is generated from it with
// ts-rs into ./generated (`pnpm gen:types`), and re-exported here so this stays
// the one import path. The GUI layer's own result types live in src/lib/gui-types.ts.

import type {
  AccountState,
  DraftChanged,
  MutationsRolledBack,
  OperationState,
  OperationStatus,
  Progress,
  RpcError,
  ServerSearchHit,
  SyncCompleted,
} from "./generated";

export type * from "./generated";

// Event payloads, by `EventEnvelope.kind`. Those the daemon builds from an
// `mp-protocol` struct are aliases of the generated type.

export type SyncCompletedPayload = SyncCompleted;
export type DraftChangedPayload = DraftChanged;
export type MutationsRolledBackPayload = MutationsRolledBack;

// The daemon assembles the payloads below inline (src/daemon/state/events.rs,
// src/daemon/methods/message_server.rs), with no struct in `mp-protocol` to
// generate from, so they stay hand-written against the fixtures in
// crates/mp-protocol/fixtures.

export type StateInvalidatePayload = { resource: string; scope: unknown };
export type StateRemovePayload = { resource: string };
export type AccountStateChangedPayload = { account: string; state: AccountState; reason?: string };

/** `message.server_hit`: one hit of a running `message.search_server`. */
export type ServerHitPayload = { operation_id: string; hit: ServerSearchHit };

/** `operation.finished`: `result` on success, `error` on failure or cancellation. */
export type OperationFinishedPayload = {
  operation_id: string;
  state: OperationState;
  result?: unknown;
  error?: RpcError | null;
};

/**
 * `operation.progress`: one report of a running operation, its `Progress`
 * beside the `operation_id` (src/daemon/operations.rs, `progress_payload`).
 */
export type OperationProgressPayload = Progress & { operation_id: string };

/** `daemon.shutting_down`: each pending entry is an `operation.status` result. */
export type ShuttingDownPayload = { grace_secs: number; pending: OperationStatus[] };
