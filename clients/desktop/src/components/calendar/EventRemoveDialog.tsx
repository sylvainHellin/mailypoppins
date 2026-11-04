// The cancel or delete confirmation (CAL-13, PERSO-255; the TUI's `D`
// overlay, the CLI's `mp event cancel` and `mp event delete`).

import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { submitRemove } from "@/app/eventChange";
import { useDispatch } from "@/app/store";
import type { EventRemoveDialog as EventRemoveState } from "@/app/state";

export type EventRemoveDialogProps = { dialog: EventRemoveState | null; onOpenChange: (open: boolean) => void };

/** The question, which names an occurrence alone or its whole series. */
function title(cancel: boolean, scope: EventRemoveState["scope"]): string {
  const what = scope === "series" ? "the whole series" : scope === "occurrence" ? "this occurrence" : cancel ? "the meeting" : "the event";
  return `${cancel ? "Cancel" : "Delete"} ${what}?`;
}

/**
 * A cancel names how many people it tells and takes a comment for them,
 * which has the focus; Enter cancels the meeting. A delete says it tells
 * nobody; `y` or Enter deletes and `n` keeps the event. Escape keeps it
 * either way, and a daemon refusal shows in the dialog's alert. Opened again
 * after a failed cancel or delete, it holds the comment and says why.
 */
export function EventRemoveDialog({ dialog, onOpenChange }: EventRemoveDialogProps) {
  const dispatch = useDispatch();
  const commentRef = useRef<HTMLInputElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const [comment, setComment] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const open = dialog !== null;
  const cancel = dialog?.cancel ?? false;

  useEffect(() => {
    setComment(dialog?.comment ?? "");
    setError(dialog?.error ?? null);
    setBusy(false);
  }, [dialog]);

  const latest = useRef({ dialog, comment, busy });
  latest.current = { dialog, comment, busy };
  const confirm = async () => {
    const { dialog: d, comment: c, busy: b } = latest.current;
    if (!d || b) return;
    setBusy(true);
    const refused = await submitRemove(dispatch, d, c);
    setBusy(false);
    setError(refused);
  };

  // On the window, so the keys work before the popup has taken focus; a
  // cancel's comment takes its own typing, Enter included.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const onButton = document.activeElement instanceof HTMLButtonElement;
      if (latest.current.dialog?.cancel) {
        if (e.key === "Enter" && !onButton) {
          e.preventDefault();
          void confirm();
        }
        return;
      }
      if (e.key === "y" || (e.key === "Enter" && !onButton)) {
        e.preventDefault();
        void confirm();
      } else if (e.key === "n") {
        e.preventDefault();
        onOpenChange(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // `confirm` reads the latest state through the ref.
  }, [open, onOpenChange]);

  const people = dialog?.attendees === 1 ? "its attendee" : `its ${dialog?.attendees ?? 0} attendees`;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={cancel ? commentRef : confirmRef} showCloseButton={false} data-slot="event-remove">
        <DialogHeader>
          <DialogTitle>{title(cancel, dialog?.scope ?? null)}</DialogTitle>
          <DialogDescription className="break-words">{dialog?.summary ?? ""}</DialogDescription>
          <p className="text-sm text-muted-foreground">
            {cancel ? `The server sends ${people} the cancellation.` : "The server tells nobody."}
          </p>
        </DialogHeader>
        {cancel ? (
          <label className="flex flex-col gap-1 text-sm">
            <span className="text-muted-foreground">Comment (optional)</span>
            <Input
              ref={commentRef}
              data-slot="cancel-comment"
              aria-label="Comment"
              value={comment}
              onChange={(e) => setComment(e.currentTarget.value)}
              placeholder="Sent with the cancellation"
            />
          </label>
        ) : null}
        <p role="alert" data-slot="event-remove-error" className="min-h-5 text-sm text-destructive">
          {error}
        </p>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>
            Keep it {cancel ? null : <Kbd aria-hidden="true">n</Kbd>}
          </DialogClose>
          <Button ref={confirmRef} variant="destructive" disabled={busy} onClick={() => void confirm()}>
            {cancel ? "Cancel meeting" : "Delete"} <Kbd aria-hidden="true">{cancel ? "↵" : "y"}</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
