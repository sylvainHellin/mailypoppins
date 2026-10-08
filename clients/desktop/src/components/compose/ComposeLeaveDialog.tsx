// The question the `compose_leave` overlay asks while an embedded editor
// runs (ticket 0130): before a navigation away from it, before the window
// closes, and before a restart into an installed update (ticket 0139).
// docs/shell.md, "Compose", has the wording.

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
import { restartNow } from "@/app/updates";
import { useAppState, useDispatch } from "@/app/store";

function names(list: string[]): string {
  if (list.length <= 1) return list[0] ?? "A draft";
  return `${list.slice(0, -1).join(", ")} and ${list[list.length - 1]}`;
}

/**
 * Navigating away: "Keep editing in the background" (the initial focus),
 * "Close the editor" or "Stay". Closing the window: "Close the editor" (the
 * initial focus) or "Stay", since the window is the editor's terminal.
 * Restarting into an update: "Close the editor and restart" (the initial
 * focus) or "Stay". Escape stays.
 */
export function ComposeLeaveDialog() {
  const s = useAppState();
  const dispatch = useDispatch();
  const keepRef = useRef<HTMLButtonElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const leave = s.overlay === "compose_leave" ? s.composeLeave : null;
  const restarting = leave?.kind === "restart";
  // The window close and the restart both end every editor, so neither offers the background.
  const closing = leave?.kind === "close" || restarting;
  const shown = s.composeShown ? s.compose[s.composeShown] : undefined;
  const stay = () => dispatch({ type: "overlay", overlay: null });
  const open = runningEditors(s);
  const stillOpen = `${names(open.map((c) => c.name))} ${open.length > 1 ? "are" : "is"} still open in the editor.`;

  const title = restarting ? "Restart to finish the update?" : closing ? "Close the window?" : "Leave the editor?";
  const detail = restarting
    ? `${stillOpen} Restarting closes the editor; the draft keeps what was last saved.`
    : closing
      ? `${stillOpen} Closing the window closes the editor; the draft keeps what was last saved.`
      : `${shown?.name ?? "The draft"} is still open in the editor. In the background it keeps running and the banner lists it; closed, the draft keeps what was last saved.`;
  const confirm = () => {
    if (restarting) return void restartNow(s, dispatch);
    return void (closing ? closeWindow(s, dispatch) : leaveClosingEditor(s, dispatch));
  };

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
            onClick={confirm}
          >
            {restarting ? "Close the editor and restart" : "Close the editor"}
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
