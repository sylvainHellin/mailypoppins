// Search: the hit shapes and the reducer's search transitions. Local search
// is one `search_local` answer; a server search is an operation whose hits
// stream as `message.server_hit` events and which ends with
// `operation.finished`, or with `operation_settled` / `operation_dropped`
// when a re-bootstrap re-queried it (docs/rust-layer.md, "The event stream").

import type { Dispatch } from "react";
import type { FetchOutcome, LocalSearchHit, SearchForm } from "@/lib/gui-types";
import { pushNotice } from "@/app/pending";
import { withNotice } from "@/app/activity";
import type { Action } from "@/app/reducer";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";
import type {
  OperationFinishedPayload,
  OperationStatus,
  ServerHitPayload,
  ServerSearchHit,
  ServerSearchSettled,
} from "@/protocol/types";
import {
  type AppState,
  type MessageRef,
  type SearchFormState,
  type SearchHit,
  type SearchScope,
  type SearchSignal,
  type SearchState,
} from "@/app/state";

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

/**
 * What a run searches beyond its query: the account (else the shown
 * search's, else the selection's), one mailbox (else the shown search's when
 * the query is the same, else the whole account), whether a server leg
 * follows the local pass, and the form it came from.
 */
export type SearchRun = { account?: string; mailbox?: string | null; chain?: boolean; form?: SearchFormState | null };

/** Start a run: a new query, or the server leg of the one shown. */
export function startSearch(s: AppState, mode: "local" | "server", query: string, run: SearchRun = {}): AppState {
  const account = run.account ?? s.search?.account ?? s.selection.account;
  const q = query.trim();
  if (!account || !q) return s;
  const prev = s.search;
  // The same query again (the header's server button over a form search)
  // keeps the scope and the form it was run with.
  const same = prev !== null && prev.account === account && prev.query === q;
  const mailbox = run.mailbox !== undefined ? run.mailbox : same ? prev.mailbox : null;
  const form = run.form !== undefined ? run.form : same ? prev.form : null;
  // The server leg of a finished local search keeps its hits and asks the
  // server only for what the store does not already have, as the TUI does.
  const keep =
    mode === "server" && same && prev.mode === "local" && prev.status === "done" && prev.mailbox === mailbox
      ? prev.hits
      : [];
  const search: SearchState = {
    query: q,
    account,
    mode,
    hits: keep,
    operationId: null,
    status: "searching",
    seq: s.searchSeq + 1,
    error: null,
    summary: null,
    early: [],
    restore: prev?.restore ?? { selection: s.selection, focus: s.focus },
    mailbox,
    chain: mode === "local" && (run.chain ?? false),
    form,
  };
  return { ...s, search, searchSeq: search.seq };
}

/**
 * The local pass ended, with hits or refused: a chained search goes on to
 * the server leg, which keeps the hits and excludes them; a refused pass
 * leaves the server to answer alone, as the TUI's overlay does.
 */
export function afterLocalPass(s: AppState): AppState {
  const search = s.search;
  if (!search || search.mode !== "local" || !search.chain) return s;
  if (search.status !== "done" && search.status !== "failed") return s;
  return startSearch(s, "server", search.query, { mailbox: search.mailbox, form: search.form });
}

/** The words the notice line says when the store cannot answer a query the list header's field ran. */
export const REROUTED_NOTICE = "The store's index cannot answer this query, so the server searches it.";

/**
 * `search_local` refused a query the store's index cannot answer (recipient
 * and message-id terms, an OR group with a date or attachment term, or one
 * mixing a filename with another term): the same query goes to the server,
 * which can, scoped to the query's own `in:` mailbox as the form's run is,
 * and the notice line says why (PERSO-115).
 */
export function localRerouted(s: AppState, seq: number, inMailbox: string | null = null): AppState {
  const search = s.search;
  if (!search || search.seq !== seq || search.mode !== "local" || search.status !== "searching") return s;
  const mailbox = search.mailbox ?? inMailbox;
  return withNotice(startSearch(s, "server", search.query, { mailbox, form: search.form }), REROUTED_NOTICE);
}

// ---------------------------------------------------------------------------
// The Server Search form (`ff`): the TUI overlay's fields, whose results
// land in the list pane as every search's do.
// ---------------------------------------------------------------------------

export const EMPTY_SEARCH_FORM: SearchForm = {
  from: "",
  to: "",
  subject: "",
  keywords: "",
  after: "",
  before: "",
  attachment: false,
  advanced: "",
};

export const SCOPE_LABEL: Record<SearchScope, string> = { mailbox: "Current Mailbox", account: "Current Account" };

/** The TUI's words for a Current Mailbox scope with no server folder to search. */
export const NO_MAILBOX_TARGET = "Current mailbox has no server search target";

/** A non-blank Advanced line takes over: the structured fields are greyed and ignored. */
export function advancedActive(fields: SearchForm): boolean {
  return fields.advanced.trim() !== "";
}

/**
 * Open the form on the account a search would run on. It opens with the
 * shown search's form when it came from one, else blank with the list
 * header's typed text as its keywords; the scope is the account, the TUI's
 * default, unless the shown form searched one mailbox.
 */
export function openServerSearch(s: AppState): AppState {
  const account = s.search?.account ?? s.selection.account;
  if (!account) return withNotice(s, "No account is selected");
  const slug = s.search ? s.search.restore.selection.mailbox : s.selection.mailbox;
  const row = slug ? mailboxRow(s, account, slug) : null;
  const mailbox = row && row.role !== "drafts" ? row.label : null;
  const typed = s.filter.trim();
  const form: SearchFormState = s.search?.form ?? {
    fields: { ...EMPTY_SEARCH_FORM, keywords: typed && !s.search ? typed : "" },
    scope: "account",
  };
  return {
    ...s,
    overlay: "server_search",
    serverSearchDialog: { account, mailbox, form },
    dialog: null,
    composeDialog: null,
    attachDialog: null,
    rsvpDialog: null,
    inviteDialog: null,
    signaturesDialog: null,
  };
}

/** An account's mailbox row by slug: the listing once loaded, else the snapshot's. */
function mailboxRow(s: AppState, account: string, slug: string): { role: string; label: string } | null {
  const rows = s.mailboxes[account]?.data?.mailboxes ?? s.bootstrap?.snapshot.mailboxes[account] ?? [];
  const row = rows.find((m) => m.slug === slug);
  return row ? { role: row.role, label: row.label } : null;
}

/** The form's run, once `search_query` built its query and the scope resolved. */
export type FormRun = { account: string; query: string; local: boolean; mailbox: string | null; form: SearchFormState };

/** Run the form: the overlay closes and the list pane shows the results. */
export function runForm(s: AppState, run: FormRun): AppState {
  const closed: AppState = { ...s, overlay: null, serverSearchDialog: null };
  // The store answers first when its index can, then the server leg runs
  // with the hits excluded; a query only the server can answer goes there
  // at once.
  return startSearch(closed, run.local ? "local" : "server", run.query, {
    account: run.account,
    mailbox: run.mailbox,
    chain: run.local,
    form: run.form,
  });
}

/**
 * Build the form's query with `search_query` and run it, or say why not:
 * the parser's refusal verbatim, the TUI's prompt for a blank form, or its
 * words for a mailbox scope with nothing to search. Null once it runs.
 */
export async function submitServerSearch(
  dispatch: Dispatch<Action>,
  dialog: { account: string; mailbox: string | null },
  form: SearchFormState,
): Promise<string | null> {
  let plan;
  try {
    plan = await cmd.searchQuery(form.fields);
  } catch (e) {
    return asGuiError(e).message;
  }
  if (plan.kind === "blank") return "Fill the form and press Enter to search";
  if (plan.kind === "invalid") return plan.message;
  // The query's own `in:` names the mailbox; else the scope toggle decides.
  let mailbox: string | null = plan.in_mailbox;
  if (mailbox === null && form.scope === "mailbox") {
    if (dialog.mailbox === null) return NO_MAILBOX_TARGET;
    mailbox = dialog.mailbox;
  }
  dispatch({ type: "search_run", run: { account: dialog.account, query: plan.query, local: plan.local, mailbox, form } });
  return null;
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
  // A daemon that predates PERSO-131 sends neither `coverage` nor `local_terms`.
  const r = result as Partial<Record<keyof ServerSearchSettled, unknown>>;
  const localTerms =
    r.coverage === "local" && Array.isArray(r.local_terms)
      ? r.local_terms.filter((t): t is string => typeof t === "string")
      : [];
  return {
    hits: typeof r.hits === "number" ? r.hits : 0,
    unreachable: Array.isArray(r.unreachable) ? r.unreachable.length : 0,
    localTerms,
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
