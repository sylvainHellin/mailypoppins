// The notes on mail (PERSO-315): the TUI's `n` prompt, its `N` list, the
// glyph beside the star, the noted-only filter, the reader's band and the
// hand-off to the coding agent. The daemon keeps the notes and renders the
// batch prompt; every write publishes `note.changed` with the whole list,
// which replaces the model's list, and the shown listing is read again so
// its rows carry their note state.
//
// The agent opens in the terminal panel beside the reader, whatever `open_in`
// says, but `clipboard` (copy only) and `paseo` (a detached `paseo run`). A
// note is marked handed off once its agent started, never before.

import {
  draftItems,
  markStale,
  readerKey,
  type AgentPanel,
  type AppState,
  type NotePrompt,
} from "@/app/state";
import type { GuiError } from "@/lib/gui-types";
import type { Note, NotePreset, NoteState } from "@/protocol/types";

/** The words that name a note state, the glyph's label beside the flag star. */
export const NOTE_LABEL: Record<NoteState, string> = {
  open: "Note open",
  handed_off: "Note handed off",
  done: "Note done",
};

/** The draft selector of `id`, as a Drafts listing names it. */
export function draftSelector(s: AppState, account: string, id: string): string {
  return draftItems(s.messages.data).find((d) => d.id === id)?.selector ?? `mp://${account}/drafts/${id}`;
}

/**
 * The prompt on `targets`: a note on the one target opens at its text to be
 * edited, a done one at the choice between clearing it and a new instruction.
 */
export function promptFor(
  targets: { selectors: string[]; account: string; title: string },
  presets: NotePreset[],
  existing: Note | null,
  returnTo: "notes" | null = null,
): NotePrompt {
  const live = existing !== null && existing.state !== "done";
  return {
    selectors: targets.selectors,
    account: targets.account,
    title: targets.title,
    presets,
    editing: existing,
    step: existing === null ? "keys" : live ? "text" : "done",
    preset: live ? (existing.preset ?? null) : null,
    text: live ? existing.text : "",
    returnTo,
  };
}

/** Today in this window's zone, `YYYY-MM-DD`, the `{date}` of the agent's command. */
export function localDate(now: Date = new Date()): string {
  const two = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}`;
}

/** Whether the panel's agent still runs, or is starting. */
export function agentRunning(agent: AgentPanel | null): boolean {
  return agent !== null && (agent.status.kind === "starting" || agent.status.kind === "running");
}

/** A note's mail, in a list or a notice: its subject, or the new email it asks for. */
export function noteTitle(note: Note): string {
  return note.selector || note.message_id || note.draft_id ? note.subject || "(no subject)" : `a new email from ${note.account}`;
}

/** The instruction a note gives, as a line: the preset's label, then the text. */
export function noteInstruction(note: Note, presets: NotePreset[] = []): string {
  const label = note.preset ? (presets.find((p) => p.key === note.preset)?.label ?? `Preset ${note.preset}`) : null;
  return [label, note.text.trim()].filter(Boolean).join(": ");
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/** The notes list opens on a list read again. */
export function openNotesList(s: AppState): AppState {
  return { ...s, overlay: "notes", notes: markStale(s.notes), notePrompt: null };
}

/** A `note_list` answer, kept only when nothing replaced the list since it was asked. */
export function notesLoaded(s: AppState, gen: number, notes: Note[]): AppState {
  if (gen !== s.notes.gen) return s;
  return { ...s, notes: { ...s.notes, data: notes, loadedGen: gen, error: null } };
}

export function notesFailed(s: AppState, gen: number, error: GuiError): AppState {
  if (gen !== s.notes.gen) return s;
  return { ...s, notes: { ...s.notes, loadedGen: gen, error } };
}

/**
 * The whole list, from `note.changed`: the truth, so a read still under way
 * is dropped when it lands; every band is read again where it shows.
 */
export function notesReplaced(s: AppState, notes: Note[]): AppState {
  const gen = s.notes.gen + 1;
  return { ...s, notes: { data: notes, gen, loadedGen: gen, error: null }, noteBands: { ...s.noteBands, gen: s.noteBands.gen + 1 } };
}

/** The selector of the mail the reader shows, whose note the band reads; null for none. */
export function bandSelector(s: AppState): string | null {
  if (s.view !== "mail") return null;
  const { account, message, draft } = s.selection;
  if (draft && account) return draftSelector(s, account, draft);
  if (message?.selector && account && s.reader.key === readerKey(account, message.row_id)) return message.selector;
  return null;
}

/** The band to read: the reader's selector, when its note was not read at the current generation. */
export function bandWanted(s: AppState): { selector: string; gen: number } | null {
  const selector = bandSelector(s);
  if (!selector) return null;
  const read = s.noteBands.of[selector];
  return read && read.gen === s.noteBands.gen ? null : { selector, gen: s.noteBands.gen };
}

/** A `note_get` answer for the band, dropped when a `note.changed` came after it was asked. */
export function bandLoaded(s: AppState, selector: string, gen: number, note: Note | null): AppState {
  if (gen !== s.noteBands.gen) return s;
  return { ...s, noteBands: { ...s.noteBands, of: { ...s.noteBands.of, [selector]: { note, gen } } } };
}

/** The note the band shows for the reader's mail, as last read; null for none. */
export function bandNote(s: AppState): Note | null {
  const selector = bandSelector(s);
  return selector ? (s.noteBands.of[selector]?.note ?? null) : null;
}

/** The panel opens on a fresh agent, shown. */
export function agentOpened(s: AppState, a: { text: string; ids: string[]; date: string }): AppState {
  const spawn = (s.agent?.spawn ?? 0) + 1;
  // The notes list a hand-off came from closes, so the agent shows.
  const overlay = s.overlay === "notes" ? null : s.overlay;
  return { ...s, overlay, agent: { spawn, text: a.text, ids: a.ids, date: a.date, shown: true, status: { kind: "starting" } } };
}

/** The status of the panel's agent of `spawn`; another spawn's report is dropped. */
export function agentStatus(s: AppState, spawn: number, status: AgentPanel["status"]): AppState {
  if (!s.agent || s.agent.spawn !== spawn) return s;
  return { ...s, agent: { ...s.agent, status } };
}
