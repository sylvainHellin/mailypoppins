// "This occurrence" or "The whole series" (CAL-12, CAL-13, PERSO-255; the
// TUI's scope chooser, the CLI's `--scope`): what Edit, `D`, the card's Edit
// and its Cancel meeting or Delete ask first on an occurrence of a series,
// as Outlook does, with this occurrence the default.

import { useEffect, useRef } from "react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { chooseEventScope, type EventScope } from "@/app/eventChange";
import { eventTitle } from "@/app/calendar";
import { useAppState, useDispatch } from "@/app/store";
import type { EventScopeDialog as EventScopeState } from "@/app/state";

export type EventScopeDialogProps = { dialog: EventScopeState | null; onOpenChange: (open: boolean) => void };

/** Two buttons, `o` and `s` their keys; Enter takes the focused one, this occurrence when it opens, and Escape keeps the event as it is. */
export function EventScopeDialog({ dialog, onOpenChange }: EventScopeDialogProps) {
  const dispatch = useDispatch();
  const state = useAppState();
  const occurrenceRef = useRef<HTMLButtonElement>(null);
  const open = dialog !== null;
  const latest = useRef(state);
  latest.current = state;
  const choose = (scope: EventScope) => void chooseEventScope(latest.current, dispatch, scope);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      if (e.key === "o" || e.key === "s") {
        e.preventDefault();
        choose(e.key === "o" ? "occurrence" : "series");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // `choose` reads the latest state through the ref.
  }, [open]);

  const remove = dialog?.remove ?? false;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={occurrenceRef} showCloseButton={false} data-slot="event-scope">
        <DialogHeader>
          <DialogTitle>{remove ? "Cancel or delete a recurring event" : "Edit a recurring event"}</DialogTitle>
          <DialogDescription className="break-words">
            {dialog ? `${eventTitle(dialog.event)}${dialog.event.recurrence ? `, ${dialog.event.recurrence}` : ""}` : ""}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>Keep it</DialogClose>
          <Button ref={occurrenceRef} variant="outline" data-scope="occurrence" onClick={() => choose("occurrence")}>
            This occurrence <Kbd aria-hidden="true">o</Kbd>
          </Button>
          <Button data-scope="series" onClick={() => choose("series")}>
            The whole series <Kbd aria-hidden="true">s</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
