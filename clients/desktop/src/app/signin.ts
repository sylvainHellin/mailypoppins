// The device-code sign-in (ACC-06, INT-04; clients/desktop/docs/shell.md,
// "Account wizard"): `config_oauth2_login` awaited as `oauth2_login`, its
// one `operation.progress` (phase `device_code`, message "<url> <code>")
// shown by the device-code dialog, and how it ended. One sign-in at a time,
// the one the `device_code` overlay shows. Pure functions over the model,
// which the reducer routes to, and at the bottom the commands the wizard,
// the Settings view and the dialog run.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { pushNotice } from "@/app/pending";
import type { AppState, OperationEnd, SignIn } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, type OAuth2Stored } from "@/lib/gui-types";
import type { Bootstrap } from "@/protocol/types";

/** The phase the daemon reports the device code under (`DEVICE_CODE_PHASE`). */
export const DEVICE_CODE_PHASE = "device_code";

/** The line a finished sign-in leaves: `mp_client::format::oauth2_stored_line`, without its check mark. */
export function storedTokenLine(account: string): string {
  return `OAuth2 token acquired and cached for account '${account}'`;
}

/** What the dialog and the notice say once Cancel took: the daemon stops waiting, the provider does not. */
export function cancelledLine(account: string): string {
  return `The sign-in of ${account} was cancelled. The provider's poll runs on, so a sign-in you finish in the browser may still complete and store the token.`;
}

export function failedLine(account: string, why: string): string {
  return `The sign-in of ${account} failed: ${why}`;
}

export function droppedLine(account: string, why: string): string {
  return `The sign-in of ${account} was interrupted: ${why}`;
}

/** The verification URL and the user code of a device-code progress, split on its one space. */
export type DeviceCode = { url: string; code: string };

export function parseDeviceCode(message: string | null | undefined): DeviceCode | null {
  const text = (message ?? "").trim();
  const at = text.indexOf(" ");
  if (at <= 0) return null;
  const url = text.slice(0, at);
  const code = text.slice(at + 1).trim();
  return code ? { url, code } : null;
}

/** The device code the sign-in's operation reported, once it has. */
export function deviceCodeOf(s: AppState): DeviceCode | null {
  const id = s.signIn?.operation_id;
  const p = id ? s.progress[id] : undefined;
  if (!p || p.phase !== DEVICE_CODE_PHASE) return null;
  return parseDeviceCode(p.message);
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

const SIGN_IN_EARLY_CAP = 16;

/** Whether a sign-in runs: asked for, or started and not ended. */
export function signInRunning(s: AppState): boolean {
  return s.signIn !== null && s.signIn.outcome === null;
}

/** Whether the sign-in's start has not answered yet. */
export function signInStarting(s: AppState): boolean {
  return s.signIn !== null && s.signIn.outcome === null && s.signIn.operation_id === null;
}

export function isSignInOperation(s: AppState, operationId: string): boolean {
  return s.signIn?.operation_id === operationId && s.signIn.outcome === null;
}

/** Open the device-code dialog on a new sign-in of `account`. */
export function signInRequested(s: AppState, token: number, account: string): AppState {
  return {
    ...s,
    overlay: "device_code",
    signIn: { token, account, operation_id: null, cancelling: false, outcome: null },
    signInEarly: [],
    dialog: null,
    composeDialog: null,
    attachDialog: null,
    rsvpDialog: null,
    inviteDialog: null,
    signaturesDialog: null,
    passwordDialog: null,
    accountWizard: null,
  };
}

function ended(s: AppState, outcome: NonNullable<SignIn["outcome"]>): AppState {
  if (!s.signIn) return s;
  const next: AppState = { ...s, signIn: { ...s.signIn, outcome, cancelling: false }, signInEarly: [] };
  const kind = outcome.kind === "stored" || outcome.kind === "cancelled" ? "applied" : "failed";
  return pushNotice(next, { kind, account: s.signIn.account, text: outcome.text });
}

function signInEnded(s: AppState, end: OperationEnd): AppState {
  const account = s.signIn?.account ?? "";
  if ("dropped" in end) return ended(s, { kind: "failed", text: droppedLine(account, end.dropped) });
  if (end.state === "succeeded") {
    const stored = (end.result ?? null) as OAuth2Stored | null;
    return ended(s, { kind: "stored", text: storedTokenLine(stored?.account || account) });
  }
  if (end.state === "cancelled") return ended(s, { kind: "cancelled", text: cancelledLine(account) });
  return ended(s, { kind: "failed", text: failedLine(account, end.error ?? end.state) });
}

/** An operation ended: the sign-in settles; while its start is unanswered an unknown id waits for it. */
export function signInSignal(s: AppState, end: OperationEnd): AppState {
  if (isSignInOperation(s, end.operation_id)) return signInEnded(s, end);
  if (signInStarting(s)) return { ...s, signInEarly: [...s.signInEarly, end].slice(-SIGN_IN_EARLY_CAP) };
  return s;
}

/** `config_oauth2_login` answered: await the id, or settle it now if its end came first. */
export function signInStarted(s: AppState, token: number, operationId: string): AppState {
  if (!s.signIn || s.signIn.token !== token || s.signIn.outcome) return s;
  const next: AppState = { ...s, signIn: { ...s.signIn, operation_id: operationId } };
  const early = next.signInEarly.find((e) => e.operation_id === operationId);
  return early ? signInEnded({ ...next, signInEarly: [] }, early) : { ...next, signInEarly: [] };
}

/** `config_oauth2_login` was refused (an unknown account, a password account, no client): the dialog says why. */
export function signInStartFailed(s: AppState, token: number, message: string): AppState {
  if (!s.signIn || s.signIn.token !== token) return s;
  return ended(s, { kind: "failed", text: failedLine(s.signIn.account, message) });
}

export function signInCancelling(s: AppState): AppState {
  return s.signIn && s.signIn.outcome === null ? { ...s, signIn: { ...s.signIn, cancelling: true } } : s;
}

/** The dialog closed on an ended sign-in: it is forgotten. */
export function signInClosed(s: AppState): AppState {
  if (signInRunning(s)) return s;
  return { ...s, signIn: null, signInEarly: [], overlay: s.overlay === "device_code" ? null : s.overlay };
}

/**
 * A bootstrap of the same daemon lists the operations it has not settled,
 * each with its last progress: the sign-in's device code survives a missed
 * `operation.progress` that way. Another daemon's operation ids mean
 * nothing here; the Rust layer drops the sign-in then.
 */
export function signInRebootstrapped(s: AppState, bootstrap: Bootstrap, sameInstance: boolean): AppState {
  const id = s.signIn?.operation_id;
  if (!sameInstance || !id || s.signIn?.outcome) return s;
  const status = bootstrap.snapshot.operations.find((o) => o.operation_id === id);
  if (!status?.progress) return s;
  return { ...s, progress: { ...s.progress, [id]: status.progress } };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

let nextSignIn = 1;

/** Sign `account` in with a device code: the dialog opens at once and shows the code once the daemon reports it. */
export async function startSignIn(dispatch: Dispatch<Action>, account: string): Promise<void> {
  const token = nextSignIn++;
  dispatch({ type: "sign_in_requested", token, account });
  try {
    const { operation_id } = await cmd.configOauth2Login(account);
    dispatch({ type: "sign_in_started", token, operation_id });
  } catch (e: unknown) {
    dispatch({ type: "sign_in_failed", token, error: asGuiError(e) });
  }
}

/**
 * Settings' Sign in: a sign-in already running is shown again rather than
 * started twice (the daemon would run two provider polls); another
 * account's running one is shown with a notice.
 */
export function signInOrShow(s: AppState, dispatch: Dispatch<Action>, account: string): void {
  if (signInRunning(s) && s.signIn) {
    if (s.signIn.account !== account) {
      dispatch({ type: "notice", text: `The sign-in of ${s.signIn.account} is still running; finish or cancel it first` });
    }
    dispatch({ type: "overlay", overlay: "device_code" });
    return;
  }
  void startSignIn(dispatch, account);
}

/**
 * Cancel the running sign-in: `operation.cancel` settles it `cancelled`,
 * whose finish ends it and says the provider may still complete it. One
 * whose start has not answered cannot be named yet; the dialog calls this
 * again once it has an id.
 */
export async function cancelSignIn(dispatch: Dispatch<Action>, operationId: string | null): Promise<void> {
  dispatch({ type: "sign_in_cancelling" });
  if (!operationId) return;
  try {
    await cmd.configOauth2Cancel(operationId);
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The sign-in could not be cancelled: ${asGuiError(e).message}`, level: "error" });
  }
}
