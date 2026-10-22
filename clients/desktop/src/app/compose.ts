// Compose (clients/desktop/docs/shell.md, "Compose"): new draft, reply,
// reply all, forward, edit a draft, edit its recipients. Each write answers
// the draft file's path, which opens in the embedded terminal editor when
// the route says so (ticket 0130) and in the user's external editor
// otherwise; the editor's saves reach the list as the watcher's
// `draft.changed`, so nothing here reloads a list.

import { useEffect, useRef, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { actionTargets } from "@/app/reducer";
import { createMutations } from "@/app/mutations";
import {
  conversationDraft,
  conversationMember,
  draftsShown,
  filteredDrafts,
  isRunning,
  readerKey,
  sendingRefusal,
  targetKey,
  viewId,
  type AppState,
  type ComposeDialog,
  type ComposeSession,
  type MessageTarget,
  type MutationDialog,
} from "@/app/state";
import * as cmd from "@/lib/commands";
import { getCurrentWindow } from "@/lib/tauri";
import { tauriBridge } from "@/lib/terminal";
import { asGuiError, fixtureNotice, type DraftHeaders, type MessageSource } from "@/lib/gui-types";
import type { DraftCreated, DraftKind, DraftMessage } from "@/protocol/types";

/** What a reply or a forward is built from: a stored row, or a server-only hit's own headers. */
export type ComposeSource =
  | { kind: "row"; account: string; row_id: number; subject: string }
  | { kind: "hit"; account: string; message: DraftMessage; mailbox: string; message_id: string | null };

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
    if (hit?.source) return { kind: "hit", account: hit.account, message: hit.source, mailbox: hit.mailbox, message_id: hit.message_id };
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
 * Open a draft file in the editor, by the route `editor_setting_get`
 * reports. The embedded route starts a session the reader area's terminal
 * pane spawns (`components/compose/TerminalHost.tsx`), or shows the draft's
 * running one. A draft whose embedded editor runs (`sessions` is
 * `state.compose` when asked) shows that editor whatever the route says:
 * the setting may have changed meanwhile, and replacing the session would
 * kill the editor with its unsaved buffer. The external route is M3's: the session shows `opening`
 * until `editor_open` answers; a launch that fails (`setup`: the command did
 * not start or exited at once) is a failure notice whose text names
 * `MP_DESKTOP_EDITOR` or the editor setting. In fixture mode a notice says
 * that no editor was launched. A route that cannot be read is external.
 */
export async function openInEditor(
  dispatch: Dispatch<Action>,
  account: string,
  draftId: string,
  path: string,
  sessions: AppState["compose"] = {},
): Promise<void> {
  if (isRunning(sessions[targetKey({ account, draft: draftId })])) {
    dispatch({ type: "compose_embedded", account, draftId, path });
    return;
  }
  if (await embeddedRoute()) {
    dispatch({ type: "compose_embedded", account, draftId, path });
    return;
  }
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

/** Whether a file opens on the embedded route now; a route that cannot be read is external. */
async function embeddedRoute(): Promise<boolean> {
  try {
    return (await cmd.editorSettingGet()).route === "embedded";
  } catch {
    // The external route reports its own failure.
    return false;
  }
}

/**
 * `e` on a received message (PERSO-92, the TUI's read-only view): the
 * daemon writes its Markdown rendition, mode 0444, and it opens by a
 * draft's route. On the embedded route it is a session of `state.compose`
 * under `viewId(source)`, in the reader area's terminal pane as a draft's
 * editor is, and its exit brings the message back; a view of the same
 * message still running is shown again rather than spawned twice. On the
 * external route `message_markdown_open` hands it to the external editor
 * and keeps no session. A row the store lost is a warning notice. A
 * server-only hit's rendition is the daemon's, of the server's copy
 * (PERSO-155), with no fetch first.
 */
export async function viewInEditor(
  dispatch: Dispatch<Action>,
  account: string,
  source: MessageSource,
  sessions: AppState["compose"] = {},
): Promise<void> {
  const running = sessions[targetKey({ account, draft: viewId(source) })];
  if (running && isRunning(running)) {
    dispatch({ type: "compose_embedded", account, draftId: running.draftId, path: running.path, view: { source } });
    return;
  }
  if (await embeddedRoute()) return viewEmbedded(dispatch, account, source);
  try {
    const launch = await cmd.messageMarkdownOpen(account, source);
    notice(dispatch, fixtureNotice(launch) ?? `Opened the message read-only in ${launch.editor}`);
  } catch (e: unknown) {
    viewFailed(dispatch, account, e);
  }
}

/** Write the rendition afresh and open it in a new embedded session. */
async function viewEmbedded(dispatch: Dispatch<Action>, account: string, source: MessageSource): Promise<void> {
  try {
    const file = await cmd.messageMarkdownPath(account, source);
    dispatch({ type: "compose_embedded", account, draftId: viewId(source), path: file.path, view: { source } });
  } catch (e: unknown) {
    viewFailed(dispatch, account, e);
  }
}

/** A row the store lost is a warning, the TUI's line; anything else a failure notice. */
function viewFailed(dispatch: Dispatch<Action>, account: string, e: unknown): void {
  const error = asGuiError(e);
  if (error.kind === "not_found") return dispatch({ type: "notice", text: error.message, level: "warning" });
  dispatch({ type: "activity", kind: "failed", account, text: `Open failed: ${error.message}` });
}

/**
 * The banner's Reopen: an embedded session spawns again on the same path,
 * whatever the route says now; an external one runs `editor_open` again. A
 * message's rendition is written afresh first, since its handle expires.
 */
export function reopen(c: ComposeSession, dispatch: Dispatch<Action>): void {
  if (c.view) void viewEmbedded(dispatch, c.account, c.view.source);
  else if (c.kind === "embedded") dispatch({ type: "compose_embedded", account: c.account, draftId: c.draftId, path: c.path });
  else void openInEditor(dispatch, c.account, c.draftId, c.path);
}

/**
 * Close embedded editors: each session is forgotten, then its child is
 * killed (`terminal_kill` waits until the exit frame has gone), so the
 * draft stays as last saved. Resolves once every kill answered.
 */
export async function closeEditors(sessions: ComposeSession[], dispatch: Dispatch<Action>): Promise<void> {
  const kills: Promise<void>[] = [];
  for (const c of sessions) {
    if (c.kind !== "embedded") continue;
    dispatch({ type: "compose_done", account: c.account, draftId: c.draftId });
    if (c.session !== null) kills.push(tauriBridge.kill(c.session).catch(() => {}));
  }
  await Promise.all(kills);
}

/** The embedded sessions whose child still runs. */
export function runningEditors(s: AppState): ComposeSession[] {
  return Object.values(s.compose).filter((c) => c.kind === "embedded" && c.status.kind === "running");
}

/**
 * The navigation question's "Close the editor": the shown editor's child is
 * killed, which leaves the draft as last saved, then the held navigation
 * runs. Its exit frame may land first and mark it crashed; the session is
 * forgotten either way.
 */
export async function leaveClosingEditor(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const c = s.composeShown ? s.compose[s.composeShown] : undefined;
  if (c?.kind === "embedded" && c.session !== null) await tauriBridge.kill(c.session).catch(() => {});
  dispatch({ type: "compose_leave_close" });
}

/**
 * The window close question's "Close the editor": every embedded editor is
 * killed, then the window is destroyed, which the close request left to
 * the webview.
 */
export async function closeWindow(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  dispatch({ type: "overlay", overlay: null });
  await closeEditors(Object.values(s.compose), dispatch);
  try {
    await getCurrentWindow().destroy();
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The window did not close: ${asGuiError(e).message}` });
  }
}

/**
 * Ask before the window closes over a running embedded editor. Tauri
 * prevents a close itself while the webview listens for
 * `tauri://close-requested`, and `onCloseRequested`'s wrapper destroys the
 * window after the handler unless it called `preventDefault`; so with no
 * editor running the handler does nothing and the window closes as before.
 * Outside Tauri (a plain browser) there is no window to guard.
 */
export function useCloseGuard(s: AppState, dispatch: Dispatch<Action>): void {
  const latest = useRef(s);
  latest.current = s;
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let gone = false;
    let win: ReturnType<typeof getCurrentWindow>;
    try {
      win = getCurrentWindow();
    } catch {
      return;
    }
    win
      .onCloseRequested((event) => {
        if (runningEditors(latest.current).length === 0) return;
        event.preventDefault();
        dispatch({ type: "compose_close_requested" });
      })
      .then(
        (u) => {
          if (gone) u();
          else unlisten = u;
        },
        () => {},
      );
    return () => {
      gone = true;
      unlisten?.();
    };
  }, [dispatch]);
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

const FROM_HIT: Record<"reply" | "reply_all", DraftKind> = { reply: "reply", reply_all: "reply_all" };

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
 * server-only hit asks for them too, as the TUI's search overlay does
 * (PERSO-155): `draft_forward` takes the hit as its source, and the daemon
 * builds the forward from the server's copy, its parts carried, ingesting
 * nothing.
 *
 * `cF` (`asAttachment`) is the same wizard over `draft_forward`'s
 * `as_attachment`, the message carried as its `.eml` (PERSO-174).
 */
export function forward(src: ComposeSource | null, dispatch: Dispatch<Action>, asAttachment = false): void {
  if (!src) {
    notice(dispatch, "Forward needs a received message; a draft has none to quote");
    return;
  }
  let source: MessageSource;
  let subject: string;
  if (src.kind === "hit") {
    if (src.message_id === null) return notice(dispatch, "This hit carries no Message-ID; cannot forward it");
    source = { kind: "hit", mailbox: src.mailbox, message_id: src.message_id };
    subject = src.message.subject;
  } else {
    source = { kind: "row", row_id: src.row_id };
    subject = src.subject;
  }
  const dialog: ComposeDialog = {
    kind: "forward",
    account: src.account,
    source,
    subject: fwdSubject(subject),
    as_attachment: asAttachment,
  };
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
 * A conversation's draft member under the cursor (ticket 0142), named by its
 * id: what `ce`, `cA`, `cD` and `x` reach outside Drafts, as the TUI's do
 * (PERSO-146). Null in Drafts, where the listed row is the cursor.
 */
export function memberDraft(s: AppState): { account: string; id: string } | null {
  const account = s.selection.account;
  const id = draftsShown(s) ? null : conversationDraft(s);
  return account && id ? { account, id } : null;
}

/**
 * The draft `e` edits: the Drafts row under the cursor, or a conversation's
 * draft member (ticket 0142), named by the conversation cursor. A member is
 * a linked draft, one that parsed when the index read it, so it counts as
 * valid and its file is whatever `draft_path` answers: the expansion's
 * `path` is absent from a daemon older than the field.
 */
export function editableDraft(s: AppState): { account: string; id: string; path: string; valid: boolean } | null {
  const cur = cursorDraft(s);
  if (cur) return { account: cur.account, id: cur.row.id, path: cur.row.path, valid: cur.row.valid };
  const account = s.selection.account;
  const id = conversationDraft(s);
  if (!account || !id) return null;
  return { account, id, path: conversationMember(s)?.path ?? "", valid: true };
}

/**
 * `e` on a draft: `draft_path` resolves the file fresh, then the editor
 * opens it. A file that does not parse opens by the path the listing gave,
 * since fixing it is what the editor is for.
 */
export async function editDraft(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const cur = editableDraft(s);
  if (!cur) return;
  const { account, id } = cur;
  const busy = sendingRefusal(s, [{ account, draft: id }]);
  if (busy) return notice(dispatch, busy);
  let path = cur.path;
  try {
    path = (await cmd.draftPath(account, id)).path;
  } catch (e: unknown) {
    if (cur.valid) {
      failed(dispatch, account, `Could not find ${id}`, e);
      return;
    }
  }
  await openInEditor(dispatch, account, id, path, s.compose);
}

/**
 * `ce`: the recipients dialog, filled from the draft's listed row, which
 * carries To, Cc, Bcc and the subject; a draft that does not parse cannot be
 * edited this way. A conversation's draft member has no listed row, so its
 * fields come from a `draft_preview` of it (PERSO-146).
 */
export function editRecipients(s: AppState, dispatch: Dispatch<Action>): void {
  const member = memberDraft(s);
  if (member) return void editMemberRecipients(s, dispatch, member);
  if (!draftsShown(s)) {
    notice(dispatch, "Edit recipients (c e) is only available in Drafts");
    return;
  }
  const cur = cursorDraft(s);
  if (!cur) return;
  const { account, row } = cur;
  const busy = sendingRefusal(s, [{ account, draft: row.id }]);
  if (busy) return notice(dispatch, busy);
  if (!row.valid) {
    dispatch({ type: "activity", kind: "compose_failed", account, text: `Cannot edit ${row.id}: ${row.diagnostic ?? "it does not parse"}` });
    return;
  }
  dispatch({
    type: "open_compose",
    dialog: { kind: "recipients", account, draftId: row.id, to: row.to ?? "", cc: row.cc ?? "", bcc: row.bcc ?? "", subject: row.subject ?? "" },
  });
}

/** `ce` on a conversation's draft member: the dialog, once `draft_preview` read its fields. */
async function editMemberRecipients(s: AppState, dispatch: Dispatch<Action>, member: { account: string; id: string }): Promise<void> {
  const { account, id } = member;
  const busy = sendingRefusal(s, [{ account, draft: id }]);
  if (busy) return notice(dispatch, busy);
  try {
    const p = await cmd.draftPreview(account, id);
    dispatch({
      type: "open_compose",
      dialog: { kind: "recipients", account, draftId: id, to: p.to ?? "", cc: p.cc ?? "", bcc: p.bcc ?? "", subject: p.subject ?? "" },
    });
  } catch (e: unknown) {
    dispatch({ type: "activity", kind: "compose_failed", account, text: `Cannot edit ${id}: ${asGuiError(e).message}` });
  }
}

/**
 * `cA` and `cD`, over the marked drafts or the cursor one: in Drafts, or on
 * a conversation's draft member (PERSO-146). Over marks they ask first, with
 * the TUI's words; one draft changes at once.
 */
export function setStatus(s: AppState, dispatch: Dispatch<Action>, approve: boolean): void {
  const what = approve ? "Approve (c A)" : "Unapprove (c D)";
  if (!draftsShown(s) && !memberDraft(s)) {
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

/** The wizard's fields; `signature` is a name, or null for none, and `body` the new draft's inline body. */
export type ComposeFields = DraftHeaders & { signature?: string | null; body?: string };

/** A recipient field as the TUI normalises it: no trailing separators. */
export function normalizeRecipients(field: string): string {
  return field.trim().replace(/[\s,;]+$/, "");
}

/** What a recipient field completes: the text after its last comma, trimmed. */
export function recipientQuery(field: string): string {
  return field.slice(field.lastIndexOf(",") + 1).trim();
}

/**
 * The TUI's `accept_suggestion`: the text after the last comma becomes
 * `Name <addr>`, the name quoted when it holds a comma, or the bare address
 * with no name, then `, ` for the next recipient.
 */
export function acceptRecipient(field: string, contact: { display_name: string; address: string }): string {
  let head = field.slice(0, field.lastIndexOf(",") + 1);
  if (head && !head.endsWith(" ")) head += " ";
  const name = contact.display_name;
  const one = name ? `${name.includes(",") ? `"${name}"` : name} <${contact.address}>` : contact.address;
  return `${head}${one}, `;
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
 * for a forward or a new draft with no inline body, the editor asked to open
 * it), or to the reason it was not, which the dialog shows while it stays
 * open. A new draft with a body is complete: the TUI's "Created: <file>" and
 * no editor.
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
    const sig = fields.signature;
    const signature = sig === null ? { no_signature: true } : sig ? { signature: sig } : {};
    if (dialog.kind === "forward") {
      draft = await cmd.draftForward(account, dialog.source, headers, { ...signature, as_attachment: dialog.as_attachment });
    } else {
      const body = fields.body?.trim() ? fields.body : undefined;
      draft = await cmd.draftCreate(account, draftName(headers.subject), { ...signature, headers, ...(body ? { body } : {}) });
      if (body) {
        dispatch({ type: "overlay", overlay: null });
        notice(dispatch, `Created: ${draft.path.slice(draft.path.lastIndexOf("/") + 1)}`);
        return null;
      }
    }
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
  dispatch({ type: "overlay", overlay: null });
  await openInEditor(dispatch, draft.account, draft.id, draft.path);
  return null;
}
