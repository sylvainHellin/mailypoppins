import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export type RestartDaemonDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
};

/** No client restarts the daemon without an explicit confirmation. */
export function RestartDaemonDialog({ open, onOpenChange, onConfirm }: RestartDaemonDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Restart the daemon?</DialogTitle>
          <DialogDescription>
            This runs <code>mp daemon restart</code>. Every connected client, the TUI and the CLI included, reconnects,
            and a running server search is dropped.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
          <Button
            onClick={() => {
              onConfirm();
              onOpenChange(false);
            }}
          >
            Restart daemon
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
