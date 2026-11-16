// The notes' commands (PERSO-315): what `n`, the note prompt, the notes list
// and a hand-off run, each one or a few `note_*` calls; the model they move
// is app/notes.ts's.

import type { Dispatch } from "react";
import { actionItems, type Action } from "@/app/reducer";
import { draftSelector, localDate, agentRunning, noteTitle, promptFor } from "@/app/notes";
import { draftItems, expandedMember, listedRow, readerKey, type AppState, type NotePrompt } from "@/app/state";
import * as cmd from "@/lib/commands";
import { copyLater } from "@/lib/clipboard";
import { asGuiError } from "@/lib/gui-types";
import type { Note, NoteState } from "@/protocol/types";

/** What `n` acts on, or why there is nothing. */
export type NoteTargets =
  | { selectors: string[]; account: string; title: string; state: NoteState | undefined }
  | { refusal: string };

/**
 * The mail `n` puts a note on: the marked rows, else the cursor row, else the
 * message the reader shows; a conversation header's is its latest message.
 * A server-only hit has nothing a note can be on.
 */
export function noteTargets(s: AppState): NoteTargets {
  const account = s.search?.account ?? s.selection.account;
  if (!account) return { refusal: "Select a message first" };
  const list = s.messages.data;
  const found: { selector: string; subject: string; note: NoteState | undefined }[] = [];
  const items = actionItems(s);
  for (const it of items) {
    if (it.kind === "hit") continue;
    if (it.kind === "draft" || (it.kind === "member" && it.draft !== null)) {
      const id = it.kind === "draft" ? it.id : it.draft!;
      const d = draftItems(list).find((x) => x.id === id);
      const member = it.kind === "member" ? s.expanded[it.thread_id]?.data : undefined;
      const m = member?.messages.find((x) => x.draft_id === id);
      found.push({
        selector: m?.selector || d?.selector || draftSelector(s, account, id),
        subject: d?.subject ?? member?.subject ?? "",
        note: d?.note ?? m?.note,
      });
      continue;
    }
    const ref = it.ref!;
    const row = listedRow(list, account, ref.row_id);
    const member = expandedMember(s, account, ref.row_id);
    const hit = s.search?.hits.find((h) => h.account === account && h.row_id === ref.row_id);
    const subject = row?.subject ?? hit?.subject ?? (member ? s.expanded[it.kind === "member" ? it.thread_id : ""]?.data?.subject : undefined) ?? "";
    found.push({ selector: ref.selector, subject, note: it.kind === "member" ? member?.note : row?.note });
  }
  if (found.length === 0) {
    const m = s.selection.message;
    if (m && m.selector && s.reader.key === readerKey(account, m.row_id)) {
      found.push({ selector: m.selector, subject: s.reader.meta?.subject ?? "", note: undefined });
    }
  }
  if (found.length === 0) {
    if (items.some((it) => it.kind === "hit")) return { refusal: "Not in the local store; press F to fetch it first" };
    return { refusal: "Select a message first" };
  }
  const one = found.length === 1;
  return {
    selectors: found.map((f) => f.selector),
    account,
    title: one ? found[0].subject || "(no subject)" : `${found.length} emails`,
    // A row the list shows with no note may still have one the list has
    // not caught up with; `note_get` asks either way for one target.
    state: one ? found[0].note : undefined,
  };
}

/** `n`: the note prompt on the marked rows, the cursor row or the shown message. */
export async function openNotePrompt(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  const targets = noteTargets(s);
  if ("refusal" in targets) return dispatch({ type: "notice", text: targets.refusal });
  try {
    const one = targets.selectors.length === 1 ? targets.selectors[0] : null;
    const [{ presets }, existing] = await Promise.all([
      cmd.notePresets(),
      one ? cmd.noteGet(one).then((l) => l.note) : Promise.resolve(null),
    ]);
    dispatch({ type: "open_note_prompt", prompt: promptFor(targets, presets, existing) });
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The note prompt did not open: ${asGuiError(e).message}`, level: "error" });
  }
}

/** The notes list's `a`: the prompt for a note asking for a new email from `account`. */
export async function openNewMailNote(account: string | null, dispatch: Dispatch<Action>): Promise<void> {
  if (!account) return dispatch({ type: "notice", text: "Select an account first: a new email goes out from one" });
  try {
    const { presets } = await cmd.notePresets();
    dispatch({ type: "open_note_prompt", prompt: promptFor({ selectors: [], account, title: `a new email from ${account}` }, presets, null, "notes") });
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The note prompt did not open: ${asGuiError(e).message}`, level: "error" });
  }
}

/** The notes list's `e`: the prompt on `note`, by its selector, or by its id when it has none or its mail is gone. */
export async function editNote(note: Note, dispatch: Dispatch<Action>): Promise<void> {
  try {
    const { presets } = await cmd.notePresets();
    const selectors = note.selector && !note.orphaned ? [note.selector] : [];
    const title = note.selector ? note.subject || "(no subject)" : `a new email from ${note.account}`;
    dispatch({ type: "open_note_prompt", prompt: promptFor({ selectors, account: note.account, title }, presets, note, "notes") });
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The note prompt did not open: ${asGuiError(e).message}`, level: "error" });
  }
}

/** What a write came to: `ok`, or the sentence the prompt or the list shows. */
export type NoteOutcome = { ok: true } | { ok: false; error: string };

/**
 * Save the prompt's note: `note_set_many` on several selectors, `note_set`
 * on one (which replaces its preset and text and opens it again), by id on
 * an edited note with no selector, or with neither for a new email.
 */
export async function saveNote(p: NotePrompt, preset: string | null, text: string, dispatch: Dispatch<Action>): Promise<NoteOutcome> {
  const content = { ...(preset ? { preset } : {}), text };
  try {
    if (p.selectors.length > 1) {
      await cmd.noteSetMany({ account: p.account, selectors: p.selectors, ...content });
    } else if (p.selectors.length === 1) {
      await cmd.noteSet({ account: p.account, selector: p.selectors[0], ...content });
    } else if (p.editing) {
      await cmd.noteSet({ id: p.editing.id, ...content });
    } else {
      await cmd.noteSet({ account: p.account, ...content });
    }
  } catch (e: unknown) {
    return { ok: false, error: asGuiError(e).message };
  }
  dispatch({ type: "overlay", overlay: p.returnTo });
  dispatch({ type: "notice", text: `Note saved on ${p.title}` });
  return { ok: true };
}

/** `d` in the list, and Clear on a done note's prompt: remove one note. */
export async function dropNote(note: Note, dispatch: Dispatch<Action>): Promise<NoteOutcome> {
  try {
    await cmd.noteClear(note.id);
  } catch (e: unknown) {
    return { ok: false, error: `Cannot drop the note: ${asGuiError(e).message}` };
  }
  dispatch({ type: "notice", text: `Dropped the note on ${noteTitle(note)}` });
  return { ok: true };
}

/** `D` in the list: remove every done note. */
export async function clearDoneNotes(dispatch: Dispatch<Action>): Promise<NoteOutcome> {
  try {
    const { removed } = await cmd.noteClearDone();
    dispatch({ type: "notice", text: removed === 0 ? "No done note to clear" : `Cleared ${removed} done ${removed === 1 ? "note" : "notes"}` });
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: `Cannot clear the done notes: ${asGuiError(e).message}` };
  }
}

/** `y` in the list: copy the batch prompt of every open note; changes nothing. Call it from the key press. */
export function copyNotesExport(dispatch: Dispatch<Action>): void {
  const text = cmd.noteExport().then((e) => {
    if (!e.text) throw new Error("no open note");
    return e.text;
  });
  text.catch((e: unknown) => {
    const why = e instanceof Error && e.message === "no open note" ? "No open note to copy" : `The export failed: ${asGuiError(e).message}`;
    dispatch({ type: "notice", text: why });
  });
  void copyLater(text, "the notes' prompt", dispatch);
}

/**
 * `x` in the list: hand every open note to the agent `[agent]` names. The
 * batch prompt is `note_export`'s; `clipboard` copies it and marks nothing,
 * `paseo` starts a detached `paseo run` and marks the notes once it started,
 * and anything else opens the agent in the panel, whose start marks them
 * (`agentStarted`). An agent still running in the panel is not replaced.
 * Called from the key press, so a copy keeps its activation.
 */
export function handOffNotes(s: AppState, dispatch: Dispatch<Action>): void {
  if (agentRunning(s.agent)) {
    dispatch({ type: "notice", text: "An agent is already running in the panel; stop it there before handing off again" });
    return;
  }
  const date = localDate();
  const prepared = Promise.all([cmd.agentSettings(), cmd.noteExport()]);
  const failed = (e: unknown) => dispatch({ type: "notice", text: `The hand-off failed: ${asGuiError(e).message}`, level: "error" });
  // The clipboard route copies within the press, the text arriving later.
  const copied = prepared.then(([settings, exported]) => {
    if (settings.route !== "clipboard" || !exported.text) throw new Error("not copied");
    return exported.text;
  });
  copied.catch(() => {});
  void prepared.then(async ([settings, exported]) => {
    if (!exported.text) return dispatch({ type: "notice", text: "No open note to hand off" });
    if (settings.route === "clipboard") {
      await copyLater(copied, "the notes' prompt; open_in = clipboard starts no agent", dispatch);
      return;
    }
    if (settings.route === "paseo") {
      try {
        await cmd.agentPaseo(exported.text, date);
        await cmd.noteHandoff(exported.ids);
        const n = exported.ids.length;
        dispatch({ type: "notice", text: `Handed ${n} ${n === 1 ? "note" : "notes"} to ${settings.name} through paseo` });
      } catch (e: unknown) {
        failed(e);
      }
      return;
    }
    dispatch({ type: "agent_open", text: exported.text, ids: exported.ids, date });
  }, failed);
}

/** The panel's agent started: its notes are marked handed off. */
export async function agentStarted(ids: string[], dispatch: Dispatch<Action>): Promise<void> {
  try {
    await cmd.noteHandoff(ids);
    const n = ids.length;
    dispatch({ type: "notice", text: `Handed ${n} ${n === 1 ? "note" : "notes"} to the agent` });
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The agent started, but its notes were not marked handed off: ${asGuiError(e).message}`, level: "warning" });
  }
}

/**
 * Enter in the notes list: the note's mail in Mail, a message in the mailbox
 * its selector names, a draft in Drafts; a note for a new email, or one whose
 * mail is gone, has none to open.
 */
export function openNoteMail(note: Note, dispatch: Dispatch<Action>): void {
  if (!note.selector || note.orphaned) {
    const why = note.selector ? `The mail of this note is gone (${note.selector})` : "This note asks for a new email; it has no mail to open";
    return dispatch({ type: "notice", text: why });
  }
  if (note.draft_id) return dispatch({ type: "open_note_draft", account: note.account, id: note.draft_id });
  const mailbox = note.selector.replace(/^mp:\/\//, "").split("/")[1];
  if (!mailbox) return dispatch({ type: "notice", text: `The selector ${note.selector} names no mailbox` });
  const messageId = note.message_id ? `<${note.message_id}>` : "";
  dispatch({ type: "open_note_mail", account: note.account, mailbox, selector: note.selector, message_id: messageId });
}
