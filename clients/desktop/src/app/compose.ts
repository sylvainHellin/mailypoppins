// Compose through the external editor (clients/desktop/docs/shell.md,
// "Compose"): new draft, reply, reply all, forward, edit a draft, edit its
// recipients. Each write answers the draft file's path, which opens in the
// user's editor; the editor's saves reach the list as the watcher's
// `draft.changed`, so nothing here reloads a list.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { actionTargets } from "@/app/reducer";
import { createMutations } from "@/app/mutations";
import {
  draftsShown,
  filteredDrafts,
  readerKey,
  sendingRefusal,
  type AppState,
  type ComposeDialog,
  type MessageTarget,
  type MutationDialog,
} from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, fixtureNotice, type DraftHeaders } from "@/lib/gui-types";
import type { DraftCreated, DraftKind, DraftMessage } from "@/protocol/types";

/** What a reply or a forward is built from: a stored row, or a server-only hit's own headers. */
export type ComposeSource =
  | { kind: "row"; account: string; row_id: number; subject: string }
  | { kind: "hit"; account: string; message: DraftMessage };

/** The subject a forward shows before its draft exists, by `mp_core::draft::fwd_subject`'s rule. */
export function fwdSubject(subject: string): string {
  return subject.toLowerCase().startsWith("fwd: ") ? subject : `Fwd: ${subject}`;
}

/** A stored message's subject, wherever the model shows it. */
function subjectOf(s: AppState, t: MessageTarget): string {
  const list = s.messages.data;
  const row =
    (list?.kind === "messages" && list.account === t.account ? list.rows.find((r) => r.id === t.row_id) : undefined) ??
    s.search?.hits.find((h) => h.account === t.account && h.row_id === t.row_id);
  if (row) return row.subject;
  const meta = s.reader.key === readerKey(t.account, t.row_id) ? s.reader.meta : null;
  return meta?.subject ?? "";
}

/** A stored message as a compose source. */
export function rowSource(s: AppState, t: MessageTarget): ComposeSource {
  return { kind: "row", account: t.account, row_id: t.row_id, subject: subjectOf(s, t) };
}

/**
 * What the cursor names for a reply or a forward: the selected message, or
 * a server-only search hit, which `draft_from_message` builds from its own
 * headers. A draft has nothing to quote.
 */
export function cursorSource(s: AppState): ComposeSource | null {
  const account = s.search?.account ?? s.selection.account;
  if (!account) return null;
  if (s.search && s.selection.hit) {
    const hit = s.search.hits.find((h) => h.key === s.selection.hit);
    if (hit?.source) return { kind: "hit", account: hit.account, message: hit.source };
    return null;
  }
  const m = s.selection.message;
  return m ? rowSource(s, { account, row_id: m.row_id }) : null;
}

function notice(dispatch: Dispatch<Action>, text: string): void {
  dispatch({ type: "notice", text });
}

function failed(dispatch: Dispatch<Action>, account: string, what: string, e: unknown): void {
  dispatch({ type: "activity", kind: "compose_failed", account, text: `${what}: ${asGuiError(e).message}` });
}

/**
 * Open a draft file in the editor. The session shows `opening` until
 * `editor_open` answers; a launch that fails (`setup`: the command did not
 * start or exited at once) is a failure notice whose text names
 * `MP_DESKTOP_EDITOR` or the editor setting. In fixture mode a notice says
 * that no editor was launched.
 */
export async function openInEditor(dispatch: Dispatch<Action>, account: string, draftId: string, path: string): Promise<void> {
  dispatch({ type: "compose_opening", account, draftId, path });
  try {
    const launch = await cmd.editorOpen(path);
    dispatch({ type: "compose_editing", account, draftId, editor: launch.editor });
    const fixture = fixtureNotice(launch);
    if (fixture) notice(dispatch, fixture);
  } catch (e: unknown) {
    dispatch({ type: "compose_failed", account, draftId, error: asGuiError(e) });
  }
}

async function created(dispatch: Dispatch<Action>, account: string, what: string, write: Promise<DraftCreated>): Promise<void> {
  let draft: DraftCreated;
  try {
    draft = await write;
  } catch (e: unknown) {
    failed(dispatch, account, what, e);
    return;
  }
  await openInEditor(dispatch, draft.account, draft.id, draft.path);
}

const FROM_HIT: Record<"reply" | "reply_all" | "forward", DraftKind> = { reply: "reply", reply_all: "reply_all", forward: "forward" };

/**
 * `r`, `cr`, `ca`: the TUI writes the reply and goes straight to the
 * editor, with no wizard. A stored row goes through `draft_reply`, a
 * server-only hit through `draft_from_message`.
 */
export function reply(src: ComposeSource | null, all: boolean, dispatch: Dispatch<Action>): void {
  if (!src) {
    notice(dispatch, "Reply needs a received message");
    return;
  }
  const what = all ? "Reply all failed" : "Reply failed";
  const write =
    src.kind === "row"
      ? cmd.draftReply(src.account, src.row_id, all)
      : cmd.draftFromMessage(src.account, FROM_HIT[all ? "reply_all" : "reply"], src.message);
  void created(dispatch, src.account, what, write);
}

/**
 * `cf`: the TUI asks for the recipients first, with the subject it will
 * write, and the draft keeps them over what the builder derives. A
 * server-only hit is forwarded at once, as the TUI's search overlay does:
 * `draft_from_message` takes no recipients.
 */
export function forward(src: ComposeSource | null, dispatch: Dispatch<Action>): void {
  if (!src) {
    notice(dispatch, "Forward needs a received message; a draft has none to quote");
    return;
  }
  if (src.kind === "hit") {
    void created(dispatch, src.account, "Forward failed", cmd.draftFromMessage(src.account, FROM_HIT.forward, src.message));
    return;
  }
  const dialog: ComposeDialog = { kind: "forward", account: src.account, row_id: src.row_id, subject: fwdSubject(src.subject) };
  dispatch({ type: "open_compose", dialog });
}

/** `cn`: the wizard, for the account shown. */
export function newDraft(s: AppState, dispatch: Dispatch<Action>): void {
  // The outbox view may show another account than the selected one.
  const account = s.outboxView?.account ?? s.search?.account ?? s.selection.account;
  if (!account) return;
  dispatch({ type: "open_compose", dialog: { kind: "new", account } });
}

/** The draft under the cursor of the shown Drafts list, with its row. */
function cursorDraft(s: AppState) {
  if (!draftsShown(s) || !s.selection.account || !s.selection.draft) return null;
  const row = filteredDrafts(s.messages.data, "").find((d) => d.id === s.selection.draft);
  return row ? { account: s.selection.account, row } : null;
}

/**
 * `e` on a draft: `draft_path` resolves the file fresh, then the editor
 * opens it. A file that does not parse opens by the path the listing gave,
 * since fixing it is what the editor is for.
 */
export async function editDraft(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const cur = cursorDraft(s);
  if (!cur) return;
  const { account, row } = cur;
  const busy = sendingRefusal(s, [{ account, draft: row.id }]);
  if (busy) return notice(dispatch, busy);
  let path = row.path;
  try {
    path = (await cmd.draftPath(account, row.id)).path;
  } catch (e: unknown) {
    if (row.valid) {
      failed(dispatch, account, `Could not find ${row.id}`, e);
      return;
    }
  }
  await openInEditor(dispatch, account, row.id, path);
}

/**
 * `ce`, Drafts only: the recipients dialog, filled from the draft. The
 * listing has no Bcc, so the fields come from `draft_preview`, which reads
 * the file; a draft that does not parse cannot be edited this way.
 */
export async function editRecipients(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  if (!draftsShown(s)) {
    notice(dispatch, "Edit recipients (c e) is only available in Drafts");
    return;
  }
  const cur = cursorDraft(s);
  if (!cur) return;
  const { account, row } = cur;
  const busy = sendingRefusal(s, [{ account, draft: row.id }]);
  if (busy) return notice(dispatch, busy);
  try {
    const p = await cmd.draftPreview(account, row.id);
    dispatch({
      type: "open_compose",
      dialog: { kind: "recipients", account, draftId: row.id, to: p.to ?? "", cc: p.cc ?? "", bcc: p.bcc ?? "", subject: p.subject },
    });
  } catch (e: unknown) {
    failed(dispatch, account, `Cannot edit ${row.id}`, e);
  }
}

/**
 * `cA` and `cD`, Drafts only, over the marked drafts or the cursor one.
 * Over marks they ask first, with the TUI's words; one draft changes at once.
 */
export function setStatus(s: AppState, dispatch: Dispatch<Action>, approve: boolean): void {
  const what = approve ? "Approve (c A)" : "Unapprove (c D)";
  if (!draftsShown(s)) {
    notice(dispatch, `${what} is only available in Drafts`);
    return;
  }
  const drafts = actionTargets(s).filter((t): t is { account: string; draft: string } => "draft" in t);
  if (drafts.length === 0) return;
  const busy = sendingRefusal(s, drafts);
  if (busy) return notice(dispatch, busy);
  if (s.marked.keys.size > 0) {
    const n = drafts.length;
    const dialog: MutationDialog = {
      kind: approve ? "approve" : "demote",
      targets: drafts,
      title: approve ? `Approve ${n} drafts?` : `Mark ${n} drafts as draft?`,
      detail: `${n} selected drafts`,
    };
    dispatch({ type: "open_dialog", dialog });
    return;
  }
  void createMutations(dispatch).setDraftStatus(drafts[0].account, [drafts[0].draft], approve);
}

/**
 * The draft preview's Approve and Back to draft: the draft the preview shows,
 * whatever is marked, so no dialog; a draft being sent is refused as by `cA`.
 */
export function setShownStatus(s: AppState, dispatch: Dispatch<Action>, account: string, draftId: string, approve: boolean): void {
  const busy = sendingRefusal(s, [{ account, draft: draftId }]);
  if (busy) return notice(dispatch, busy);
  void createMutations(dispatch).setDraftStatus(account, [draftId], approve);
}

// ---------------------------------------------------------------------------
// The wizard's and the recipients dialog's submit
// ---------------------------------------------------------------------------

/** The wizard's fields; `signature` is a name, or null for none. */
export type ComposeFields = DraftHeaders & { signature?: string | null };

/** A recipient field as the TUI normalises it: no trailing separators. */
export function normalizeRecipients(field: string): string {
  return field.trim().replace(/[\s,;]+$/, "");
}

function normalized(f: ComposeFields): DraftHeaders {
  return { to: normalizeRecipients(f.to), cc: normalizeRecipients(f.cc), bcc: normalizeRecipients(f.bcc), subject: f.subject.trim() };
}

function slug(subject: string): string {
  return subject
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40)
    .replace(/-+$/, "");
}

/** The new draft's file name, the TUI wizard's: `draft-<local time>-<subject slug>`. */
export function draftName(subject: string, now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, "0");
  const stamp = `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}-${p(now.getHours())}${p(now.getMinutes())}${p(now.getSeconds())}`;
  const s = slug(subject);
  return s ? `draft-${stamp}-${s}` : `draft-${stamp}`;
}

/** The TUI's rule: at least one recipient across To, Cc and Bcc. */
export const NO_RECIPIENT = "Add a recipient in To, Cc or Bcc";

/**
 * Submit a compose dialog. Resolves to null once the draft is written (and,
 * for a new draft or a forward, the editor asked to open it), or to the
 * reason it was not, which the dialog shows while it stays open.
 */
export async function submitCompose(dialog: ComposeDialog, fields: ComposeFields, dispatch: Dispatch<Action>): Promise<string | null> {
  const headers = normalized(fields);
  if (!headers.to && !headers.cc && !headers.bcc) return NO_RECIPIENT;
  const account = dialog.account;
  let draft: DraftCreated;
  try {
    if (dialog.kind === "recipients") {
      const changed = headers.subject !== dialog.subject;
      const loc = await cmd.draftSetRecipients(account, dialog.draftId, {
        to: headers.to,
        cc: headers.cc,
        bcc: headers.bcc,
        ...(changed ? { subject: headers.subject } : {}),
      });
      dispatch({ type: "overlay", overlay: null });
      dispatch({ type: "activity", kind: "applied", account, text: `Recipients updated: ${loc.selector}` });
      return null;
    }
    if (dialog.kind === "forward") {
      draft = await cmd.draftForward(account, dialog.row_id, headers);
    } else {
      const sig = fields.signature;
      draft = await cmd.draftCreate(account, draftName(headers.subject), {
        ...(sig === null ? { no_signature: true } : sig ? { signature: sig } : {}),
        headers,
      });
    }
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
  dispatch({ type: "overlay", overlay: null });
  await openInEditor(dispatch, draft.account, draft.id, draft.path);
  return null;
}
