// The password dialog (ACC-05): one SMTP or IMAP password of one account,
// stored through the daemon's secrets backend. The value lives in this
// component's state only, never in the model, and is cleared on submit and
// on close, whatever the answer.

import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { kindLabel, storePassword } from "@/app/settings";
import { useAppState, useDispatch } from "@/app/store";
import type { PasswordDialog as PasswordDialogModel } from "@/app/state";

export type PasswordDialogProps = { dialog: PasswordDialogModel | null; onOpenChange: (open: boolean) => void };

/**
 * Enter stores, Escape cancels. A refusal shows in the dialog's alert and
 * the field starts empty again, so a retry is typed afresh.
 */
export function PasswordDialog({ dialog, onOpenChange }: PasswordDialogProps) {
  const s = useAppState();
  const dispatch = useDispatch();
  const id = useId();
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fieldRef = useRef<HTMLInputElement>(null);
  const open = dialog !== null;
  const account = dialog?.account ?? "";
  const kind = dialog?.kind ?? "smtp";
  const label = kindLabel(kind);
  const configured = s.config.data?.config.accounts.find((a) => a.name === account);
  const username = configured ? configured[kind].username : "";
  const backend = s.config.data?.config.secrets_backend || "configured";

  // A new target, or none: nothing typed for another survives.
  useEffect(() => {
    setValue("");
    setError(null);
    setBusy(false);
  }, [dialog]);

  const submit = async () => {
    if (!dialog || busy || value === "") return;
    const typed = value;
    setValue("");
    setBusy(true);
    const refused = await storePassword(dispatch, dialog.account, dialog.kind, typed);
    setBusy(false);
    setError(refused);
  };

  // After a refusal the field, enabled again, takes the retry.
  useEffect(() => {
    if (error && !busy) fieldRef.current?.focus();
  }, [error, busy]);

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setValue("");
        onOpenChange(next);
      }}
    >
      <DialogContent initialFocus={fieldRef} data-slot="password-dialog">
        <DialogHeader>
          <DialogTitle>{`Set ${label} password`}</DialogTitle>
          <DialogDescription>
            {`For ${account}${username ? ` (${username})` : ""}; stored in the ${backend} secrets backend, never shown again.`}
          </DialogDescription>
        </DialogHeader>
        <form
          aria-label={`${label} password for ${account}`}
          className="flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <label htmlFor={`${id}-value`} className="text-sm text-muted-foreground">
            Password
          </label>
          <Input
            id={`${id}-value`}
            ref={fieldRef}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={value}
            disabled={busy}
            onChange={(e) => setValue(e.currentTarget.value)}
          />
          <p role="alert" data-slot="password-error" className="min-h-5 text-sm text-destructive">
            {error}
          </p>
          <DialogFooter>
            <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" disabled={busy || value === ""}>
              Store password
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
