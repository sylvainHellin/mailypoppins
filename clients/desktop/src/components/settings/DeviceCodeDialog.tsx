// The device-code sign-in dialog (ACC-06, INT-04): the verification URL
// and the user code the daemon reports as the sign-in's one progress, a Copy
// button for the code and "Open verification page" through `open_external`;
// then how it ended. While it runs, Escape and Cancel cancel it (the
// provider's poll runs on, which the dialog says); once it ended they close.

import { useEffect, useId, useRef, type RefObject } from "react";
import { Copy, ExternalLink } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { openInBrowser } from "@/components/reader/links";
import { cancelSignIn, deviceCodeOf, type DeviceCode } from "@/app/signin";
import { useAppState, useDispatch } from "@/app/store";
import { copyText } from "@/lib/clipboard";

/**
 * The verification page and the code of a running sign-in, with Copy and
 * Open: the dialog's body, and the account wizard's Connection step's.
 */
export function DeviceCodeView({ code, cancelling, copyRef }: { code: DeviceCode; cancelling: boolean; copyRef?: RefObject<HTMLButtonElement | null> }) {
  const dispatch = useDispatch();
  const id = useId();
  return (
    <div className="flex flex-col gap-3" data-slot="device-code">
      <div className="flex flex-col gap-1">
        <span className="text-sm text-muted-foreground" id={`${id}-url`}>
          Verification page
        </span>
        <span aria-labelledby={`${id}-url`} className="font-mono text-xs break-all">
          {code.url}
        </span>
      </div>
      <div className="flex flex-col gap-1">
        <span className="text-sm text-muted-foreground" id={`${id}-code`}>
          Code
        </span>
        <output aria-labelledby={`${id}-code`} data-slot="device-code-value" className="font-mono text-2xl tracking-widest">
          {code.code}
        </output>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button ref={copyRef} type="button" variant="outline" size="sm" onClick={() => void copyText(code.code, "the sign-in code", dispatch)}>
          <Copy aria-hidden="true" />
          Copy code
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={() => openInBrowser(code.url, dispatch)}>
          <ExternalLink aria-hidden="true" />
          Open verification page
        </Button>
      </div>
      <p role="status" className="text-xs text-muted-foreground">
        {cancelling ? "Cancelling…" : "Waiting for you to approve the sign-in in the browser…"}
      </p>
    </div>
  );
}

export function DeviceCodeDialog({ open }: { open: boolean }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const run = s.signIn;
  const code = deviceCodeOf(s);
  const running = run !== null && run.outcome === null;
  const copyRef = useRef<HTMLButtonElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  // Cancel asked before the start answered: sent once the id is known.
  const operationId = run?.operation_id ?? null;
  const cancelling = run?.cancelling ?? false;
  const sentFor = useRef<string | null>(null);
  useEffect(() => {
    if (!cancelling || !operationId || sentFor.current === operationId) return;
    sentFor.current = operationId;
    void cancelSignIn(dispatch, operationId);
  }, [cancelling, operationId, dispatch]);

  // The code arriving, and the end, move the focus to what is next.
  const hasCode = code !== null;
  useEffect(() => {
    if (open && hasCode && running) copyRef.current?.focus();
  }, [open, hasCode, running]);
  const over = run?.outcome != null;
  useEffect(() => {
    if (open && over) closeRef.current?.focus();
  }, [open, over]);

  const cancel = () => {
    if (!run || !running || run.cancelling) return;
    if (run.operation_id) sentFor.current = run.operation_id;
    void cancelSignIn(dispatch, run.operation_id);
  };
  const close = () => dispatch({ type: "sign_in_closed" });

  return (
    <Dialog
      open={open && run !== null}
      onOpenChange={(next) => {
        if (next) return;
        if (running) cancel();
        else close();
      }}
    >
      <DialogContent initialFocus={cancelRef} data-slot="device-code-dialog" showCloseButton={false} className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{`Sign in ${run?.account ?? ""}`}</DialogTitle>
          <DialogDescription>
            Microsoft signs this account in with a device code: open the verification page, enter the code, and approve. The token is cached by
            the daemon, never shown here.
          </DialogDescription>
        </DialogHeader>
        {run?.outcome ? (
          <p
            role={run.outcome.kind === "failed" ? "alert" : "status"}
            data-slot="sign-in-outcome"
            data-outcome={run.outcome.kind}
            className={run.outcome.kind === "failed" ? "text-sm text-destructive" : "text-sm"}
          >
            {run.outcome.text}
          </p>
        ) : code ? (
          <DeviceCodeView code={code} cancelling={run?.cancelling ?? false} copyRef={copyRef} />
        ) : (
          <p role="status" className="text-sm text-muted-foreground">
            {run?.cancelling ? "Cancelling…" : "Asking the provider for a device code…"}
          </p>
        )}
        <DialogFooter>
          {running ? (
            <Button ref={cancelRef} type="button" variant="outline" onClick={cancel} disabled={run?.cancelling}>
              Cancel sign-in
            </Button>
          ) : (
            <Button ref={closeRef} type="button" onClick={close}>
              Close
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
