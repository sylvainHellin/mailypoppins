// The question the `compose_leave` overlay asks while an embedded editor
// runs (ticket 0130): before a navigation away from it, and before the
// window closes. docs/shell.md, "Compose", has the wording.

import { useRef } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { closeWindow, leaveClosingEditor, runningEditors } from "@/app/compose";
import { useAppState, useDispatch } from "@/app/store";

function names(list: string[]): string {
  if (list.length <= 1) return list[0] ?? "A draft";
  return `${list.slice(0, -1).join(", ")} and ${list[list.length - 1]}`;
}

/**
 * Navigating away: "Keep editing in the background" (the initial focus),
 * "Close the editor" or "Stay". Closing the window: "Close the editor" (the
 * initial focus) or "Stay", since the window is the editor's terminal.
 * Escape stays.
 */
export function ComposeLeaveDialog() {
  const s = useAppState();
  const dispatch = useDispatch();
  const keepRef = useRef<HTMLButtonElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const leave = s.overlay === "compose_leave" ? s.composeLeave : null;
  const closing = leave?.kind === "close";
  const shown = s.composeShown ? s.compose[s.composeShown] : undefined;
  const stay = () => dispatch({ type: "overlay", overlay: null });

  const title = closing ? "Close the window?" : "Leave the editor?";
  const detail = closing
    ? `${names(runningEditors(s).map((c) => c.name))} ${runningEditors(s).length > 1 ? "are" : "is"} still open in the editor. Closing the window closes the editor; the draft keeps what was last saved.`
    : `${shown?.name ?? "The draft"} is still open in the editor. In the background it keeps running and the banner lists it; closed, the draft keeps what was last saved.`;

  return (
    <Dialog open={leave !== null} onOpenChange={(open) => !open && stay()}>
      <DialogContent initialFocus={closing ? closeRef : keepRef} showCloseButton={false} data-slot="compose-leave" className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription className="break-words">{detail}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={stay}>
            Stay
          </Button>
          <Button
            ref={closeRef}
            variant={closing ? "default" : "outline"}
            onClick={() => void (closing ? closeWindow(s, dispatch) : leaveClosingEditor(s, dispatch))}
          >
            Close the editor
          </Button>
          {closing ? null : (
            <Button ref={keepRef} onClick={() => dispatch({ type: "compose_leave_keep" })}>
              Keep editing in the background
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
