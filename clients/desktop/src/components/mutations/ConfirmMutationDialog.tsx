import { useEffect, useRef } from "react";
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
import type { MutationDialog } from "@/app/state";

export type ConfirmMutationDialogProps = {
  dialog: Exclude<MutationDialog, { kind: "move" }> | null;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
};

const VERB = { archive: "Archive", delete: "Delete", approve: "Approve", demote: "Mark as draft" } as const;

/**
 * The TUI's confirmation before an archive, a delete, or an approve or
 * demote over marked drafts: `y` or Enter runs it, `n` or Escape cancels.
 * The confirm button has the initial focus.
 */
export function ConfirmMutationDialog({ dialog, onOpenChange, onConfirm }: ConfirmMutationDialogProps) {
  const confirmRef = useRef<HTMLButtonElement>(null);
  const verb = VERB[dialog?.kind ?? "archive"];
  const open = dialog !== null;
  // On the window, so the keys work before the popup has taken focus.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      // Enter confirms as the TUI's does, unless a focused button takes it.
      const onButton = document.activeElement instanceof HTMLButtonElement;
      if (e.key === "y" || (e.key === "Enter" && !onButton)) {
        e.preventDefault();
        onConfirm();
      } else if (e.key === "n") {
        e.preventDefault();
        onOpenChange(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onConfirm, onOpenChange]);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        initialFocus={confirmRef}
        showCloseButton={false}
        data-slot="confirm-mutation"
      >
        <DialogHeader>
          <DialogTitle>{dialog?.title ?? ""}</DialogTitle>
          <DialogDescription className="break-words">{dialog?.detail ?? ""}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>
            Cancel <Kbd aria-hidden="true">n</Kbd>
          </DialogClose>
          <Button ref={confirmRef} variant={dialog?.kind === "delete" ? "destructive" : "default"} onClick={onConfirm}>
            {verb} <Kbd aria-hidden="true">y</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
