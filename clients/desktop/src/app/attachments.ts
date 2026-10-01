// Attachments, the browser rendition and the fetch of a server-only hit
// (clients/desktop/docs/reader.md, "Attachments"): the TUI's `to`, `ts`,
// `tb` and `ta`, and its search overlay's `f`, here `F`. What the keys, the
// palette and the buttons run; the dialogs call the helpers at the bottom.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import {
  draftsShown,
  filteredDrafts,
  readerKey,
  sendingRefusal,
  type AppState,
  type AttachmentItem,
  type AttachmentOwner,
  type SearchHit,
} from "@/app/state";
import { isOpenable } from "@/app/search";
import * as cmd from "@/lib/commands";
import { asGuiError, type Attachment, type DraftAttachments } from "@/lib/gui-types";
import { homeDir, openPicker } from "@/lib/tauri";

/** What an attachment key acts on: the cursor's message, a server-only hit, or the Drafts cursor draft. */
type Subject =
  | { kind: "message"; account: string; row_id: number; subject: string }
  | { kind: "hit"; account: string; hit: SearchHit }
  | { kind: "draft"; account: string; draftId: string; subject: string; valid: boolean };

export const FETCH_FIRST = "This message is on the server only: fetch it first (F)";
export const NO_HTML = "No HTML version available";

function notice(dispatch: Dispatch<Action>, text: string): void {
  dispatch({ type: "notice", text });
}

function failed(dispatch: Dispatch<Action>, account: string | null, text: string): void {
  dispatch({ type: "activity", kind: "failed", account, text });
}

/** The hit under the search's cursor, if the cursor is on one with no row. */
function cursorHit(s: AppState): SearchHit | null {
  if (!s.search || !s.selection.hit) return null;
  return s.search.hits.find((h) => h.key === s.selection.hit) ?? null;
}

function cursorSubject(s: AppState): Subject | null {
  const account = s.search?.account ?? s.selection.account;
  if (!account) return null;
  const hit = cursorHit(s);
  if (hit) return { kind: "hit", account: hit.account, hit };
  if (!s.search && draftsShown(s) && s.selection.draft) {
    const row = filteredDrafts(s.messages.data, "").find((d) => d.id === s.selection.draft);
    if (!row) return null;
    const valid = row.valid && row.diagnostic === null;
    return { kind: "draft", account, draftId: row.id, subject: row.subject || "(no subject)", valid };
  }
  const m = s.selection.message;
  if (!m) return null;
  const meta = s.reader.key === readerKey(account, m.row_id) ? s.reader.meta : null;
  return { kind: "message", account, row_id: m.row_id, subject: meta?.subject ?? "(no subject)" };
}

/** A message's parts: the reader's headers when they are this message's, else `message.get`'s. */
async function partsOf(s: AppState, account: string, row_id: number): Promise<Attachment[]> {
  const meta = s.reader.key === readerKey(account, row_id) ? s.reader.meta : null;
  if (meta) return meta.attachments;
  return (await cmd.messageHtmlMeta(account, row_id)).attachments;
}

export function messageItems(parts: Attachment[]): AttachmentItem[] {
  return parts.map((p, part) => ({ part, name: p.name, size: p.size, missing: false }));
}

export function draftItemsOf(list: DraftAttachments): AttachmentItem[] {
  return list.attachments.map((a) => ({
    part: a.index,
    name: a.entry,
    size: null,
    missing: !a.exists,
  }));
}

/**
 * `to`: one attachment opens at once, several open the picker, as the
 * TUI's; a draft lists the files its frontmatter names (ATT-04).
 */
export async function openAttachment(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const subj = cursorSubject(s);
  if (!subj) return;
  if (subj.kind === "hit") return notice(dispatch, isOpenable(subj.hit) ? "Open the result first" : FETCH_FIRST);
  let owner: AttachmentOwner;
  let items: AttachmentItem[];
  try {
    if (subj.kind === "draft") {
      if (!subj.valid) return notice(dispatch, "This draft does not parse; fix it in the editor first");
      owner = { kind: "draft", account: subj.account, draftId: subj.draftId };
      items = draftItemsOf(await cmd.draftAttachments(subj.account, subj.draftId));
    } else {
      owner = { kind: "message", account: subj.account, row_id: subj.row_id };
      items = messageItems(await partsOf(s, subj.account, subj.row_id));
    }
  } catch (e: unknown) {
    return failed(dispatch, subj.account, `Attachments failed: ${asGuiError(e).message}`);
  }
  if (items.length === 0) return notice(dispatch, "No attachments");
  if (items.length === 1) return void openItem(owner, items[0], dispatch);
  dispatch({ type: "open_attachments", dialog: { kind: "open", owner, subject: subj.subject, items } });
}

/** `ts`: the Save dialog, every part checked, the last directory offered. */
export async function saveAttachment(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const subj = cursorSubject(s);
  if (!subj) return;
  if (subj.kind === "hit") return notice(dispatch, isOpenable(subj.hit) ? "Open the result first" : FETCH_FIRST);
  if (subj.kind === "draft") return notice(dispatch, "A draft's attachments are files already; t o opens one");
  let items: AttachmentItem[];
  try {
    items = messageItems(await partsOf(s, subj.account, subj.row_id));
  } catch (e: unknown) {
    return failed(dispatch, subj.account, `Attachments failed: ${asGuiError(e).message}`);
  }
  if (items.length === 0) return notice(dispatch, "No attachments");
  dispatch({ type: "open_attachments", dialog: { kind: "save", account: subj.account, row_id: subj.row_id, subject: subj.subject, items } });
}

/**
 * `tb`: the daemon's rendition of a stored message, or a server-only hit's
 * own markup, in the default browser.
 */
export async function openHtml(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const subj = cursorSubject(s);
  if (!subj) return;
  if (subj.kind === "draft") return notice(dispatch, NO_HTML);
  try {
    if (subj.kind === "hit") {
      const html = subj.hit.source?.html_body;
      if (!html) return notice(dispatch, NO_HTML);
      await cmd.hitHtmlOpen(html);
    } else {
      const file = await cmd.htmlOpen(subj.account, subj.row_id);
      if (!file) return notice(dispatch, NO_HTML);
    }
  } catch (e: unknown) {
    return failed(dispatch, subj.account, `Open failed: ${asGuiError(e).message}`);
  }
  notice(dispatch, "Opened in browser");
}

/** `ta`, Drafts only: the path dialog, until the native picker arrives. */
export function attachFile(s: AppState, dispatch: Dispatch<Action>): void {
  if (!draftsShown(s) || s.search) return notice(dispatch, "Attach file (t a) is only available in Drafts");
  const subj = cursorSubject(s);
  if (!subj || subj.kind !== "draft") return;
  if (!subj.valid) return notice(dispatch, "This draft does not parse; fix it in the editor first");
  const busy = sendingRefusal(s, [{ account: subj.account, draft: subj.draftId }]);
  if (busy) return notice(dispatch, busy);
  dispatch({ type: "open_attachments", dialog: { kind: "attach", account: subj.account, draftId: subj.draftId, subject: subj.subject } });
}

/**
 * `F` on a server-only hit: `message_fetch` ingests it, and the hit becomes
 * the row it landed in. The TUI's words for a hit already stored and one
 * with no Message-ID.
 */
export async function fetchHit(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const hit = cursorHit(s);
  if (!hit) {
    const onRow = s.search && s.selection.message;
    return notice(dispatch, onRow ? "Already in the local store" : "Fetch works on a server-only search result");
  }
  if (isOpenable(hit)) return notice(dispatch, "Already in the local store");
  if (hit.message_id === null) return notice(dispatch, "This hit carries no Message-ID; cannot fetch it");
  if (s.fetching.includes(hit.key)) return;
  dispatch({ type: "hit_fetch_started", key: hit.key });
  notice(dispatch, "Fetching…");
  try {
    const outcome = await cmd.messageFetch(hit.account, hit.mailbox, hit.message_id);
    dispatch({ type: "hit_fetched", key: hit.key, outcome });
  } catch (e: unknown) {
    dispatch({ type: "hit_fetch_failed", key: hit.key, error: asGuiError(e) });
  }
}

// ---------------------------------------------------------------------------
// What the dialogs and the buttons run
// ---------------------------------------------------------------------------

/** Open one attachment; resolves to why it did not, or null. */
export async function openItem(owner: AttachmentOwner, item: AttachmentItem, dispatch: Dispatch<Action>): Promise<string | null> {
  try {
    const file =
      owner.kind === "message"
        ? await cmd.attachmentOpen(owner.account, owner.row_id, item.part)
        : await cmd.draftAttachmentOpen(owner.account, owner.draftId, item.part);
    notice(dispatch, `Opened: ${file.name}`);
    return null;
  } catch (e: unknown) {
    const why = asGuiError(e).message;
    failed(dispatch, owner.account, `Open failed: ${why}`);
    return why;
  }
}

function files(n: number): string {
  return `${n} file${n === 1 ? "" : "s"}`;
}

/**
 * Save `parts` into `dir` as typed; resolves to why nothing was saved, or
 * null. The notice names the directory as the user typed it.
 */
export async function saveParts(
  account: string,
  row_id: number,
  parts: number[],
  dir: string,
  dispatch: Dispatch<Action>,
): Promise<string | null> {
  if (parts.length === 0) return "Pick at least one attachment to save";
  let saved;
  try {
    saved = await cmd.attachmentSave(account, row_id, parts, dir.trim());
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
  dispatch({ type: "save_dir", dir });
  dispatch({ type: "overlay", overlay: null });
  const n = saved.saved.length;
  if (saved.failed.length === 0) {
    dispatch({ type: "activity", kind: "applied", account, text: `Saved ${files(n)} to ${dir.trim()}` });
  } else {
    const why = saved.failed.map((f) => f.error.message).join("; ");
    const text = `Saved ${n}/${n + saved.failed.length} files to ${dir.trim()} (${saved.failed.length} failed: ${why})`;
    dispatch({ type: "activity", kind: "failed", account, text });
  }
  return null;
}

/** `path` with the home directory written `~`, the form the fields take and a draft keeps portable. */
export function tildePath(path: string, home: string): string {
  const h = home.replace(/\/+$/, "");
  if (!h) return path;
  if (path === h) return "~";
  return path.startsWith(`${h}/`) ? `~${path.slice(h.length)}` : path;
}

/** A field's path with `~` expanded, for where the picker opens; null when it is not absolute. */
function pickerStart(typed: string, home: string): string | null {
  const t = typed.trim();
  if (t === "~" || t.startsWith("~/")) return home ? `${home.replace(/\/+$/, "")}${t.slice(1)}` : null;
  return t.startsWith("/") ? t : null;
}

/**
 * The native picker (`tauri-plugin-dialog`'s `open`): a folder for the Save
 * dialog, a file for Attach file, opened where the field points. Resolves to
 * the path picked, under the home directory written with `~`, or null when
 * the user closed the picker; rejects with a sentence when the window has no
 * picker (a browser under `pnpm dev`), and the typed field remains.
 */
export async function pickPath(kind: "directory" | "file", typed: string): Promise<string | null> {
  let home = "";
  try {
    home = await homeDir();
  } catch {
    // No home to start from or to shorten with: the picker still opens.
  }
  let picked: unknown;
  try {
    picked = await openPicker({
      directory: kind === "directory",
      multiple: false,
      title: kind === "directory" ? "Save attachments to" : "Attach file",
      defaultPath: pickerStart(typed, home) ?? undefined,
    });
  } catch (e: unknown) {
    throw new Error(`The file picker did not open (${asGuiError(e).message}); type the path instead`);
  }
  return typeof picked === "string" && picked !== "" ? tildePath(picked, home) : null;
}

/** Attach `path` to the draft; resolves to why it was refused (the dialog stays open), or null. */
export async function attachPath(account: string, draftId: string, path: string, dispatch: Dispatch<Action>): Promise<string | null> {
  const typed = path.trim();
  if (!typed) return "Type the path of the file to attach";
  try {
    await cmd.draftAttach(account, draftId, typed);
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
  dispatch({ type: "overlay", overlay: null });
  dispatch({ type: "activity", kind: "applied", account, text: `Attached ${typed} to ${draftId}` });
  return null;
}

/** Remove entry `index` from the draft's list; the new list, or null when refused (a notice says why). */
export async function removeItem(
  account: string,
  draftId: string,
  item: AttachmentItem,
  dispatch: Dispatch<Action>,
): Promise<DraftAttachments | null> {
  try {
    const list = await cmd.draftAttachmentRemove(account, draftId, item.part);
    dispatch({ type: "activity", kind: "applied", account, text: `Removed ${item.name} from ${draftId}` });
    return list;
  } catch (e: unknown) {
    failed(dispatch, account, `Remove failed: ${asGuiError(e).message}`);
    return null;
  }
}
