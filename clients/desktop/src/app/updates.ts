// The app's own update (ticket 0139, stage 1; clients/desktop/docs/shell.md,
// "Updates"): the check, the install with its progress, and the restart into
// the new version. The Rust layer checks and installs (docs/rust-layer.md,
// "Updates"); the window shows what it knows in `state.update`, the sidebar
// entry, the palette rows, the Settings line and the activity card. Pure
// functions over the model first, which the reducer routes to, then what the
// entry points run.

import { useEffect, useRef, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { logActivity, withNotice } from "@/app/activity";
import { closeEditors, runningEditors } from "@/app/compose";
import type { AppState, UpdateState } from "@/app/state";
import type { ActionId, PaletteEntry } from "@/keymap/catalog";
import * as cmd from "@/lib/commands";
import { onUpdateAvailable, onUpdateCheckRequested } from "@/lib/events";
import { asGuiError, type UpdateAvailable, type UpdateCheck, type UpdateProgress, type UpdateStatus } from "@/lib/gui-types";
import { Channel } from "@/lib/tauri";

/** Where "Open the release page" goes after a failed install. */
export const RELEASE_PAGE = "https://github.com/sylvainHellin/mailypoppins/releases/latest";

/** The version an update entry names, or null while none is known. */
export function knownVersion(u: UpdateState): string | null {
  return u.kind === "idle" ? null : u.version;
}

/** Whether an install can start: an update is available, or the last install of it failed. */
export function installable(u: UpdateState): u is Extract<UpdateState, { kind: "available" | "failed" }> {
  return u.kind === "available" || u.kind === "failed";
}

/** The sidebar entry's label, or null while it is hidden (nothing known, or a download running). */
export function updateEntryLabel(u: UpdateState): string | null {
  if (installable(u)) return `Update to ${u.version}`;
  if (u.kind === "installed") return "Restart to finish the update";
  return null;
}

/** The palette rows an update adds beside "Check for updates", which is always there. */
export function updatePaletteEntries(u: UpdateState): PaletteEntry[] {
  const row = (label: string, id: ActionId): PaletteEntry => ({ section: "APP", label, keys: [], id, badge: null });
  if (installable(u)) return [row(`Update to v${u.version}`, "update_install")];
  if (u.kind === "installed") return [row("Restart to finish the update", "update_restart")];
  return [];
}

/** The line a manual check leaves on the notice line, at its level. */
export function checkNotice(check: UpdateCheck): { text: string; level: "info" | "warning" | "error" } {
  switch (check.state) {
    case "up_to_date":
      return { text: `mailypoppins ${check.current} is up to date`, level: "info" };
    case "available":
      return { text: `Update ${check.version ?? ""} available`, level: "info" };
    case "disabled":
      return { text: check.reason ?? "Updates are off in this build.", level: "warning" };
    case "failed":
      return { text: `The update check failed: ${check.reason ?? "no reason given"}`, level: "error" };
  }
}

/**
 * An update became known, from the silent check's event or a check's
 * answer. A download or an installed update keeps its state: the version it
 * installs is the one the Rust layer holds.
 */
export function updateFound(s: AppState, found: UpdateAvailable): AppState {
  if (s.update.kind === "downloading" || s.update.kind === "installed") return s;
  if (s.update.kind === "available" && s.update.version === found.version && s.update.notes === found.notes && s.update.date === found.date) return s;
  return { ...s, update: { kind: "available", version: found.version, notes: found.notes, date: found.date } };
}

/**
 * A check answered. It records the running version and the last check for
 * Settings; `available` makes the update known and `up_to_date` forgets one
 * that is not downloading or installed, since the Rust layer dropped it too.
 * A manual check says how it went on the notice line; a silent one says
 * nothing, and its failure is only in the Rust layer's log.
 */
export function updateChecked(s: AppState, check: UpdateCheck, manual: boolean): AppState {
  const reason = check.state === "disabled" ? (check.reason ?? "Updates are off in this build.") : null;
  let next: AppState = { ...s, updateInfo: { current: check.current, last_check: check.last_check ?? s.updateInfo?.last_check ?? null, reason } };
  if (check.state === "available" && check.version) {
    next = updateFound(next, { version: check.version, notes: check.notes, date: check.date });
  } else if (check.state === "up_to_date" && installable(next.update)) {
    next = { ...next, update: { kind: "idle" } };
  }
  if (!manual) return next;
  const { text, level } = checkNotice(check);
  return withNotice(next, text, level);
}

/**
 * `update_status` answered: it records the running version, the last check
 * and why this build never checks, for Settings. With nothing known yet in
 * this window (a start or a webview reload), it seeds the update the Rust
 * layer already has: one this run installed waits for the restart, one it
 * holds is available. Any other state is the window's own and stays.
 */
export function updateStatusRead(s: AppState, status: UpdateStatus): AppState {
  const next: AppState = {
    ...s,
    updateInfo: { current: status.current, last_check: status.last_check ?? null, reason: status.enabled ? null : (status.reason ?? "Updates are off in this build.") },
  };
  if (next.update.kind !== "idle") return next;
  if (status.installed) return { ...next, update: { kind: "installed", version: status.installed, asking: false } };
  if (status.available) return { ...next, update: { kind: "available", version: status.available } };
  return next;
}

/** `update_install` was called for `version`: the card shows the download. */
export function updateInstallStarted(s: AppState, version: string): AppState {
  return { ...s, update: { kind: "downloading", version, downloaded: 0 } };
}

/** One message of the install's channel; `finished` means the new bundle is in place. */
export function updateProgressed(s: AppState, p: UpdateProgress): AppState {
  const u = s.update;
  if (u.kind !== "downloading") return s;
  switch (p.type) {
    case "started":
      return { ...s, update: { ...u, downloaded: 0, content_length: p.content_length } };
    case "progress":
      return { ...s, update: { ...u, downloaded: p.downloaded, content_length: p.content_length ?? u.content_length } };
    case "finished":
      return updateInstalled(s);
  }
}

/** The install resolved (or its channel said `finished`): the card asks Restart now or Later. */
export function updateInstalled(s: AppState): AppState {
  const u = s.update;
  if (u.kind !== "downloading") return s;
  return logActivity({ ...s, update: { kind: "installed", version: u.version, asking: true } }, "info", `mailypoppins ${u.version} is installed; a restart runs it`);
}

/** The install rejected: the card shows why, and the update can be installed again. */
export function updateInstallFailed(s: AppState, reason: string): AppState {
  const u = s.update;
  if (u.kind !== "downloading") return s;
  return logActivity({ ...s, update: { kind: "failed", version: u.version, reason } }, "error", `The update to ${u.version} failed: ${reason}`);
}

/** The card's Later: the update stays installed, and the sidebar entry offers the restart. */
export function updateLater(s: AppState): AppState {
  return s.update.kind === "installed" && s.update.asking ? { ...s, update: { ...s.update, asking: false } } : s;
}

/** The failure card's Dismiss: the update is available again. */
export function updateFailureDismissed(s: AppState): AppState {
  const u = s.update;
  return u.kind === "failed" ? { ...s, update: { kind: "available", version: u.version } } : s;
}

// ---------------------------------------------------------------------------
// What the entry points run
// ---------------------------------------------------------------------------

/** A string rejection is the Rust layer's sentence; anything else goes through `asGuiError`. */
export function updateReason(e: unknown): string {
  return typeof e === "string" ? e : asGuiError(e).message;
}

/** Ask the Rust layer whether a newer app exists; `manual` says so on the notice line. */
export async function checkForUpdates(dispatch: Dispatch<Action>, manual: boolean): Promise<void> {
  let check: UpdateCheck;
  try {
    check = await cmd.updateCheck(manual);
  } catch (e: unknown) {
    // `update_check` never rejects; a bridge failure still deserves a line.
    if (manual) dispatch({ type: "notice", text: `The update check failed: ${updateReason(e)}`, level: "error" });
    return;
  }
  dispatch({ type: "update_checked", check, manual });
}

/** Read what the Rust layer knows about updates, without a check; a bridge failure leaves the state as it is. */
export async function readUpdateStatus(dispatch: Dispatch<Action>): Promise<void> {
  let status: UpdateStatus;
  try {
    status = await cmd.updateStatus();
  } catch {
    return;
  }
  dispatch({ type: "update_status", status });
}

/**
 * The sidebar entry, the palette row and the failure card's retry: an
 * installed update restarts, an available or failed one installs, with its
 * progress on the activity card.
 */
export async function installUpdate(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const u = s.update;
  if (u.kind === "installed") return restartIntoUpdate(s, dispatch);
  if (u.kind === "downloading") {
    dispatch({ type: "notice", text: `mailypoppins ${u.version} is already downloading` });
    return;
  }
  if (!installable(u)) {
    dispatch({ type: "notice", text: "No update is known; Check for updates first" });
    return;
  }
  dispatch({ type: "update_install_started", version: u.version });
  const channel = new Channel<UpdateProgress>();
  channel.onmessage = (progress) => dispatch({ type: "update_progress", progress });
  try {
    await cmd.updateInstall(channel);
  } catch (e: unknown) {
    dispatch({ type: "update_install_failed", reason: updateReason(e) });
    return;
  }
  // The channel's `finished` may land after the answer: either one ends the download.
  dispatch({ type: "update_installed" });
}

/**
 * Restart into the installed update: with an embedded editor running, the
 * leave question asks first, as a window close does, and its "Close the
 * editor" comes back to `restartNow`.
 */
export async function restartIntoUpdate(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  if (s.update.kind !== "installed") {
    dispatch({ type: "notice", text: "No update is waiting for a restart" });
    return;
  }
  if (runningEditors(s).length > 0) {
    dispatch({ type: "update_restart_asked" });
    return;
  }
  await restartNow(s, dispatch);
}

/** Close every embedded editor, then stop the daemon and relaunch (`update_restart`). */
export async function restartNow(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  dispatch({ type: "overlay", overlay: null });
  await closeEditors(Object.values(s.compose), dispatch);
  try {
    await cmd.updateRestart();
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The restart failed: ${updateReason(e)}`, level: "error" });
  }
}

/** The failure card's "Open the release page", through the opener as every https link. */
export function openReleasePage(dispatch: Dispatch<Action>): void {
  cmd.openExternal(RELEASE_PAGE).catch((e: unknown) => dispatch({ type: "notice", text: `Could not open the release page: ${asGuiError(e).message}`, level: "error" }));
}

/**
 * The updater's app events: `update:available` makes the update known, and
 * the App menu's `update:check_requested` runs `onCheck`, the palette's
 * "Check for updates". On mount it reads `update_status` once, so a window
 * that reloaded still knows an update the Rust layer holds or installed.
 */
export function useUpdateEvents(dispatch: Dispatch<Action>, onCheck: () => void): void {
  const checkRef = useRef(onCheck);
  checkRef.current = onCheck;
  useEffect(() => {
    let live = true;
    const available = onUpdateAvailable((update) => {
      if (live) dispatch({ type: "update_available", update });
    }).catch(() => undefined);
    const requested = onUpdateCheckRequested(() => {
      if (live) checkRef.current();
    }).catch(() => undefined);
    void readUpdateStatus((action) => {
      if (live) dispatch(action);
    });
    return () => {
      live = false;
      void available.then((f) => f?.());
      void requested.then((f) => f?.());
    };
  }, [dispatch]);
}
