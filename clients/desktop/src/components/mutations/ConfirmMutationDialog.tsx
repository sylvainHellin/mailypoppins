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

/**
 * What the confirmation asks about: a mutation of the `mutation` overlay, or
 * the Signatures dialog's delete, which it raises over itself.
 */
export type Confirmation = Exclude<MutationDialog, { kind: "move" }> | { kind: "signature_delete"; title: string; detail: string };

export type ConfirmMutationDialogProps = {
  dialog: Confirmation | null;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
};

const VERB = {
  archive: "Archive",
  delete: "Delete",
  approve: "Approve",
  demote: "Mark as draft",
  send: "Send",
  send_approved: "Send",
  outbox_retry: "Retry",
  outbox_discard: "Discard",
  signature_delete: "Delete",
} as const;

/**
 * The TUI's confirmation before an archive, a delete, an approve or
 * demote over marked drafts, a send, an outbox row's retry or discard, which
 * also says what the row may already have done, or a signature's delete: `y`
 * or Enter runs it, `n` or Escape cancels.
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
          {dialog && "warning" in dialog && dialog.warning ? (
            <p data-slot="confirm-warning" className="text-sm text-warning">
              {dialog.warning}
            </p>
          ) : null}
        </DialogHeader>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>
            Cancel <Kbd aria-hidden="true">n</Kbd>
          </DialogClose>
          <Button ref={confirmRef} variant={dialog?.kind === "delete" || dialog?.kind === "outbox_discard" || dialog?.kind === "signature_delete" ? "destructive" : "default"} onClick={onConfirm}>
            {verb} <Kbd aria-hidden="true">y</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
