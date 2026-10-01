// The compose dialogs: the new-draft wizard (`cn`), the forward wizard
// (`cf` on a stored message) and the recipients edit (`ce`). The TUI's
// wizard fields: a new draft and a forward pick their signature, and a new
// draft takes an inline body, which the daemon writes above the signature;
// a draft created with one does not open the editor, as in the TUI.

import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { RecipientsInput } from "@/components/compose/RecipientsInput";
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
import { useAppState, useDispatch } from "@/app/store";
import type { ComposeDialog } from "@/app/state";
import type { SignatureListing } from "@/lib/gui-types";

/** The signature select's value for "no signature". */
const NONE = "\u0000none";

const TITLE: Record<ComposeDialog["kind"], string> = {
  new: "New draft",
  forward: "Forward",
  recipients: "Edit recipients",
};

const DESCRIPTION: Record<ComposeDialog["kind"], string> = {
  new: "Type a short body here, or leave it empty and write it in your editor.",
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
  if (dialog?.kind === "new") return { to: dialog.to ?? "", cc: "", bcc: "", subject: "", body: "" };
  return { to: "", cc: "", bcc: "", subject: dialog?.kind === "forward" ? dialog.subject : "" };
}

export type ComposeWizardProps = { dialog: ComposeDialog | null; onOpenChange: (open: boolean) => void };

/**
 * Enter in a field moves to the next one (in the body it starts a new line),
 * Cmd+Enter or Ctrl+Enter submits, Escape cancels. A refusal (a name already taken, an unknown row) shows in
 * the dialog, which stays open.
 */
export function ComposeWizard({ dialog, onOpenChange }: ComposeWizardProps) {
  const dispatch = useDispatch();
  const id = useId();
  const [fields, setFields] = useState<ComposeFields>(() => initialFields(dialog));
  const [signature, setSignature] = useState<string>(NONE);
  // Whether the user picked a signature in this dialog: until then the
  // select follows the account's default as the listing changes.
  const picked = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const toRef = useRef<HTMLInputElement>(null);
  const subjectRef = useRef<HTMLInputElement>(null);
  // A draft to a contact comes with its recipient: the subject is next, as in the TUI.
  const seeded = dialog?.kind === "new" && Boolean(dialog.to);
  const open = dialog !== null;
  const kind = dialog?.kind ?? "new";
  const account = dialog?.account ?? null;

  // A new dialog starts from its own fields.
  useEffect(() => {
    setFields(initialFields(dialog));
    setError(null);
    setBusy(false);
    setSignature(NONE);
    picked.current = false;
  }, [dialog]);

  // A new draft and a forward pick a signature, from the account's listing in the
  // store, which the open wizard reads again on every `signature.changed`
  // and every change the Signatures dialog makes. A listing that failed
  // offers none.
  const loadable = useAppState().signatures;
  const signs = kind === "new" || kind === "forward";
  const entry = signs && account ? loadable[account] : undefined;
  const data = entry?.data ?? null;
  const failed = entry?.error != null;
  const signatures = useMemo<SignatureListing | null>(
    () => (data ?? (failed && account ? { account, names: [], default: null } : null)),
    [data, failed, account],
  );

  // The default is preselected; a pick survives a new listing while its
  // name is still listed, and falls back to the default when it is not.
  useEffect(() => {
    if (!signatures) return;
    const fallback = signatures.default && signatures.names.includes(signatures.default) ? signatures.default : NONE;
    setSignature((current) => {
      if (!picked.current) return fallback;
      return current === NONE || signatures.names.includes(current) ? current : fallback;
    });
  }, [signatures]);

  const submit = async () => {
    if (!dialog || busy) return;
    setBusy(true);
    // An untouched select sends neither field, so the daemon resolves the
    // account default and honours `include_signature = false`, as the TUI's
    // wizard does; an explicit pick, the default included, names it.
    const withSignature: ComposeFields =
      signs && signatures && signatures.names.length > 0 && picked.current
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

  // To, Cc and Bcc complete contacts; the subject is a plain field.
  const field = (name: keyof Omit<ComposeFields, "signature" | "body">, label: string) => (
    <div className="grid grid-cols-[4.5rem_1fr] items-center gap-2">
      <label htmlFor={`${id}-${name}`} className="text-sm text-muted-foreground">
        {label}
      </label>
      {name === "subject" ? (
        <Input
          id={`${id}-${name}`}
          ref={subjectRef}
          data-field={name}
          value={fields[name] ?? ""}
          autoComplete="off"
          spellCheck
          onChange={(e) => setFields({ ...fields, [name]: e.currentTarget.value })}
        />
      ) : (
        <RecipientsInput
          id={`${id}-${name}`}
          ref={name === "to" ? toRef : undefined}
          account={account}
          data-field={name}
          value={fields[name] ?? ""}
          autoComplete="off"
          spellCheck={false}
          aria-invalid={error !== null ? true : undefined}
          onValueChange={(v) => setFields((f) => ({ ...f, [name]: v }))}
        />
      )}
    </div>
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={seeded ? subjectRef : toRef} data-slot="compose-wizard" data-kind={kind} className="sm:max-w-lg">
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
          {signs && signatures && signatures.names.length > 0 ? (
            <div className="grid grid-cols-[4.5rem_1fr] items-center gap-2">
              <label htmlFor={`${id}-signature`} className="text-sm text-muted-foreground">
                Signature
              </label>
              <select
                id={`${id}-signature`}
                data-field="signature"
                value={signature}
                onChange={(e) => {
                  picked.current = true;
                  setSignature(e.currentTarget.value);
                }}
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
          {kind === "new" ? (
            <div className="grid grid-cols-[4.5rem_1fr] items-start gap-2">
              <label htmlFor={`${id}-body`} className="pt-1.5 text-sm text-muted-foreground">
                Body
              </label>
              <Textarea
                id={`${id}-body`}
                data-field="body"
                value={fields.body ?? ""}
                rows={5}
                spellCheck
                placeholder="Leave empty to write the body in your editor"
                onChange={(e) => setFields({ ...fields, body: e.currentTarget.value })}
              />
            </div>
          ) : null}
          <p role="alert" data-slot="compose-error" className="min-h-5 text-sm text-destructive">
            {error}
          </p>
          <DialogFooter>
            <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" data-submit="" disabled={busy}>
              {kind === "new" && fields.body?.trim() ? "Create" : SUBMIT[kind]} <Kbd aria-hidden="true">⌘↵</Kbd>
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
