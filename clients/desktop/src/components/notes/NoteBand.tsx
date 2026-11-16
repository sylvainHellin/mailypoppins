// The note band above the reader's body (PERSO-315): the note on the shown
// mail, as `note_get` answers it, with its instruction and, once the agent
// closed it, its result. `n` edits it, `N` lists every note.

import { useEffect, useState } from "react";
import { NoteGlyph } from "@/components/list/RowControls";
import { bandNote, NOTE_LABEL, noteInstruction } from "@/app/notes";
import { useAppState } from "@/app/store";
import * as cmd from "@/lib/commands";
import type { NotePreset } from "@/protocol/types";

export function NoteBand() {
  const s = useAppState();
  const note = bandNote(s);
  const [presets, setPresets] = useState<NotePreset[]>([]);
  const preset = note?.preset ?? null;

  // The preset's label, read from the daemon, which alone holds the presets.
  useEffect(() => {
    if (!preset) return;
    let live = true;
    cmd
      .notePresets()
      .then((p) => live && setPresets(p.presets))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [preset]);

  if (!note) return null;
  return (
    <section
      aria-label="Note"
      data-slot="note-band"
      data-state={note.state}
      className="mx-5 mt-3 flex flex-col gap-1 rounded-lg border border-border bg-muted/40 px-3 py-2 text-sm"
    >
      <p className="flex items-center gap-2">
        <NoteGlyph state={note.state} />
        <span className="font-medium">{NOTE_LABEL[note.state]}</span>
        <span className="min-w-0 flex-1 truncate text-muted-foreground">{noteInstruction(note, presets) || "(no instruction)"}</span>
        <span className="shrink-0 text-xs text-muted-foreground">n edits</span>
      </p>
      {note.state === "done" && note.result ? (
        <p data-slot="note-result" className="whitespace-pre-wrap">
          {note.result}
        </p>
      ) : null}
    </section>
  );
}
