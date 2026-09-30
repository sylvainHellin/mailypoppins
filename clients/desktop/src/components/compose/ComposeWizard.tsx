// The compose dialogs: the new-draft wizard (`cn`), the forward wizard
// (`cf` on a stored message) and the recipients edit (`ce`). The TUI's
// wizard fields, less its inline body: `draft_create` takes no body, so the
// body is written in the editor the draft opens in.

import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
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
import { submitCompose, type ComposeFields } from "@/app/compose";
import { useDispatch } from "@/app/store";
import type { ComposeDialog } from "@/app/state";
import * as cmd from "@/lib/commands";
import type { SignatureListing } from "@/lib/gui-types";

/** The signature select's value for "no signature". */
const NONE = "\u0000none";

const TITLE: Record<ComposeDialog["kind"], string> = {
  new: "New draft",
  forward: "Forward",
  recipients: "Edit recipients",
};

const DESCRIPTION: Record<ComposeDialog["kind"], string> = {
  new: "The draft opens in your editor for the body.",
  forward: "The forward keeps these recipients and this subject, then opens in your editor.",
  recipients: "Rewrites the recipient and subject lines of the draft file; the body stays as it is.",
};

const SUBMIT: Record<ComposeDialog["kind"], string> = {
  new: "Create and edit",
  forward: "Forward and edit",
  recipients: "Save recipients",
};

function initialFields(dialog: ComposeDialog | null): ComposeFields {
  if (dialog?.kind === "recipients") return { to: dialog.to, cc: dialog.cc, bcc: dialog.bcc, subject: dialog.subject };
  return { to: "", cc: "", bcc: "", subject: dialog?.kind === "forward" ? dialog.subject : "" };
}

export type ComposeWizardProps = { dialog: ComposeDialog | null; onOpenChange: (open: boolean) => void };

/**
 * Enter in a field moves to the next one, Cmd+Enter or Ctrl+Enter submits,
 * Escape cancels. A refusal (a name already taken, an unknown row) shows in
 * the dialog, which stays open.
 */
export function ComposeWizard({ dialog, onOpenChange }: ComposeWizardProps) {
  const dispatch = useDispatch();
  const id = useId();
  const [fields, setFields] = useState<ComposeFields>(() => initialFields(dialog));
  const [signatures, setSignatures] = useState<SignatureListing | null>(null);
  const [signature, setSignature] = useState<string>(NONE);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const toRef = useRef<HTMLInputElement>(null);
  const open = dialog !== null;
  const kind = dialog?.kind ?? "new";
  const account = dialog?.account ?? null;

  // A new dialog starts from its own fields.
  useEffect(() => {
    setFields(initialFields(dialog));
    setError(null);
    setBusy(false);
  }, [dialog]);

  // Only a new draft picks a signature; the account's default is preselected.
  useEffect(() => {
    setSignatures(null);
    if (kind !== "new" || !account) return;
    let live = true;
    cmd
      .signatureList(account)
      .then((listing) => {
        if (!live) return;
        setSignatures(listing);
        setSignature(listing.default && listing.names.includes(listing.default) ? listing.default : NONE);
      })
      .catch(() => live && setSignatures({ account, names: [], default: null }));
    return () => {
      live = false;
    };
  }, [kind, account]);

  const submit = async () => {
    if (!dialog || busy) return;
    setBusy(true);
    const withSignature: ComposeFields =
      kind === "new" && signatures && signatures.names.length > 0
        ? { ...fields, signature: signature === NONE ? null : signature }
        : fields;
    const refusal = await submitCompose(dialog, withSignature, dispatch);
    setBusy(false);
    setError(refusal);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLFormElement>) => {
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    if (e.metaKey || e.ctrlKey) {
      e.preventDefault();
      void submit();
      return;
    }
    const target = e.target as HTMLElement;
    if (!(target instanceof HTMLInputElement || target instanceof HTMLSelectElement)) return;
    // Enter moves on, to the submit button after the last field.
    e.preventDefault();
    const order = [...(formRef.current?.querySelectorAll<HTMLElement>("[data-field], [data-submit]") ?? [])];
    order[order.indexOf(target) + 1]?.focus();
  };

  const field = (name: keyof Omit<ComposeFields, "signature">, label: string) => (
    <div className="grid grid-cols-[4.5rem_1fr] items-center gap-2">
      <label htmlFor={`${id}-${name}`} className="text-sm text-muted-foreground">
        {label}
      </label>
      <Input
        id={`${id}-${name}`}
        ref={name === "to" ? toRef : undefined}
        data-field={name}
        value={fields[name] ?? ""}
        autoComplete="off"
        spellCheck={name === "subject"}
        aria-invalid={error !== null && name !== "subject" ? true : undefined}
        onChange={(e) => setFields({ ...fields, [name]: e.currentTarget.value })}
      />
    </div>
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={toRef} data-slot="compose-wizard" data-kind={kind} className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{TITLE[kind]}</DialogTitle>
          <DialogDescription>{DESCRIPTION[kind]}</DialogDescription>
        </DialogHeader>
        <form
          ref={formRef}
          className="flex flex-col gap-2"
          onKeyDown={onKeyDown}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {field("to", "To")}
          {field("cc", "Cc")}
          {field("bcc", "Bcc")}
          {field("subject", "Subject")}
          {kind === "new" && signatures && signatures.names.length > 0 ? (
            <div className="grid grid-cols-[4.5rem_1fr] items-center gap-2">
              <label htmlFor={`${id}-signature`} className="text-sm text-muted-foreground">
                Signature
              </label>
              <select
                id={`${id}-signature`}
                data-field="signature"
                value={signature}
                onChange={(e) => setSignature(e.currentTarget.value)}
                className="h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
              >
                {signatures.names.map((n) => (
                  <option key={n} value={n}>
                    {n === signatures.default ? `${n} (default)` : n}
                  </option>
                ))}
                <option value={NONE}>none</option>
              </select>
            </div>
          ) : null}
          <p role="alert" data-slot="compose-error" className="min-h-5 text-sm text-destructive">
            {error}
          </p>
          <DialogFooter>
            <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" data-submit="" disabled={busy}>
              {SUBMIT[kind]} <Kbd aria-hidden="true">⌘↵</Kbd>
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
