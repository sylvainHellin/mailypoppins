import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { RESPONSE_LABEL, sendRsvp } from "@/app/rsvp";
import { useDispatch } from "@/app/store";
import { RSVP_RESPONSES, type RsvpDialog as RsvpDialogState } from "@/app/state";

const KEY: Record<string, number> = { a: 0, t: 1, d: 2 };

export type RsvpDialogProps = { dialog: RsvpDialogState | null; onOpenChange: (open: boolean) => void };

/**
 * The RSVP choice (`tv` in Mail, `V` in the Calendar view), the TUI's RSVP
 * overlay: `a`, `t` and `d` pick Accept, Tentative and Decline, `j`, Down or
 * Tab move down, `k`, Up or Shift+Tab move up, Enter sends the choice,
 * Escape or `q` closes. The dialog owns every key while it is open.
 */
export function RsvpDialog({ dialog, onOpenChange }: RsvpDialogProps) {
  const dispatch = useDispatch();
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState(0);
  const open = dialog !== null;

  useEffect(() => setSelected(0), [dialog]);

  const latest = useRef({ dialog, selected });
  latest.current = { dialog, selected };

  // On the window, so the keys work before the popup has taken focus, and in
  // the capture phase, since the popup stops the arrow keys on their way up.
  useEffect(() => {
    if (!open) return;
    const send = () => {
      const { dialog: d, selected: i } = latest.current;
      if (d) void sendRsvp(dispatch, d, RSVP_RESPONSES[i]);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const move = (by: number) => setSelected((i) => Math.max(0, Math.min(RSVP_RESPONSES.length - 1, i + by)));
      if (e.key === "j" || e.key === "ArrowDown" || (e.key === "Tab" && !e.shiftKey)) {
        e.preventDefault();
        move(1);
      } else if (e.key === "k" || e.key === "ArrowUp" || (e.key === "Tab" && e.shiftKey)) {
        e.preventDefault();
        move(-1);
      } else if (e.key in KEY) {
        e.preventDefault();
        setSelected(KEY[e.key]);
      } else if (e.key === "q") {
        e.preventDefault();
        onOpenChange(false);
      } else if (e.key === "Enter") {
        // Enter sends, unless a button of the dialog (Cancel, Send) has the
        // focus and takes it; a button behind the dialog, focused before the
        // popup took the focus, does not.
        const active = document.activeElement;
        const popup = listRef.current?.closest('[role="dialog"]');
        if (active instanceof HTMLButtonElement && popup?.contains(active)) return;
        e.preventDefault();
        send();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, dispatch, onOpenChange]);

  const optionId = (i: number) => `${id}-option-${i}`;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={listRef} data-slot="rsvp-dialog" className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>RSVP</DialogTitle>
          <DialogDescription className="break-words">{dialog?.summary ?? ""}</DialogDescription>
        </DialogHeader>
        <div
          ref={listRef}
          role="listbox"
          aria-label="Response"
          tabIndex={0}
          aria-activedescendant={optionId(selected)}
          className="flex flex-col gap-1 rounded-lg outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {RSVP_RESPONSES.map((response, i) => (
            <div
              key={response}
              id={optionId(i)}
              role="option"
              aria-selected={i === selected}
              data-response={response}
              onClick={() => setSelected(i)}
              onDoubleClick={() => dialog && void sendRsvp(dispatch, dialog, response)}
              className="flex cursor-default items-center justify-between rounded-md px-3 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
            >
              {RESPONSE_LABEL[response]}
              <Kbd aria-hidden="true">{response[0]}</Kbd>
            </div>
          ))}
        </div>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>
            Cancel <Kbd aria-hidden="true">q</Kbd>
          </DialogClose>
          <Button onClick={() => dialog && void sendRsvp(dispatch, dialog, RSVP_RESPONSES[selected])}>
            Send {RESPONSE_LABEL[RSVP_RESPONSES[selected]]} <Kbd aria-hidden="true">↵</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
