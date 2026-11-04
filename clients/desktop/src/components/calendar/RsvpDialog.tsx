import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
import { PROPOSAL_NEEDS_RESPONSE, RESPONSE_LABEL, sendRsvp } from "@/app/rsvp";
import { localZone } from "@/app/eventForm";
import { useDispatch } from "@/app/store";
import { RSVP_RESPONSES, type RsvpDialog as RsvpDialogState, type RsvpResponse } from "@/app/state";

const KEY: Record<string, number> = { a: 0, t: 1, d: 2 };

export type RsvpDialogProps = { dialog: RsvpDialogState | null; onOpenChange: (open: boolean) => void };

/**
 * The respond choice (`tv` in Mail, `V` in the Calendar view), the TUI's
 * respond overlay (PERSO-200): `a`, `t` and `d` pick Accept, Tentative and
 * Decline, `j`, Down or Tab move down, `k`, Up or Shift+Tab move up, Enter
 * sends the choice, Escape or `q` closes. Tab from Decline goes on to the
 * comment, sent with the answer, on a tentative or a decline of a server
 * event whose organizer accepts proposals to "Propose a new time" (a start,
 * and an end that defaults to the event's length; PERSO-255), and on a
 * server event to "Send a response to the organizer", which an invitation
 * mail's answer, a reply mail, does not offer. A proposal with the response
 * off is refused in the dialog, the TUI's sentence. The dialog owns every
 * key while it is open, the fields' own typing aside.
 */
export function RsvpDialog({ dialog, onOpenChange }: RsvpDialogProps) {
  const dispatch = useDispatch();
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState(0);
  const [comment, setComment] = useState("");
  const [sendResponse, setSendResponse] = useState(true);
  const [proposedStart, setProposedStart] = useState("");
  const [proposedEnd, setProposedEnd] = useState("");
  const open = dialog !== null;

  useEffect(() => {
    setSelected(0);
    setComment("");
    setSendResponse(true);
    setProposedStart("");
    setProposedEnd("");
  }, [dialog]);

  const latest = useRef({ dialog, selected, comment, sendResponse, proposedStart, proposedEnd });
  latest.current = { dialog, selected, comment, sendResponse, proposedStart, proposedEnd };

  const proposing = !!dialog?.propose && RSVP_RESPONSES[selected] !== "accept";
  const problem = proposing && proposedStart.trim() && !sendResponse ? PROPOSAL_NEEDS_RESPONSE : null;

  /** The answer `response` with what the fields hold, unless the proposal would tell nobody. */
  const sendNow = (response: RsvpResponse) => {
    const { dialog: d, comment: c, sendResponse: tell, proposedStart: from, proposedEnd: to } = latest.current;
    if (!d) return;
    const start = d.propose && response !== "accept" ? from.trim() : "";
    if (start && !tell) return;
    const proposedTime = start ? { start, end: to.trim() || undefined, zone: localZone() } : null;
    void sendRsvp(dispatch, d, response, { comment: c, sendResponse: d.server ? tell : null, proposedTime });
  };
  const sendRef = useRef(sendNow);
  sendRef.current = sendNow;

  // On the window, so the keys work before the popup has taken focus, and in
  // the capture phase, since the popup stops the arrow keys on their way up.
  useEffect(() => {
    if (!open) return;
    const send = () => sendRef.current(RSVP_RESPONSES[latest.current.selected]);
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      // The comment and the toggle take their own keys; Enter still sends,
      // and Tab moves on from them as a field's Tab does.
      if (e.target instanceof Element && e.target.closest("[data-rsvp-field]")) {
        if (e.key === "Enter") {
          e.preventDefault();
          send();
        }
        return;
      }
      const move = (by: number) => setSelected((i) => Math.max(0, Math.min(RSVP_RESPONSES.length - 1, i + by)));
      // Tab from the last answer leaves the list for the comment.
      if (e.key === "Tab" && !e.shiftKey && latest.current.selected === RSVP_RESPONSES.length - 1) return;
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
              onDoubleClick={() => sendNow(response)}
              className="flex cursor-default items-center justify-between rounded-md px-3 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
            >
              {RESPONSE_LABEL[response]}
              <Kbd aria-hidden="true">{response[0]}</Kbd>
            </div>
          ))}
        </div>
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-muted-foreground">Comment (optional)</span>
          <Input
            data-rsvp-field
            data-slot="rsvp-comment"
            aria-label="Comment"
            value={comment}
            onChange={(e) => setComment(e.currentTarget.value)}
            placeholder="Sent with the answer"
          />
        </label>
        {dialog?.server ? (
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              data-rsvp-field
              data-slot="rsvp-send-response"
              checked={sendResponse}
              onChange={(e) => setSendResponse(e.currentTarget.checked)}
            />
            Send a response to the organizer
          </label>
        ) : (
          <p className="text-xs text-muted-foreground">The answer goes to the organizer as a reply mail.</p>
        )}
        {proposing ? (
          <fieldset data-slot="rsvp-propose" className="flex flex-col gap-1 text-sm">
            <legend className="text-muted-foreground">Propose a new time (optional)</legend>
            <div className="flex gap-2">
              <label className="flex flex-1 flex-col gap-1">
                <span className="text-xs text-muted-foreground">Start</span>
                <Input
                  data-rsvp-field
                  type="datetime-local"
                  aria-label="Proposed start"
                  value={proposedStart}
                  onChange={(e) => setProposedStart(e.currentTarget.value)}
                />
              </label>
              <label className="flex flex-1 flex-col gap-1">
                <span className="text-xs text-muted-foreground">End (default: the event's length)</span>
                <Input
                  data-rsvp-field
                  type="datetime-local"
                  aria-label="Proposed end"
                  value={proposedEnd}
                  onChange={(e) => setProposedEnd(e.currentTarget.value)}
                />
              </label>
            </div>
          </fieldset>
        ) : null}
        {problem ? (
          <p role="alert" data-slot="rsvp-problem" className="text-xs text-destructive">
            {problem}
          </p>
        ) : null}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>
            Cancel <Kbd aria-hidden="true">q</Kbd>
          </DialogClose>
          <Button disabled={problem !== null} onClick={() => sendNow(RSVP_RESPONSES[selected])}>
            Send {RESPONSE_LABEL[RSVP_RESPONSES[selected]]} <Kbd aria-hidden="true">↵</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
