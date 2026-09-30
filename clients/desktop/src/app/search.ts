// Search: the hit shapes and the reducer's search transitions. Local search
// is one `search_local` answer; a server search is an operation whose hits
// stream as `message.server_hit` events and which ends with
// `operation.finished`, or with `operation_settled` / `operation_dropped`
// when a re-bootstrap re-queried it (docs/rust-layer.md, "The event stream").

import type { FetchOutcome, LocalSearchHit } from "@/lib/gui-types";
import { pushNotice } from "@/app/pending";
import type { OperationFinishedPayload, OperationStatus, ServerHitPayload, ServerSearchHit } from "@/protocol/types";
import type { AppState, MessageRef, SearchHit, SearchSignal, SearchState } from "@/app/state";

export function localHit(account: string, h: LocalSearchHit): SearchHit {
  return {
    key: `${h.mailbox}\u0000${h.message_id}`,
    account,
    mailbox: h.mailbox,
    message_id: h.message_id,
    row_id: h.id,
    selector: h.selector,
    from: h.from,
    subject: h.subject,
    date_display: h.date_display,
    date_sort: h.date_sort,
    flags: h.flags,
    has_attachments: h.has_attachments,
    is_invite: h.is_invite,
    origin: "local",
    source: null,
  };
}

export function serverHit(h: ServerSearchHit): SearchHit {
  return {
    key: `${h.mailbox}\u0000${h.message_id}`,
    account: h.account,
    mailbox: h.mailbox,
    message_id: h.message_id,
    row_id: h.row_id,
    selector: h.selector,
    from: h.from ?? "",
    subject: h.subject ?? "",
    date_display: h.date_display ?? "",
    date_sort: h.date_sort ?? "",
    flags: h.flags,
    has_attachments: h.has_attachments,
    is_invite: h.is_invite,
    origin: "server",
    source: {
      from: h.from ?? "",
      to: h.to ?? "",
      cc: h.cc,
      reply_to: h.reply_to,
      subject: h.subject ?? "",
      message_id: h.message_id,
      date_display: h.date_display ?? "",
      body_text: h.body_text ?? "",
      html_body: h.html_body,
    },
  };
}

/** A hit the reader can open: the store holds a row for it. */
export function isOpenable(h: SearchHit): boolean {
  return h.row_id !== null && h.selector !== null && h.message_id !== null;
}

/** The hits the reader can open: those the store holds a row for. */
export function openableHits(search: SearchState): Omit<MessageRef, "verified">[] {
  return search.hits.flatMap((h) =>
    h.row_id !== null && h.selector !== null && h.message_id !== null
      ? [{ row_id: h.row_id, message_id: h.message_id, selector: h.selector }]
      : [],
  );
}

export function isLive(search: SearchState | null): boolean {
  return search !== null && (search.status === "searching" || search.status === "running");
}

/** Start a run: a new query, or the server leg of the one shown. */
export function startSearch(s: AppState, mode: "local" | "server", query: string): AppState {
  const account = s.search?.account ?? s.selection.account;
  const q = query.trim();
  if (!account || !q) return s;
  const prev = s.search;
  // The server leg of a finished local search keeps its hits and asks the
  // server only for what the store does not already have, as the TUI does.
  const keep =
    mode === "server" && prev?.mode === "local" && prev.status === "done" && prev.query === q ? prev.hits : [];
  const search: SearchState = {
    query: q,
    account,
    mode,
    hits: keep,
    operationId: null,
    status: "searching",
    seq: (prev?.seq ?? 0) + 1,
    error: null,
    summary: null,
    early: [],
    restore: prev?.restore ?? { selection: s.selection, focus: s.focus },
  };
  return { ...s, search };
}

function addHit(search: SearchState, hit: SearchHit): SearchState {
  // `message_id` is the dedup key, so a hit without one is never a duplicate
  // and is keyed by its position instead.
  if (hit.message_id === null) {
    const keyed = { ...hit, key: `${hit.mailbox}\u0000#${search.hits.length}` };
    return { ...search, hits: [...search.hits, keyed] };
  }
  if (search.hits.some((h) => h.message_id === hit.message_id && h.mailbox === hit.mailbox)) return search;
  return { ...search, hits: [...search.hits, hit] };
}

function summaryOf(result: unknown): SearchState["summary"] {
  if (typeof result !== "object" || result === null) return null;
  const r = result as { hits?: unknown; unreachable?: unknown };
  return {
    hits: typeof r.hits === "number" ? r.hits : 0,
    unreachable: Array.isArray(r.unreachable) ? r.unreachable.length : 0,
  };
}

function applySignal(search: SearchState, sig: SearchSignal): SearchState {
  if (sig.operation_id !== search.operationId) return search;
  // Nothing about an operation follows its end.
  if (search.status !== "running") return search;
  switch (sig.kind) {
    case "hit":
      return addHit(search, sig.hit);
    case "finish":
      switch (sig.state) {
        case "succeeded":
          return { ...search, status: "done", summary: summaryOf(sig.result) };
        case "cancelled":
          return { ...search, status: "cancelled" };
        case "failed":
          return { ...search, status: "failed", error: sig.error ?? "the server search failed" };
        default:
          return search;
      }
    case "dropped":
      return { ...search, status: "dropped", error: sig.reason };
  }
}

/** Route one signal: to the running search, or held until its id is known. */
export function signal(s: AppState, sig: SearchSignal): AppState {
  const search = s.search;
  if (!search || search.mode !== "server") return s;
  if (search.operationId === null) {
    if (search.status !== "searching") return s;
    return { ...s, search: { ...search, early: [...search.early, sig] } };
  }
  const next = applySignal(search, sig);
  return next === search ? s : { ...s, search: next };
}

export function serverHitSignal(payload: unknown): SearchSignal {
  const p = payload as ServerHitPayload;
  return { kind: "hit", operation_id: p.operation_id, hit: serverHit(p.hit) };
}

export function finishedSignal(payload: unknown): SearchSignal {
  const p = payload as OperationFinishedPayload;
  return {
    kind: "finish",
    operation_id: p.operation_id,
    state: p.state,
    result: p.result ?? null,
    error: p.error?.message ?? null,
  };
}

export function settledSignal(operation_id: string, status: OperationStatus): SearchSignal {
  return {
    kind: "finish",
    operation_id,
    state: status.state,
    result: status.result,
    error: status.error?.message ?? null,
  };
}

/** `search_server_start` answered: adopt the id and replay what came early. */
export function serverStarted(s: AppState, seq: number, operationId: string): AppState {
  const search = s.search;
  if (!search || search.seq !== seq || search.mode !== "server" || search.status !== "searching") return s;
  let next: SearchState = { ...search, operationId, status: "running", early: [] };
  for (const sig of search.early) next = applySignal(next, sig);
  return { ...s, search: next };
}

/**
 * `message_fetch` of the server-only hit `key` answered: the hit is the row
 * it landed in, as the TUI's overlay resolves the hit, and the reader opens
 * it when the cursor is still on it. The row reaches its mailbox's list
 * through the counts invalidation the daemon publishes.
 */
export function hitFetched(s: AppState, key: string, outcome: FetchOutcome): AppState {
  const text = outcome.already_present ? "Already in the local store" : "Fetched into the local store";
  const next = pushNotice(s, { kind: "applied", account: outcome.account, text });
  const search = next.search;
  const hit = search?.hits.find((h) => h.key === key);
  if (!search || !hit || search.account !== outcome.account) return next;
  const fetched = { ...hit, row_id: outcome.row_id, selector: outcome.selector };
  const hits = search.hits.map((h) => (h === hit ? fetched : h));
  const withHits: AppState = { ...next, search: { ...search, hits } };
  if (next.selection.hit !== key || hit.message_id === null) return withHits;
  const message = { row_id: outcome.row_id, message_id: hit.message_id, selector: outcome.selector, verified: true };
  return { ...withHits, selection: { ...withHits.selection, hit: null, draft: null, message } };
}
