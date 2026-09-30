// The New invitation form (SND-05, the CLI's `mp send --invite`; the TUI has
// none): To, Cc, Subject, Start, End or Duration, Location, Description.
// A Graph account cannot send one, and the form says so with the daemon's
// sentence and takes no input.

import { useEffect, useId, useRef, useState, type ComponentProps, type KeyboardEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { submitInvite } from "@/app/invite";
import { useAppState, useDispatch } from "@/app/store";
import type { InviteDialog } from "@/app/state";

type Fields = { to: string; cc: string; subject: string; start: string; end: string; duration: string; location: string; description: string };

const EMPTY: Fields = { to: "", cc: "", subject: "", start: "", end: "", duration: "", location: "", description: "" };

export type NewInvitationDialogProps = { dialog: InviteDialog | null; onOpenChange: (open: boolean) => void };

/**
 * Enter in a field moves to the next one, Cmd+Enter or Ctrl+Enter sends,
 * Escape cancels. What the form lacks and every refusal show in its alert,
 * and the form stays open with what was typed.
 */
export function NewInvitationDialog({ dialog, onOpenChange }: NewInvitationDialogProps) {
  const s = useAppState();
  const dispatch = useDispatch();
  const id = useId();
  const [fields, setFields] = useState<Fields>(EMPTY);
  const [length, setLength] = useState<"end" | "duration">("duration");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const toRef = useRef<HTMLInputElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const open = dialog !== null;
  const account = dialog?.account ?? null;
  const refusal = account ? (s.inviteRefusals[account] ?? null) : null;

  useEffect(() => {
    setFields(EMPTY);
    setLength("duration");
    setError(null);
    setBusy(false);
  }, [dialog]);

  const submit = async () => {
    if (!account || busy || refusal) return;
    setBusy(true);
    const refused = await submitInvite(dispatch, account, {
      subject: fields.subject,
      start: fields.start,
      to: fields.to,
      cc: fields.cc,
      end: length === "end" ? fields.end : null,
      duration: length === "duration" ? fields.duration : null,
      location: fields.location,
      description: fields.description,
    });
    setBusy(false);
    setError(refused);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLFormElement>) => {
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    if (e.metaKey || e.ctrlKey) {
      e.preventDefault();
      void submit();
      return;
    }
    // Enter moves on from a one-line field, to the send button after the last.
    if (!(e.target instanceof HTMLInputElement) || e.target.type === "radio") return;
    e.preventDefault();
    const order = [...(formRef.current?.querySelectorAll<HTMLElement>("[data-field], [data-submit]") ?? [])];
    order[order.indexOf(e.target) + 1]?.focus();
  };

  const set = (name: keyof Fields) => (e: { currentTarget: { value: string } }) => setFields({ ...fields, [name]: e.currentTarget.value });
  const row = (name: keyof Fields, label: string, input: ReactNode) => (
    <div className="grid grid-cols-[5.5rem_1fr] items-center gap-2">
      <label htmlFor={`${id}-${name}`} className="text-sm text-muted-foreground">
        {label}
      </label>
      {input}
    </div>
  );
  const field = (name: keyof Fields, label: string, extra: ComponentProps<typeof Input> = {}) =>
    row(
      name,
      label,
      <Input
        id={`${id}-${name}`}
        ref={name === "to" ? toRef : undefined}
        data-field={name}
        value={fields[name]}
        autoComplete="off"
        spellCheck={name === "subject" || name === "location"}
        disabled={refusal !== null}
        onChange={set(name)}
        {...extra}
      />,
    );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={refusal ? cancelRef : toRef} data-slot="new-invitation" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>New invitation</DialogTitle>
          <DialogDescription>
            {account ? `Sent from ${account}; each recipient gets an invitation they can accept or decline.` : ""}
          </DialogDescription>
        </DialogHeader>
        {refusal ? (
          <p data-slot="invite-refusal" className="text-sm text-warning">
            {refusal}
          </p>
        ) : null}
        <form
          ref={formRef}
          aria-label="New invitation"
          className="flex flex-col gap-2"
          onKeyDown={onKeyDown}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {field("to", "To")}
          {field("cc", "Cc")}
          {field("subject", "Subject")}
          {field("start", "Start", { type: "datetime-local" })}
          <fieldset className="grid grid-cols-[5.5rem_1fr] items-center gap-2" disabled={refusal !== null}>
            <legend className="sr-only">End or duration</legend>
            <span className="flex flex-col gap-0.5 text-sm text-muted-foreground">
              <label className="flex items-center gap-1.5">
                <input type="radio" name={`${id}-length`} checked={length === "end"} onChange={() => setLength("end")} />
                Ends at
              </label>
              <label className="flex items-center gap-1.5">
                <input type="radio" name={`${id}-length`} checked={length === "duration"} onChange={() => setLength("duration")} />
                Lasts
              </label>
            </span>
            {length === "end" ? (
              <Input
                aria-label="End"
                data-field="end"
                type="datetime-local"
                value={fields.end}
                disabled={refusal !== null}
                onChange={set("end")}
              />
            ) : (
              <Input
                aria-label="Duration"
                data-field="duration"
                value={fields.duration}
                placeholder="1h30m or PT1H30M"
                autoComplete="off"
                spellCheck={false}
                disabled={refusal !== null}
                onChange={set("duration")}
              />
            )}
          </fieldset>
          {field("location", "Location")}
          {row(
            "description",
            "Description",
            <Textarea
              id={`${id}-description`}
              data-field="description"
              rows={3}
              value={fields.description}
              disabled={refusal !== null}
              onChange={set("description")}
            />,
          )}
          <p role="alert" data-slot="invite-error" className="min-h-5 text-sm text-destructive">
            {error}
          </p>
          <DialogFooter>
            <DialogClose render={<Button ref={cancelRef} type="button" variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" data-submit="" disabled={busy || refusal !== null}>
              Send invitation <Kbd aria-hidden="true">⌘↵</Kbd>
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
