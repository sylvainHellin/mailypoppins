// The notes on mail (PERSO-315), as the fixture keeps them: src-tauri/src/fixture/notes.rs
// in TypeScript, answering the `note_*` commands with the daemon's sentences.
// A note on a received message is held by its account and Message-ID, a note on
// a draft by its account and id, a note for a new email by neither; a selector is
// read for its account and key only. What a selector names, and where a note's
// mail is now, come from the backend through `NoteWorld`.

import type { GuiError } from "@/lib/gui-types";
import type { Note, NoteExport, NotePreset, NoteState } from "@/protocol/types";

/** The daemon's built-in presets (`mp_core::config::default_note_presets`), what the fixture's configuration has. */
export const FIXTURE_PRESETS: NotePreset[] = [
  { key: "r", label: "Reply", needs_text: true, instruction: "Draft a reply in my voice from this gist." },
  { key: "d", label: "Decline", needs_text: true, instruction: "Draft a polite refusal; use my reason if I gave one." },
  { key: "s", label: "Summarize", needs_text: false, instruction: "Summarize in three lines: who, what is asked, by when. Post it as the result." },
  {
    key: "e",
    label: "Explain",
    needs_text: false,
    instruction: "Explain the context from the vault and past mail: who this is, what came before. Post it as the result.",
  },
  {
    key: "p",
    label: "Review",
    needs_text: false,
    instruction:
      "Review this draft before I approve it: spelling and grammar (fix them in the draft), an attachment the text mentions but the draft lacks, a question from the mail it answers that it leaves unanswered, wrong names or dates. Post what you found as the result; change nothing beyond spelling and grammar.",
  },
  {
    key: "t",
    label: "Task",
    needs_text: false,
    instruction: "Turn this mail into a Plane task or a vault note, whichever fits; post the link as the result.",
  },
  { key: "f", label: "Forward", needs_text: true, instruction: "Draft a forward to the person named, with a short cover line." },
];

/** What a selector names, with the snapshot a new note takes of it. */
export type NoteTarget = {
  account: string;
  selector: string;
  message_id?: string;
  draft_id?: string;
  from: string;
  to: string;
  subject: string;
};

/** The rows and drafts the notes are on, as the backend holds them. */
export interface NoteWorld {
  /** The message or draft `selector` names; throws the daemon's refusal. */
  target(account: string | undefined, selector: string): NoteTarget;
  /** The selector of the mail `note` is on now; null when it is gone. */
  locate(note: Note): string | null;
  knows(account: string): boolean;
}

/** A `-32602` refusal as the Rust layer passes it on: a `protocol` error with the sentence and no code. */
export function noteRefusal(message: string): GuiError {
  return { kind: "protocol", message, code: null };
}

type Key = { account: string; message_id?: string; draft_id?: string };

/** The account and key of `selector`, a full `mp://<account>/<mailbox>/<key>` or, with `account`, `<mailbox>/<key>`. */
export function keyOf(selector: string, account?: string): Key {
  const refused = () => noteRefusal(`\`${selector}\` is not a selector of a message or a draft`);
  let rest = selector;
  let acct = account;
  if (selector.startsWith("mp://")) {
    const tail = selector.slice("mp://".length);
    const at = tail.indexOf("/");
    if (at < 0) throw refused();
    acct = tail.slice(0, at);
    rest = tail.slice(at + 1);
  }
  const slash = rest.indexOf("/");
  if (!acct || slash <= 0) throw refused();
  const mailbox = rest.slice(0, slash);
  const key = rest.slice(slash + 1).split("/")[0].replace(/^</, "").replace(/>$/, "");
  if (!key) throw refused();
  return mailbox === "drafts" ? { account: acct, draft_id: key } : { account: acct, message_id: key };
}

function isOn(note: Note, key: Key): boolean {
  if (note.account !== key.account) return false;
  return key.message_id !== undefined ? note.message_id === key.message_id : note.draft_id === key.draft_id;
}

const bare = (id: string) => id.replace(/^</, "").replace(/>$/, "");

export class NoteBook {
  /** Oldest first. */
  notes: Note[] = [];
  private nextId = 1;
  private ticks = 0;
  /** What every write calls with the whole list, the `note.changed` payload. */
  onChanged: (notes: Note[]) => void = () => {};

  /** The state of the note on a message (by Message-ID) or a draft (by id) of `account`. */
  stateOf(account: string, messageId: string | null, draftId: string | null): NoteState | undefined {
    const m = messageId === null ? null : bare(messageId);
    return this.notes.find((n) => n.account === account && ((m !== null && n.message_id === m) || (draftId !== null && n.draft_id === draftId)))?.state;
  }

  /** Every note, newest first, located again. */
  listing(world: NoteWorld): Note[] {
    return [...this.notes].reverse().map((n) => located(n, world));
  }

  private now(): string {
    this.ticks += 1;
    const two = (n: number) => String(n).padStart(2, "0");
    return `2026-11-16T09:${two(Math.floor(this.ticks / 60) % 60)}:${two(this.ticks % 60)}Z`;
  }

  private byId(id: string): Note {
    const note = this.notes.find((n) => n.id === id);
    if (!note) throw noteRefusal(`no note has the id ${id}`);
    return note;
  }

  private changed(world: NoteWorld): void {
    this.onChanged(this.listing(world));
  }

  private setOn(target: NoteTarget | null, account: string, preset: string | undefined, text: string): Note {
    const now = this.now();
    const key: Key | null = target ? { account: target.account, message_id: target.message_id, draft_id: target.draft_id } : null;
    const old = key ? this.notes.find((n) => isOn(n, key)) : undefined;
    const fresh: Note = {
      id: old?.id ?? `f${String(this.nextId++).padStart(7, "0")}`,
      account: target?.account ?? account,
      text,
      state: "open",
      created: old?.created ?? now,
      updated: now,
      from: target?.from ?? "",
      to: target?.to ?? "",
      subject: target?.subject ?? "",
      orphaned: false,
    };
    if (target) fresh.selector = target.selector;
    if (target?.message_id) fresh.message_id = target.message_id;
    if (target?.draft_id) fresh.draft_id = target.draft_id;
    if (preset) fresh.preset = preset;
    if (old) this.notes[this.notes.indexOf(old)] = fresh;
    else this.notes.push(fresh);
    return fresh;
  }

  presets(): { presets: NotePreset[] } {
    return { presets: structuredClone(FIXTURE_PRESETS) };
  }

  list(world: NoteWorld): { notes: Note[] } {
    return { notes: this.listing(world) };
  }

  get(world: NoteWorld, selector: string): { note: Note | null } {
    const key = keyOf(selector);
    const note = this.notes.find((n) => isOn(n, key));
    return { note: note ? located(note, world) : null };
  }

  set(world: NoteWorld, params: { account?: string; selector?: string; id?: string; preset?: string; text?: string }): Note {
    const { preset, text } = content(params);
    let note: Note;
    if (params.selector !== undefined && params.id !== undefined) throw noteRefusal("selector and id are two addresses; send at most one");
    if (params.selector !== undefined) {
      needContent(preset, text);
      const target = world.target(params.account, params.selector);
      note = this.setOn(target, target.account, preset, text ?? "");
    } else if (params.id !== undefined) {
      const old = this.byId(params.id);
      const p = preset ?? old.preset;
      const t = text ?? old.text;
      needContent(p, t);
      note = { ...old, text: t, state: "open", updated: this.now() };
      if (p) note.preset = p;
      delete note.result;
      this.notes[this.notes.indexOf(old)] = note;
    } else {
      needContent(preset, text);
      if (!params.account) {
        throw noteRefusal("a note for a new email names the account it goes out from; send account, or a selector or an id");
      }
      if (!world.knows(params.account)) {
        throw { kind: "not_found", message: `note.set: the daemon refused the call: account_unknown: ${params.account} (-32005)`, code: -32005 } satisfies GuiError;
      }
      note = this.setOn(null, params.account, preset, text ?? "");
    }
    this.changed(world);
    return located(note, world);
  }

  setMany(world: NoteWorld, params: { account?: string; selectors: string[]; preset?: string; text?: string }): { notes: Note[] } {
    const { preset, text } = content(params);
    needContent(preset, text);
    if (params.selectors.length === 0) throw noteRefusal("selectors names no message or draft");
    const targets = params.selectors.map((s) => world.target(params.account, s));
    const set: Note[] = [];
    for (const t of targets) {
      const note = this.setOn(t, t.account, preset, text ?? "");
      if (!set.some((n) => n.id === note.id)) set.push(note);
    }
    this.changed(world);
    return { notes: set.map((n) => located(this.byId(n.id), world)) };
  }

  /** `note.done`, which only the agent calls; the tests close a note with it. */
  done(world: NoteWorld, params: { selector?: string; id?: string; result: string }): Note {
    if (!params.result.trim()) throw noteRefusal("result is the line a client shows on the mail, and may not be empty");
    const note = params.id !== undefined ? this.byId(params.id) : this.bySelector(params.selector ?? "");
    const closed: Note = { ...note, state: "done", result: params.result, updated: this.now() };
    this.notes[this.notes.indexOf(note)] = closed;
    this.changed(world);
    return located(closed, world);
  }

  private bySelector(selector: string): Note {
    const key = keyOf(selector);
    const note = this.notes.find((n) => isOn(n, key));
    if (!note) throw noteRefusal(`no note is on ${selector}`);
    return note;
  }

  clear(world: NoteWorld, id: string): { removed: number } {
    const note = this.byId(id);
    this.notes = this.notes.filter((n) => n !== note);
    this.changed(world);
    return { removed: 1 };
  }

  clearDone(world: NoteWorld): { removed: number } {
    const before = this.notes.length;
    this.notes = this.notes.filter((n) => n.state !== "done");
    const removed = before - this.notes.length;
    if (removed > 0) this.changed(world);
    return { removed };
  }

  export(world: NoteWorld, ids: string[] | null, handoff: boolean): NoteExport {
    const listed = this.listing(world);
    let picked: Note[];
    if (ids === null) picked = listed.filter((n) => n.state === "open");
    else {
      const unknown = ids.find((id) => !listed.some((n) => n.id === id));
      if (unknown !== undefined) throw noteRefusal(`no note has the id ${unknown}`);
      picked = listed.filter((n) => ids.includes(n.id));
    }
    const out = { text: renderBatch(picked, FIXTURE_PRESETS), ids: picked.map((n) => n.id) };
    if (handoff) {
      const now = this.now();
      let moved = false;
      this.notes = this.notes.map((n) => {
        if (!out.ids.includes(n.id) || n.state !== "open") return n;
        moved = true;
        return { ...n, state: "handed_off", updated: now };
      });
      if (moved) this.changed(world);
    }
    return out;
  }
}

function located(note: Note, world: NoteWorld): Note {
  const n = { ...note };
  if (n.message_id === undefined && n.draft_id === undefined) return n;
  const at = world.locate(n);
  if (at === null) n.orphaned = true;
  else {
    n.selector = at;
    n.orphaned = false;
  }
  return n;
}

function content(params: { preset?: string; text?: string }): { preset: string | undefined; text: string | undefined } {
  const preset = params.preset ?? undefined;
  if (preset !== undefined && !FIXTURE_PRESETS.some((p) => p.key === preset)) {
    throw noteRefusal(`preset ${preset} is not configured; the presets are ${FIXTURE_PRESETS.map((p) => p.key).join(", ")}`);
  }
  return { preset, text: params.text === undefined || params.text === null ? undefined : params.text.trimEnd() };
}

function needContent(preset: string | undefined, text: string | undefined): void {
  if (preset === undefined && (text === undefined || !text.trim())) throw noteRefusal("a note needs a preset or a text");
}

/** The batch prompt, as the daemon's `notes::render` words it. */
export function renderBatch(notes: Note[], presets: NotePreset[]): string {
  if (notes.length === 0) return "";
  let out = notes.length === 1 ? "I have a note on one email.\n" : `I have notes on ${notes.length} emails.\n`;
  out +=
    "Read each with `mp show <selector>` (a draft: `mp path <selector>` gives its file), carry out the instruction, and close it with `mp notes done <selector> --result \"...\"`.\n" +
    "A note without a selector asks for a new email: write it as its item says, then close it with `mp notes done --id <id> --result \"...\"`.\n" +
    "Never send or approve anything.\n\n";
  notes.forEach((note, index) => {
    const lines: string[] = [];
    if (note.selector !== undefined) {
      lines.push(note.selector);
      const who = note.draft_id !== undefined ? `Draft to ${note.to}` : `From ${note.from}`;
      lines.push(`${who}, subject "${note.subject}"`);
    } else {
      lines.push(`New email from account ${note.account} (note ${note.id})`);
    }
    if (note.preset !== undefined) {
      const p = presets.find((x) => x.key === note.preset);
      lines.push(p ? `${p.label}: ${p.instruction}` : `Preset ${note.preset}, which the configuration no longer has.`);
    }
    if (note.text) lines.push(...note.text.split("\n"));
    const number = `${index + 1}. `;
    lines.forEach((line, at) => {
      out += `${at === 0 ? number : " ".repeat(number.length)}${line}\n`;
    });
  });
  return out;
}
