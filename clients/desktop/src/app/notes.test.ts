import { describe, expect, it } from "vitest";
import { agentRunning, bandNote, bandWanted, localDate, noteInstruction, noteTitle, notesLoaded, promptFor } from "@/app/notes";
import { noteTargets } from "@/app/noteActions";
import { reducer, visibleItems, type Action } from "@/app/reducer";
import { filteredDrafts, filteredRows, initialState, isStale, type AppState } from "@/app/state";
import type { MessageList } from "@/lib/gui-types";
import type { Note, NotePreset } from "@/protocol/types";
import { fixtures } from "@/test/tauri-mock";

const run = (s: AppState, ...actions: Action[]) => actions.reduce(reducer, s);

const rebootstrapped: Action = { type: "gui_event", event: { type: "rebootstrapped", cause: "subscribed", bootstrap: fixtures.bootstrap } };

function changed(notes: Note[], revision: number): Action {
  return {
    type: "gui_event",
    event: { type: "event", event: { instance_id: fixtures.bootstrap.instance_id, revision, kind: "note.changed", payload: { notes } } },
  };
}

const PRESETS: NotePreset[] = [
  { key: "r", label: "Reply", needs_text: true, instruction: "Draft a reply." },
  { key: "s", label: "Summarize", needs_text: false, instruction: "Summarize." },
];

function note(over: Partial<Note> = {}): Note {
  return {
    id: "f0000001",
    selector: "mp://work/inbox/a@x",
    account: "work",
    message_id: "a@x",
    text: "",
    state: "open",
    created: "2026-11-16T09:00:01Z",
    updated: "2026-11-16T09:00:01Z",
    from: "Ann <ann@x>",
    to: "me@example.com",
    subject: "Lunch",
    orphaned: false,
    ...over,
  };
}

const row = (id: number, subject: string, noteState?: Note["state"]) => ({
  ...fixtures.messages.work.inbox[0],
  id,
  subject,
  message_id: `<m${id}@x>`,
  selector: `mp://work/inbox/m${id}@x`,
  ...(noteState ? { note: noteState } : {}),
});

/** The shell on `work`'s Inbox listed flat with `rows`, the cursor on the first. */
function listed(rows: ReturnType<typeof row>[]): AppState {
  let s = run(initialState(), rebootstrapped);
  const list: MessageList = { kind: "messages", account: "work", mailbox: "inbox", total: rows.length, rows };
  s = { ...s, selection: { ...s.selection, account: "work", mailbox: "inbox" }, messages: { data: list, gen: 1, loadedGen: 1, error: null, key: "work/inbox" } };
  return run(s, { type: "select_message", message: { row_id: rows[0].id, message_id: rows[0].message_id, selector: rows[0].selector } });
}

describe("the notes model", () => {
  it("note.changed replaces the list, drops a read it overtook, and reads the shown listing again for its glyphs", () => {
    let s = run(listed([row(1, "One")]), { type: "open_notes_list" });
    expect(s.overlay).toBe("notes");
    expect(isStale(s.notes)).toBe(true);
    const asked = s.notes.gen;
    s = run(s, changed([note()], 900));
    expect(s.notes.data).toEqual([note()]);
    expect(isStale(s.notes)).toBe(false);
    expect(isStale(s.messages)).toBe(true);
    expect(notesLoaded(s, asked, []).notes.data).toEqual([note()]);
    expect(isStale(run(s, rebootstrapped).notes)).toBe(true);
  });

  it("the band reads the shown mail's note once per generation, and every note.changed reads it again", () => {
    let s = listed([row(1, "One")]);
    s = { ...s, reader: { ...s.reader, key: "work#1" } };
    const g = s.noteBands.gen;
    const want = bandWanted(s);
    expect(want).toEqual({ selector: "mp://work/inbox/m1@x", gen: g });
    s = run(s, { type: "note_band_loaded", selector: want!.selector, gen: want!.gen, note: note({ selector: want!.selector }) });
    expect(bandWanted(s)).toBeNull();
    expect(bandNote(s)?.id).toBe("f0000001");
    s = run(s, changed([], 901));
    expect(bandWanted(s)).toEqual({ selector: "mp://work/inbox/m1@x", gen: g + 1 });
    // An answer asked before the change is dropped; the band still shows the last one read.
    s = run(s, { type: "note_band_loaded", selector: want!.selector, gen: g, note: null });
    expect(bandNote(s)?.id).toBe("f0000001");
    s = run(s, { type: "note_band_loaded", selector: want!.selector, gen: g + 1, note: null });
    expect(bandNote(s)).toBeNull();
  });

  it("fN narrows the list to the noted rows, beside the text filter, and the moves follow it", () => {
    let s = listed([row(1, "Alpha"), row(2, "Beta", "open"), row(3, "Alpha two", "done")]);
    expect(filteredRows(s.messages.data, "", true).map((r) => r.id)).toEqual([2, 3]);
    expect(filteredRows(s.messages.data, "alpha", true).map((r) => r.id)).toEqual([3]);
    s = run(s, { type: "toggle_noted_only" });
    expect(s.notedOnly).toBe(true);
    expect(visibleItems(s)).toHaveLength(2);
    s = run(s, { type: "toggle_noted_only" });
    expect(visibleItems(s)).toHaveLength(3);
    const drafts: MessageList = { kind: "drafts", account: "work", listing: { ...fixtures.drafts.work, drafts: fixtures.drafts.work.drafts.map((d, i) => (i === 0 ? { ...d, note: "handed_off" } : d)) } };
    expect(filteredDrafts(drafts, "", true).map((d) => d.id)).toEqual(["angebot-antwort"]);
  });

  it("n targets the marked rows, else the cursor row, with its note state", () => {
    let s = listed([row(1, "Alpha", "done"), row(2, "Beta")]);
    expect(noteTargets(s)).toEqual({ selectors: ["mp://work/inbox/m1@x"], account: "work", title: "Alpha", state: "done" });
    s = run(s, { type: "mark_toggle", key: "work#1" }, { type: "mark_toggle", key: "work#2" });
    const marked = noteTargets(s);
    expect(marked).toMatchObject({ selectors: ["mp://work/inbox/m1@x", "mp://work/inbox/m2@x"], title: "2 emails", state: undefined });
    expect(noteTargets(initialState())).toEqual({ refusal: "Select a message first" });
  });

  it("a prompt on a new note waits for a letter; on an open one it edits the text; on a done one it shows the result, to clear or instruct anew", () => {
    const targets = { selectors: ["mp://work/inbox/a@x"], account: "work", title: "Lunch" };
    expect(promptFor(targets, PRESETS, null)).toMatchObject({ step: "keys", preset: null, text: "", editing: null, returnTo: null });
    expect(promptFor(targets, PRESETS, note({ preset: "r", text: "yes" }))).toMatchObject({ step: "text", preset: "r", text: "yes" });
    const done = promptFor(targets, PRESETS, note({ state: "done", result: "Drafted" }), "notes");
    expect(done).toMatchObject({ step: "done", preset: null, text: "", returnTo: "notes" });
    let s = run(initialState(), { type: "open_note_prompt", prompt: done });
    expect(s.overlay).toBe("note_prompt");
    // A letter whose preset needs a text, on a done note, is a new instruction.
    s = run(s, { type: "note_prompt_pick", preset: "r" });
    expect(s.notePrompt).toMatchObject({ step: "text", preset: "r" });
    s = run(s, { type: "overlay", overlay: null });
    expect(s.notePrompt).toBeNull();
  });

  it("the agent panel opens shown, follows its own spawn's status, toggles and closes", () => {
    let s = run(initialState(), { type: "agent_open", text: "I have a note", ids: ["f1"], date: "2026-11-16" });
    expect(s.agent).toMatchObject({ spawn: 1, shown: true, status: { kind: "starting" } });
    expect(agentRunning(s.agent)).toBe(true);
    s = run(s, { type: "agent_status", spawn: 1, status: { kind: "running", session: 3, command: "pi" } });
    s = run(s, { type: "agent_status", spawn: 7, status: { kind: "failed", error: "stale" } });
    expect(s.agent?.status).toEqual({ kind: "running", session: 3, command: "pi" });
    s = run(s, { type: "agent_toggle" });
    expect(s.agent?.shown).toBe(false);
    s = run(s, { type: "agent_status", spawn: 1, status: { kind: "exited", code: 0, signal: null } });
    expect(agentRunning(s.agent)).toBe(false);
    s = run(s, { type: "agent_open", text: "again", ids: ["f2"], date: "2026-11-17" });
    expect(s.agent).toMatchObject({ spawn: 2, shown: true, ids: ["f2"] });
    expect(run(s, { type: "agent_close" }).agent).toBeNull();
  });

  it("names a note's mail and its instruction in the list's words", () => {
    expect(noteTitle(note())).toBe("Lunch");
    expect(noteTitle(note({ selector: undefined, message_id: undefined, account: "home" }))).toBe("a new email from home");
    expect(noteInstruction(note({ preset: "s", text: " in German " }), PRESETS)).toBe("Summarize: in German");
    expect(noteInstruction(note({ preset: "z" }), PRESETS)).toBe("Preset z");
    expect(localDate(new Date(2026, 0, 5, 23, 59))).toBe("2026-01-05");
  });
});
