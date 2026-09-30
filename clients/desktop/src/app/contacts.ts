// The Contacts view (clients/desktop/docs/shell.md, "Contacts"): each
// account's ranked contacts from `contact_search` as a Loadable, the view's
// query and cursor, the index rebuilds this window awaits and what their
// ends say. Pure functions over the model, which the reducer routes to, and
// at the bottom what the view's keys run: compose to a contact, send one as
// a vCard, copy an address, rebuild the index.

import type { Dispatch } from "react";
import type { ContactRebuilt, ContactRow, ContactSearch, GuiError } from "@/lib/gui-types";
import { asGuiError } from "@/lib/gui-types";
import * as cmd from "@/lib/commands";
import { copyText } from "@/lib/clipboard";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import { draftName, openInEditor } from "@/app/compose";
import {
  emptyLoadable,
  markStale,
  type AppState,
  type ContactsView,
  type Loadable,
  type OperationEnd,
  type RebuildRun,
} from "@/app/state";

/**
 * How many contacts one search answers (D13): the TUI lists the whole
 * index, and the daemon's `contact.search` caps every answer.
 */
export const CONTACT_LIMIT = 1000;

/** The Contacts view's search field, which `/` focuses. */
export const CONTACTS_SEARCH_ID = "mp-contacts-search";

/** How long the search field waits after the last key before it asks. */
export const SEARCH_DEBOUNCE_MS = 150;

/** The score `contact.search` gives every row of an empty query, which ranks by tier and recency instead. */
export const RANK_ONLY_SCORE = 4294967295;

/** What the view says when the index is empty, the TUI's words with a semicolon. */
export const EMPTY_INDEX = "No contacts yet; press r to build the index";
export const NO_MATCH = "No matching contacts.";
export const NO_CONTACT = "No contact selected";

const REBUILD_EARLY_CAP = 16;

// ---------------------------------------------------------------------------
// The lists
// ---------------------------------------------------------------------------

function loadableOf(s: AppState, account: string): Loadable<ContactSearch> {
  return s.contacts[account] ?? emptyLoadable<ContactSearch>();
}

/** Read `account`'s contacts again, if this window has read them. */
export function staleContacts(s: AppState, account: string): AppState {
  const l = s.contacts[account];
  if (!l) return s;
  const next = markStale(l);
  return next === l ? s : { ...s, contacts: { ...s.contacts, [account]: next } };
}

/** A bootstrap reads every list this window has read again, and forgets those of accounts the snapshot no longer has. */
export function staleAllContacts(s: AppState): AppState {
  const names = new Set(s.bootstrap?.snapshot.accounts.map((a) => a.name) ?? []);
  const contacts: AppState["contacts"] = {};
  for (const [account, l] of Object.entries(s.contacts)) if (names.has(account)) contacts[account] = markStale(l);
  const view = s.contactsView && names.has(s.contactsView.account) ? s.contactsView : null;
  return { ...s, contacts, contactsView: view };
}

/** Forget a removed account's contacts. */
export function dropContacts(s: AppState, account: string): AppState {
  if (!(account in s.contacts) && s.contactsView?.account !== account) return s;
  const contacts = { ...s.contacts };
  delete contacts[account];
  return { ...s, contacts, contactsView: s.contactsView?.account === account ? null : s.contactsView };
}

/**
 * An answer asked at an older generation is dropped: it may be for a query
 * typed over, and the read of the current one is already under way.
 */
export function contactsLoaded(s: AppState, account: string, gen: number, search: ContactSearch): AppState {
  const l = loadableOf(s, account);
  if (gen < l.gen) return s;
  return { ...s, contacts: { ...s.contacts, [account]: { ...l, data: search, loadedGen: gen, error: null } } };
}

export function contactsFailed(s: AppState, account: string, gen: number, error: GuiError): AppState {
  const l = loadableOf(s, account);
  if (gen < l.gen) return s;
  return { ...s, contacts: { ...s.contacts, [account]: { ...l, loadedGen: gen, error } } };
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/**
 * Show `account`'s contacts: its Loadable is created on the first open, and
 * the view keeps its query; the cursor stays on the same account only. A
 * list this window read before is read again when the view comes to its
 * account, since it was asked for whatever query the view had then.
 */
export function openContacts(s: AppState, account: string): AppState {
  const prev = s.contactsView;
  if (prev?.account === account && s.contacts[account]) return s;
  const l = s.contacts[account];
  const next = { ...s, contacts: { ...s.contacts, [account]: l ? markStale(l) : emptyLoadable<ContactSearch>() } };
  if (prev?.account === account) return next;
  const view: ContactsView = { account, query: prev?.query ?? "", cursor: null, searching: false };
  return { ...next, contactsView: view };
}

/** While the Contacts view shows, it follows the selection's account. */
export function followContacts(s: AppState): AppState {
  if (s.view !== "contacts") return s;
  const account = s.selection.account;
  if (!account || (s.contactsView?.account === account && s.contacts[account])) return s;
  return openContacts(s, account);
}

/** The view comes back: no event says the index changed, so it reads the list again. */
export function reopenContacts(s: AppState): AppState {
  const next = followContacts(s);
  const account = next.contactsView?.account;
  return account ? staleContacts(next, account) : next;
}

/** The search field's typing paused on `query`: the list is asked for it, the cursor back to the top. */
export function setContactsQuery(s: AppState, query: string): AppState {
  const view = s.contactsView;
  if (!view || view.query === query) return s;
  // A new generation even when a read is under way: that read was for the
  // old query, and its answer must not settle this one.
  const l = loadableOf(s, view.account);
  const contacts = { ...s.contacts, [view.account]: { ...l, gen: l.gen + 1 } };
  return { ...s, contacts, contactsView: { ...view, query, cursor: null } };
}

export function setContactsSearching(s: AppState, searching: boolean): AppState {
  const view = s.contactsView;
  return view && view.searching !== searching ? { ...s, contactsView: { ...view, searching } } : s;
}

/** The rows the view shows now. */
export function contactRows(s: AppState): ContactRow[] {
  const view = s.contactsView;
  if (!view) return [];
  return s.contacts[view.account]?.data?.contacts ?? [];
}

/** The row under the view's cursor: the one it names, else the first. */
export function cursorContact(s: AppState, rows: ContactRow[] = contactRows(s)): ContactRow | null {
  const cursor = s.contactsView?.cursor ?? null;
  return rows.find((c) => c.address === cursor) ?? rows[0] ?? null;
}

/** Move the view's cursor, as the list's `move_selection` does. */
export function moveContactsCursor(s: AppState, to: number | "first" | "last", relative: boolean): AppState {
  const view = s.contactsView;
  if (!view) return s;
  const rows = contactRows(s);
  if (rows.length === 0) return s;
  const cur = Math.max(0, rows.findIndex((c) => c.address === cursorContact(s, rows)?.address));
  let idx: number;
  if (to === "first") idx = 0;
  else if (to === "last") idx = rows.length - 1;
  else idx = relative ? cur + to : to;
  idx = Math.max(0, Math.min(rows.length - 1, idx));
  return { ...s, contactsView: { ...view, cursor: rows[idx].address }, focusSeq: s.focusSeq + 1 };
}

export function selectContact(s: AppState, address: string): AppState {
  const view = s.contactsView;
  return view ? { ...s, contactsView: { ...view, cursor: address } } : s;
}

/** A row's name as the view shows it: the display name, else the address. */
export function contactName(c: ContactRow): string {
  return c.display_name.trim() || c.address;
}

/** The name a vCard draft's subject gives the contact (the TUI's `vcard_display_name`). */
export function vcardName(c: Pick<ContactRow, "address" | "display_name">): string {
  return c.display_name.trim() || c.address.split("@")[0] || c.address;
}

// ---------------------------------------------------------------------------
// The rebuilds
// ---------------------------------------------------------------------------

/** The rebuild this window is running for `account`, if any. */
export function rebuildOf(s: AppState, account: string): RebuildRun | null {
  return s.rebuilds.find((r) => r.account === account) ?? null;
}

export function rebuildRequested(s: AppState, run: Omit<RebuildRun, "operation_id">): AppState {
  return { ...s, rebuilds: [...s.rebuilds, { ...run, operation_id: null }] };
}

export function rebuildStarting(s: AppState): boolean {
  return s.rebuilds.some((r) => r.operation_id === null);
}

export function isRebuildOperation(s: AppState, operationId: string): boolean {
  return s.rebuilds.some((r) => r.operation_id === operationId);
}

/**
 * What a settled rebuild says, the TUI's four lines (`apply_contacts_rebuild`):
 * refreshed with how many it found, or the cache guard's refusal with how
 * many it kept. An unknown verdict reads as written, as the TUI's does.
 */
export function rebuildResult(settled: ContactRebuilt | null): { kind: "applied" | "rebuild_refused"; text: string } {
  const found = settled?.contacts ?? 0;
  const kept = settled?.kept ?? 0;
  switch (settled?.saved) {
    case "refused_empty":
      return { kind: "rebuild_refused", text: `Contacts rebuild found none, kept ${kept} cached` };
    case "refused_shrunk":
      return { kind: "rebuild_refused", text: `Contacts rebuild found only ${found}, kept ${kept} cached` };
    default:
      return { kind: "applied", text: `Contacts refreshed (${found})` };
  }
}

/** A rebuild of this window ended: say how; a written index is read again. */
function rebuildEnded(s: AppState, run: RebuildRun, end: OperationEnd): AppState {
  const next: AppState = { ...s, rebuilds: s.rebuilds.filter((r) => r !== run) };
  const account = run.account;
  if ("dropped" in end) {
    return pushNotice(next, { kind: "failed", account, text: `Contacts refresh failed: ${end.dropped}` });
  }
  if (end.state !== "succeeded") {
    return pushNotice(next, { kind: "failed", account, text: `Contacts refresh failed: ${end.error ?? end.state}` });
  }
  const { kind, text } = rebuildResult((end.result ?? null) as ContactRebuilt | null);
  return pushNotice(kind === "applied" ? staleContacts(next, account) : next, { kind, account, text });
}

/** An operation ended: a rebuild this window awaits settles; while one is unanswered an unknown id waits for it. */
export function rebuildSignal(s: AppState, end: OperationEnd): AppState {
  const run = s.rebuilds.find((r) => r.operation_id === end.operation_id);
  if (run) return rebuildEnded(s, run, end);
  if (rebuildStarting(s)) return { ...s, rebuildEarly: [...s.rebuildEarly, end].slice(-REBUILD_EARLY_CAP) };
  return s;
}

/** `contact_rebuild` answered: await the id, or settle it now if its end came first. */
export function rebuildStarted(s: AppState, token: number, operationId: string): AppState {
  const run = s.rebuilds.find((r) => r.token === token);
  if (!run) return s;
  const started: RebuildRun = { ...run, operation_id: operationId };
  let next: AppState = { ...s, rebuilds: s.rebuilds.map((r) => (r === run ? started : r)) };
  const early = next.rebuildEarly.find((e) => e.operation_id === operationId);
  next = { ...next, rebuildEarly: rebuildStarting(next) ? next.rebuildEarly.filter((e) => e !== early) : [] };
  return early ? rebuildEnded(next, started, early) : next;
}

/** `contact_rebuild` was refused: nothing runs. */
export function rebuildStartFailed(s: AppState, token: number, message: string): AppState {
  const run = s.rebuilds.find((r) => r.token === token);
  if (!run) return s;
  let next: AppState = { ...s, rebuilds: s.rebuilds.filter((r) => r !== run) };
  if (!rebuildStarting(next)) next = { ...next, rebuildEarly: [] };
  return pushNotice(next, { kind: "failed", account: run.account, text: `Contacts refresh failed: ${message}` });
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

let nextRebuild = 1;

function cursorOf(s: AppState, dispatch: Dispatch<Action>): { account: string; contact: ContactRow } | null {
  const view = s.contactsView;
  const contact = view && s.view === "contacts" ? cursorContact(s) : null;
  if (!view || !contact) {
    dispatch({ type: "notice", text: NO_CONTACT });
    return null;
  }
  return { account: view.account, contact };
}

/** Enter and `n`: the new-draft wizard, To filled with the contact (the TUI's `ComposeToContact`). */
export function composeToContact(s: AppState, dispatch: Dispatch<Action>): void {
  const cur = cursorOf(s, dispatch);
  if (!cur) return;
  dispatch({ type: "open_compose", dialog: { kind: "new", account: cur.account, to: cur.contact.recipient } });
}

/** `c`: the contact's address on the clipboard, from the key handler so the write keeps its user activation. */
export function copyContactAddress(s: AppState, dispatch: Dispatch<Action>): void {
  const cur = cursorOf(s, dispatch);
  if (!cur) return;
  void copyText(cur.contact.address, cur.contact.address, dispatch);
}

/**
 * `v`: a new draft to the contact with its vCard attached
 * (`contact_vcard_draft`), then the compose session opens it in the editor.
 */
export async function sendVcard(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const cur = cursorOf(s, dispatch);
  if (!cur) return;
  const { account, contact } = cur;
  const name = draftName(`Contact: ${vcardName(contact)}`);
  let draft;
  try {
    draft = await cmd.contactVcardDraft(account, name, contact.address, contact.display_name);
  } catch (e: unknown) {
    dispatch({ type: "activity", kind: "compose_failed", account, text: `vCard draft failed: ${asGuiError(e).message}` });
    return;
  }
  dispatch({ type: "notice", text: `vCard draft: ${contact.recipient}` });
  await openInEditor(dispatch, draft.draft.account, draft.draft.id, draft.draft.path);
}

/** `r`: rebuild the view's account's index, awaited as `contact_rebuild`; one at a time per account. */
export async function rebuildContacts(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const account = s.contactsView?.account ?? s.selection.account;
  if (!account) return;
  if (rebuildOf(s, account)) {
    dispatch({ type: "notice", text: `The contact index of ${account} is already being rebuilt` });
    return;
  }
  const token = nextRebuild++;
  dispatch({ type: "rebuild_requested", token, account });
  try {
    const { operation_id } = await cmd.contactRebuild(account);
    dispatch({ type: "rebuild_started", token, operation_id });
  } catch (e: unknown) {
    dispatch({ type: "rebuild_failed", token, error: asGuiError(e) });
  }
}
