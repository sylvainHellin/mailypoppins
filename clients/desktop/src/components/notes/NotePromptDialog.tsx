// The note prompt (`n`, PERSO-315), the TUI's: the daemon's presets as
// buttons with their letter, Space for free text, a text line where the
// preset needs one, and on a done note its result, Backspace to clear it or
// a letter for a new instruction. Nothing is written until a letter that
// needs no text, Enter on the text line, Backspace on a done note, or a button.

import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { dropNote, saveNote } from "@/app/noteActions";
import { noteInstruction } from "@/app/notes";
import type { NotePrompt } from "@/app/state";
import { useDispatch } from "@/app/store";

export type NotePromptDialogProps = { prompt: NotePrompt | null };

export const NOTE_TEXT_ID = "mp-note-text";

/**
 * The dialog owns every key while it is open. On the letters step a preset's
 * letter picks it, saving at once when it needs no text, and Space goes to
 * free text; on the text step the field takes the keys and Enter saves; on a
 * done note, which shows its result, Backspace clears it and a letter or
 * Space gives it a new instruction. Escape closes, back to the notes list
 * when the prompt came from it.
 */
export function NotePromptDialog({ prompt }: NotePromptDialogProps) {
  const dispatch = useDispatch();
  const panelRef = useRef<HTMLDivElement>(null);
  const textRef = useRef<HTMLInputElement>(null);
  const open = prompt !== null;
  const [text, setText] = useState(prompt?.text ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // A new prompt, or a new step, starts from what the prompt holds.
  useEffect(() => {
    setText(prompt?.text ?? "");
    setBusy(false);
    setError(null);
  }, [prompt?.step, prompt?.editing, prompt?.selectors]);

  // The text step hands the keys to the field.
  useEffect(() => {
    if (prompt?.step === "text") queueMicrotask(() => textRef.current?.focus());
  }, [prompt?.step, prompt?.preset]);

  const close = () => dispatch({ type: "overlay", overlay: prompt?.returnTo ?? null });

  const latest = useRef({ prompt, busy });
  latest.current = { prompt, busy };

  const save = async (preset: string | null, line: string) => {
    const { prompt: p, busy: b } = latest.current;
    if (!p || b) return;
    if (preset === null && !line.trim()) return setError("A note needs a preset or a text");
    setBusy(true);
    const out = await saveNote(p, preset, line, dispatch);
    setBusy(false);
    if (!out.ok) setError(out.error);
  };
  const clear = async () => {
    const { prompt: p, busy: b } = latest.current;
    if (!p?.editing || b) return;
    setBusy(true);
    const out = await dropNote(p.editing, dispatch);
    setBusy(false);
    if (out.ok) close();
    else setError(out.error);
  };
  /** A preset's letter or button: saved at once when it needs no text, else the text line. */
  const pick = (key: string | null) => {
    const p = latest.current.prompt;
    if (!p) return;
    const preset = key === null ? null : p.presets.find((x) => x.key === key);
    if (preset && !preset.needs_text) return void save(preset.key, "");
    dispatch({ type: "note_prompt_pick", preset: preset ? preset.key : null });
  };
  const actions = useRef({ save, clear, pick });
  actions.current = { save, clear, pick };

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const p = latest.current.prompt;
      if (!p || p.step === "text") return;
      // A button of the dialog that has the focus takes its own Enter and Space.
      const active = document.activeElement;
      if ((e.key === "Enter" || e.key === " ") && active instanceof HTMLButtonElement && panelRef.current?.closest('[role="dialog"]')?.contains(active)) return;
      if (p.step === "done" && e.key === "Backspace") return e.preventDefault(), void actions.current.clear();
      if (e.key === " ") return e.preventDefault(), actions.current.pick(null);
      if (e.key.length === 1 && p.presets.some((x) => x.key === e.key)) return e.preventDefault(), actions.current.pick(e.key);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, dispatch]);

  const picked = prompt?.preset ? prompt.presets.find((x) => x.key === prompt.preset) : null;
  const editing = prompt?.editing ?? null;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && close()}>
      <DialogContent initialFocus={prompt?.step === "text" ? textRef : panelRef} data-slot="note-prompt" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Note</DialogTitle>
          <DialogDescription>
            {prompt?.selectors.length === 0 && !editing?.selector ? "A note asking the agent for " : "A note for the agent on "}
            <span className="font-medium text-foreground">{prompt?.title}</span>
            {prompt && prompt.selectors.length > 1 ? ", the same note on each" : ""}. The agent drafts, never sends.
          </DialogDescription>
        </DialogHeader>
        <div ref={panelRef} tabIndex={-1} className="flex flex-col gap-3 outline-none">
          {prompt?.step === "done" && editing ? (
            <div data-slot="note-done" className="flex flex-col gap-2 text-sm">
              <p>
                <span className="text-muted-foreground">Instruction: </span>
                {noteInstruction(editing, prompt.presets) || "(none)"}
              </p>
              <p>
                <span className="text-muted-foreground">Result: </span>
                {editing.result ?? ""}
              </p>
              <div className="flex items-center gap-2">
                <Button variant="outline" size="sm" disabled={busy} onClick={() => void clear()}>
                  Clear the note <Kbd aria-hidden="true">⌫</Kbd>
                </Button>
                <span className="text-xs text-muted-foreground">or a preset for a new instruction</span>
              </div>
            </div>
          ) : null}
          <div role="group" aria-label="Presets" className="grid grid-cols-2 gap-1.5">
            {prompt?.presets.map((p) => (
              <Button
                key={p.key}
                variant={prompt.step === "text" && prompt.preset === p.key ? "secondary" : "outline"}
                size="sm"
                aria-pressed={prompt.step === "text" ? prompt.preset === p.key : undefined}
                disabled={busy}
                title={p.instruction}
                data-preset={p.key}
                className="justify-start"
                onClick={() => pick(p.key)}
              >
                <Kbd aria-hidden="true">{p.key}</Kbd>
                {p.label}
                {p.needs_text ? <span className="text-xs text-muted-foreground">…</span> : null}
              </Button>
            ))}
            <Button
              variant={prompt?.step === "text" && prompt.preset === null ? "secondary" : "outline"}
              size="sm"
              disabled={busy}
              data-preset=""
              className="justify-start"
              onClick={() => pick(null)}
            >
              <Kbd aria-hidden="true">Space</Kbd>
              Free text
            </Button>
          </div>
          {prompt?.step === "text" ? (
            <label className="flex flex-col gap-1 text-sm">
              <span className="text-muted-foreground">{picked ? `${picked.label}: ${picked.instruction}` : "Free text"}</span>
              <Input
                id={NOTE_TEXT_ID}
                ref={textRef}
                value={text}
                disabled={busy}
                aria-label="Note text"
                placeholder={picked ? "Your gist, then Enter" : "What the agent should do, then Enter"}
                onChange={(e) => setText(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
                  e.preventDefault();
                  void save(prompt.preset, text);
                }}
              />
            </label>
          ) : null}
        </div>
        <p role="alert" data-slot="note-prompt-error" className="min-h-5 text-sm text-destructive">
          {error}
        </p>
      </DialogContent>
    </Dialog>
  );
}
