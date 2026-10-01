// The Signatures dialog (`cs`, ACC-10): the TUI's signatures overlay. The
// signatures are files shared by every account and the default is per
// account, so the dialog sets and clears the selection account's default.
// The listing is a Loadable per account the dialog and the new-draft wizard
// both read, so a change the dialog makes shows in the wizard's select.
// No event reports a delete (no `signature.removed`), so the dialog reads the
// listing again after each change of its own; `signature.changed` makes
// every listing stale.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { emptyLoadable, markStale, type AppState, type Loadable, type SignaturesDialog } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, fixtureNotice, type GuiError, type SignatureFile, type SignatureListing } from "@/lib/gui-types";

/**
 * The account whose listing an open overlay reads: the Signatures dialog's,
 * or the new-draft or forward wizard's; null when neither is open.
 */
export function signaturesWanted(s: AppState): string | null {
  if (s.overlay === "signatures" && s.signaturesDialog) return s.signaturesDialog.account;
  const kind = s.composeDialog?.kind;
  if (s.overlay === "compose" && s.composeDialog && (kind === "new" || kind === "forward")) return s.composeDialog.account;
  return null;
}

/** Ask for `account`'s listing: created on the first ask, stale again on every later one. */
export function wantSignatures(s: AppState, account: string): AppState {
  const l = s.signatures[account];
  return { ...s, signatures: { ...s.signatures, [account]: l ? markStale(l) : emptyLoadable<SignatureListing>() } };
}

/** Every listing is stale: the files are global, so a change reaches every account's. */
export function staleAllSignatures(s: AppState): AppState {
  const entries = Object.entries(s.signatures);
  if (entries.length === 0) return s;
  return { ...s, signatures: Object.fromEntries(entries.map(([a, l]) => [a, markStale(l)])) };
}

/** Forget a removed account's listing, and close its dialog. */
export function dropSignatures(s: AppState, account: string): AppState {
  if (!(account in s.signatures) && s.signaturesDialog?.account !== account) return s;
  const signatures = { ...s.signatures };
  delete signatures[account];
  const closing = s.signaturesDialog?.account === account;
  return {
    ...s,
    signatures,
    signaturesDialog: closing ? null : s.signaturesDialog,
    overlay: closing && s.overlay === "signatures" ? null : s.overlay,
  };
}

function loadableOf(s: AppState, account: string): Loadable<SignatureListing> {
  return s.signatures[account] ?? emptyLoadable<SignatureListing>();
}

export function signaturesLoaded(s: AppState, account: string, gen: number, listing: SignatureListing): AppState {
  const l = loadableOf(s, account);
  return { ...s, signatures: { ...s.signatures, [account]: { ...l, data: listing, loadedGen: gen, error: null } } };
}

export function signaturesFailed(s: AppState, account: string, gen: number, error: GuiError): AppState {
  const l = loadableOf(s, account);
  return { ...s, signatures: { ...s.signatures, [account]: { ...l, loadedGen: gen, error } } };
}

/** Open the dialog for `account`, whose listing is read again. */
export function openSignaturesDialog(s: AppState, account: string): AppState {
  const dialog: SignaturesDialog = { account };
  return wantSignatures(
    { ...s, overlay: "signatures", signaturesDialog: dialog, dialog: null, composeDialog: null, attachDialog: null, rsvpDialog: null, inviteDialog: null },
    account,
  );
}

/** `cs` and the palette's "Manage signatures": the dialog, for the selection's account. */
export function manageSignatures(s: AppState, dispatch: Dispatch<Action>): void {
  const account = s.outboxView?.account ?? s.search?.account ?? s.selection.account;
  if (!account) {
    dispatch({ type: "notice", text: "No account is selected" });
    return;
  }
  dispatch({ type: "open_signatures", account });
}

/**
 * What one dialog change came to: `ok` with the notice (the TUI's status
 * line), or the sentence to show in the dialog.
 */
export type SignatureOutcome<T> = { ok: true; value: T; notice: string } | { ok: false; error: string };

async function change<T>(dispatch: Dispatch<Action>, what: string, run: Promise<T>, notice: (v: T) => string): Promise<SignatureOutcome<T>> {
  try {
    const value = await run;
    const text = notice(value);
    dispatch({ type: "signatures_changed" });
    dispatch({ type: "notice", text });
    return { ok: true, value, notice: text };
  } catch (e: unknown) {
    // A refusal still re-reads: another window may have changed the files.
    dispatch({ type: "signatures_changed" });
    return { ok: false, error: `${what}: ${asGuiError(e).message}` };
  }
}

/** Enter on the list: make `name` the default, or clear the default when it already is. */
export function toggleDefault(dispatch: Dispatch<Action>, account: string, name: string, isDefault: boolean): Promise<SignatureOutcome<SignatureListing>> {
  return change(dispatch, "Cannot set the default signature", cmd.signatureSetDefault(account, isDefault ? null : name), () =>
    isDefault ? `'${name}' is no longer the default signature` : `'${name}' is now the default signature`,
  );
}

/** The name field of `n`: create an empty signature; the caller opens it in the editor. */
export function createSignature(dispatch: Dispatch<Action>, name: string): Promise<SignatureOutcome<SignatureFile>> {
  return change(dispatch, "Cannot create", cmd.signatureCreate(name), () => `Created signature '${name}'`);
}

/** The name field of `r`, seeded with the old name. */
export function renameSignature(dispatch: Dispatch<Action>, account: string, old: string, name: string): Promise<SignatureOutcome<SignatureListing>> {
  return change(dispatch, "Cannot rename", cmd.signatureRename(account, old, name), () => `Renamed '${old}' to '${name}'`);
}

/** The confirmed `d`. */
export function deleteSignature(dispatch: Dispatch<Action>, account: string, name: string): Promise<SignatureOutcome<SignatureListing>> {
  return change(dispatch, "Cannot delete", cmd.signatureDelete(account, name), () => `Deleted signature '${name}'`);
}

/**
 * `e`, and the step after a create: the file opens in the resolved editor,
 * whose saves come back as `signature.changed`. Null once it started, else
 * the sentence to show.
 */
export async function editSignature(dispatch: Dispatch<Action>, path: string, name: string): Promise<string | null> {
  try {
    const launch = await cmd.editorOpen(path);
    dispatch({ type: "notice", text: fixtureNotice(launch) ?? `Editing signature '${name}' in ${launch.editor}` });
    return null;
  } catch (e: unknown) {
    return `Cannot open signature: ${asGuiError(e).message}`;
  }
}

/** The dialog's title for the confirm before a delete, the TUI's. */
export const deleteTitle = (name: string): string => `Delete signature '${name}'?`;

/** Where the cursor goes when `name` leaves a list of `names`: the next row, else the one before. */
export function cursorAfterRemoval(names: readonly string[], name: string): string | null {
  const at = names.indexOf(name);
  if (at < 0) return names[0] ?? null;
  return names[at + 1] ?? names[at - 1] ?? null;
}

/** The cursor a listing keeps: the one asked for when it is listed, else the first row. */
export function keptCursor(names: readonly string[], cursor: string | null): string | null {
  return cursor !== null && names.includes(cursor) ? cursor : (names[0] ?? null);
}
