// The notes list (`N`, PERSO-315), the TUI's overlay: every note of every
// account, newest first, live on `note.changed`. Enter opens the note's mail,
// `a` adds a note asking for a new email, `e` edits, `d` drops, `D` clears
// the done notes, `y` copies the batch prompt and `x` hands the open notes
// to the agent.

import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { clearDoneNotes, copyNotesExport, dropNote, editNote, handOffNotes, openNewMailNote, openNoteMail } from "@/app/noteActions";
import { NOTE_LABEL, noteInstruction, noteTitle } from "@/app/notes";
import { NoteGlyph } from "@/components/list/RowControls";
import { accountNames } from "@/app/state";
import { useAppState, useDispatch } from "@/app/store";
import * as cmd from "@/lib/commands";
import type { Note, NotePreset } from "@/protocol/types";

export type NotesListDialogProps = { open: boolean; onOpenChange: (open: boolean) => void };

/** Where the cursor goes when `id` leaves `notes`: the next row, else the one before. */
export function cursorAfterDrop(notes: readonly Note[], id: string): string | null {
  const at = notes.findIndex((n) => n.id === id);
  if (at < 0) return notes[0]?.id ?? null;
  return notes[at + 1]?.id ?? notes[at - 1]?.id ?? null;
}

/**
 * A single-select listbox of the notes, the first under the cursor on open:
 * `j`/Down and `k`/Up move, the letter keys act on the cursor note or the
 * list, Escape or `q` closes.
 */
export function NotesListDialog({ open, onOpenChange }: NotesListDialogProps) {
  const dispatch = useDispatch();
  const state = useAppState();
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const loadable = state.notes;
  const data = loadable.data;
  const notes = data ?? [];
  const [cursor, setCursor] = useState<string | null>(null);
  const [presets, setPresets] = useState<NotePreset[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const latest = useRef({ cursor, notes, busy, state });
  latest.current = { cursor, notes, busy, state };

  // A new opening starts on the first row, with the labels of the presets.
  useEffect(() => {
    setCursor(latest.current.notes[0]?.id ?? null);
    setBusy(false);
    setError(null);
    if (!open) return;
    let live = true;
    cmd
      .notePresets()
      .then((p) => live && setPresets(p.presets))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [open]);

  // The cursor stays on its note while the list changes, else the first row.
  useEffect(() => {
    const listed = data ?? [];
    setCursor((c) => (c !== null && listed.some((n) => n.id === c) ? c : (listed[0]?.id ?? null)));
  }, [data]);

  const current = () => latest.current.notes.find((n) => n.id === latest.current.cursor) ?? null;

  const actions = {
    move(by: 1 | -1) {
      const { notes: ns, cursor: c } = latest.current;
      if (ns.length === 0) return;
      const at = c === null ? -1 : ns.findIndex((n) => n.id === c);
      setCursor(ns[Math.max(0, Math.min(ns.length - 1, at + by))].id);
    },
    open() {
      const note = current();
      if (!note) return;
      // A note for a new email has no mail to open: Enter edits it, as the TUI's.
      if (!note.selector) return void editNote(note, dispatch);
      openNoteMail(note, dispatch);
    },
    add() {
      const s = latest.current.state;
      void openNewMailNote(s.search?.account ?? s.selection.account ?? accountNames(s)[0] ?? null, dispatch);
    },
    edit() {
      const note = current();
      if (!note) return setError("No note to edit");
      void editNote(note, dispatch);
    },
    async drop() {
      const note = current();
      if (!note) return setError("No note to drop");
      if (latest.current.busy) return;
      const before = latest.current.notes;
      setBusy(true);
      const out = await dropNote(note, dispatch);
      setBusy(false);
      setError(out.ok ? null : out.error);
      if (out.ok) setCursor(cursorAfterDrop(before, note.id));
    },
    async clearDone() {
      if (latest.current.busy) return;
      setBusy(true);
      const out = await clearDoneNotes(dispatch);
      setBusy(false);
      setError(out.ok ? null : out.error);
    },
    copy() {
      copyNotesExport(dispatch);
    },
    handOff() {
      handOffNotes(latest.current.state, dispatch);
    },
  };
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const run = actionsRef.current;
      const act = (f: () => unknown) => {
        e.preventDefault();
        void f();
      };
      switch (e.key) {
        case "j":
        case "ArrowDown":
          return act(() => run.move(1));
        case "k":
        case "ArrowUp":
          return act(() => run.move(-1));
        case "Enter": {
          const active = document.activeElement;
          if (active instanceof HTMLButtonElement && listRef.current?.closest('[role="dialog"]')?.contains(active)) return;
          return act(() => run.open());
        }
        case "a":
          return act(() => run.add());
        case "e":
          return act(() => run.edit());
        case "d":
          return act(() => run.drop());
        case "D":
          return act(() => run.clearDone());
        case "y":
          return act(() => run.copy());
        case "x":
          return act(() => run.handOff());
        case "q":
          return act(() => onOpenChange(false));
        default:
          return;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onOpenChange]);

  const optionId = (noteId: string) => `${id}-option-${notes.findIndex((n) => n.id === noteId)}`;
  const empty = data !== null && notes.length === 0;
  const open_ = notes.filter((n) => n.state === "open").length;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={listRef} data-slot="notes-list" className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Notes</DialogTitle>
          <DialogDescription>
            Every account's notes for the agent, newest first; {open_} open. x hands the open ones to the agent, which drafts and never
            sends.
          </DialogDescription>
        </DialogHeader>
        <div
          ref={listRef}
          role="listbox"
          aria-label="Notes"
          tabIndex={0}
          aria-activedescendant={cursor !== null && notes.some((n) => n.id === cursor) ? optionId(cursor) : undefined}
          className="flex max-h-96 min-h-16 flex-col gap-0.5 overflow-y-auto rounded-lg outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {data === null && loadable.error ? <p className="px-2 py-1.5 text-sm text-destructive">{loadable.error.message}</p> : null}
          {empty ? <p className="px-2 py-1.5 text-sm text-muted-foreground">No notes; n on a message adds one, a a note for a new email</p> : null}
          {notes.map((note) => (
              <div
                key={note.id}
                id={optionId(note.id)}
                role="option"
                aria-selected={note.id === cursor}
                aria-label={`${NOTE_LABEL[note.state]}: ${noteTitle(note)}`}
                data-note={note.id}
                data-state={note.state}
                onClick={() => setCursor(note.id)}
                onDoubleClick={() => openNoteMail(note, dispatch)}
                className="flex cursor-default flex-col gap-0.5 rounded-md px-2 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
              >
                <span className="flex items-baseline gap-2">
                  <span aria-hidden="true" className="flex w-4 shrink-0 justify-center self-center">
                    <NoteGlyph state={note.state} />
                  </span>
                  <span className="min-w-0 flex-1 truncate font-medium">{noteTitle(note)}</span>
                  {note.orphaned ? <span className="shrink-0 text-xs text-warning">mail gone</span> : null}
                  <span className="shrink-0 text-xs text-muted-foreground">{note.account}</span>
                </span>
                <span className="truncate pl-6 text-xs text-muted-foreground">{noteInstruction(note, presets) || "(no instruction)"}</span>
                {note.state === "done" && note.result ? (
                  <span data-slot="note-result" className="truncate pl-6 text-xs">
                    {note.result}
                  </span>
                ) : null}
              </div>
          ))}
        </div>
        <p role="alert" data-slot="notes-error" className="min-h-5 text-sm text-destructive">
          {error}
        </p>
        <DialogFooter className="flex-wrap">
          <Button variant="outline" size="sm" onClick={() => actions.add()}>
            New email <Kbd aria-hidden="true">a</Kbd>
          </Button>
          <Button variant="outline" size="sm" disabled={cursor === null} onClick={() => actions.edit()}>
            Edit <Kbd aria-hidden="true">e</Kbd>
          </Button>
          <Button variant="outline" size="sm" disabled={busy || cursor === null} onClick={() => void actions.drop()}>
            Drop <Kbd aria-hidden="true">d</Kbd>
          </Button>
          <Button variant="outline" size="sm" disabled={busy} onClick={() => void actions.clearDone()}>
            Clear done <Kbd aria-hidden="true">D</Kbd>
          </Button>
          <Button variant="outline" size="sm" onClick={() => actions.copy()}>
            Copy prompt <Kbd aria-hidden="true">y</Kbd>
          </Button>
          <Button size="sm" disabled={open_ === 0} onClick={() => actions.handOff()}>
            Hand off <Kbd aria-hidden="true">x</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
