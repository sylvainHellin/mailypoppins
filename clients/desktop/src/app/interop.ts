// The handoffs out of the window (INT-01, INT-02, INT-03): config.toml and the
// daemon log in the external editor (`sc`, `sf`), and the reader's copies of
// the sender's address, the `mp://` link and the subject.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import type { AppState } from "@/app/state";
import type { MessageMeta } from "@/lib/gui-types";
import { copyText } from "@/lib/clipboard";
import * as cmd from "@/lib/commands";
import { asGuiError, type EditorLaunch } from "@/lib/gui-types";

/**
 * Run one editor handoff and say how it went: the editor it started, the
 * Rust layer's sentence when the file is not there (a warning, as the TUI's
 * "No log file found"), or the failure (an error).
 */
async function openInEditor(dispatch: Dispatch<Action>, what: string, failed: string, run: () => Promise<EditorLaunch>): Promise<void> {
  try {
    const launch = await run();
    dispatch({ type: "notice", text: `Opened ${what} in ${launch.editor}` });
  } catch (e: unknown) {
    const error = asGuiError(e);
    if (error.kind === "not_found") dispatch({ type: "notice", text: error.message, level: "warning" });
    else dispatch({ type: "notice", text: `${failed}: ${error.message}`, level: "error" });
  }
}

/** `sc`: the daemon's config.toml (`config.get`'s path) in the external editor. */
export function openConfig(dispatch: Dispatch<Action>): Promise<void> {
  return openInEditor(dispatch, "config.toml", "Open config failed", cmd.configOpen);
}

/** `sf`: the daemon's log file (`diagnostic.log_path`) in the external editor. */
export function openLog(dispatch: Dispatch<Action>): Promise<void> {
  return openInEditor(dispatch, "the daemon log", "Open log failed", cmd.logOpen);
}

/**
 * The address of a `From:` header: what its angle brackets hold, else the
 * whole field, trimmed.
 */
export function senderAddress(from: string): string {
  const m = /<([^<>]*)>/.exec(from);
  return (m ? m[1] : from).trim();
}

/** What the reader shows for the selected message, or null when it shows none. */
export function openMeta(s: AppState): MessageMeta | null {
  const meta = s.reader.meta;
  const m = s.selection.message;
  return meta && m && meta.row_id === m.row_id ? meta : null;
}

export type CopyWhat = "sender" | "link" | "subject";

/**
 * Copy one field of the message the reader shows, the Copy menu's and the
 * palette's: the sender's address, the `mp://` link, or the subject. A
 * message with no such field says so instead. Called from the click or the
 * palette's run, synchronously, so the write keeps the user activation.
 */
export function copyFromMessage(meta: MessageMeta | null, what: CopyWhat, dispatch: Dispatch<Action>): void {
  if (!meta) {
    dispatch({ type: "notice", text: "Open a message first" });
    return;
  }
  if (what === "link") {
    void copyText(meta.selector, meta.selector, dispatch);
    return;
  }
  if (what === "sender") {
    const address = meta.from ? senderAddress(meta.from) : "";
    if (!address) return dispatch({ type: "notice", text: "This message has no sender" });
    void copyText(address, address, dispatch);
    return;
  }
  const subject = meta.subject?.trim() ?? "";
  if (!subject) return dispatch({ type: "notice", text: "This message has no subject" });
  void copyText(subject, "the subject", dispatch);
}
