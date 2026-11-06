// The Block sender chooser (`B`, PERSO-261), the TUI's: the sender's address
// or everyone at its domain, what a block does, and the button that names
// what it blocks. Nothing is blocked until that button or Enter.

import { useEffect, useRef, useState } from "react";
import { Ban } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { blockSender } from "@/app/senders";
import type { BlockDialog } from "@/app/state";
import { useDispatch } from "@/app/store";

export type BlockSenderDialogProps = { dialog: BlockDialog | null; onOpenChange: (open: boolean) => void };

type Choice = "address" | "domain";

const HINTS: Record<Choice, string> = { address: "this address", domain: "everyone at this domain" };

/**
 * Two radios, the address first and picked on open: `j`/Down and `k`/Up move,
 * `a` and `d` pick each, Enter blocks the picked one, Escape cancels. The
 * dialog owns every key while it is open.
 */
export function BlockSenderDialog({ dialog, onOpenChange }: BlockSenderDialogProps) {
  const dispatch = useDispatch();
  const groupRef = useRef<HTMLDivElement>(null);
  const open = dialog !== null;
  const [choice, setChoice] = useState<Choice>("address");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // A new dialog starts on the address.
  useEffect(() => {
    setChoice("address");
    setBusy(false);
    setError(null);
  }, [dialog]);

  const entry = dialog ? dialog[choice] : "";
  const latest = useRef({ entry, busy });
  latest.current = { entry, busy };

  const block = async () => {
    const { entry: picked, busy: b } = latest.current;
    if (!picked || b) return;
    setBusy(true);
    const out = await blockSender(dispatch, picked);
    setBusy(false);
    if (out.ok) onOpenChange(false);
    else setError(out.error);
  };
  const blockRef = useRef(block);
  blockRef.current = block;

  // On the window, in the capture phase, as the Signatures dialog reads its keys.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      switch (e.key) {
        case "j":
        case "ArrowDown":
        case "d":
          e.preventDefault();
          return setChoice("domain");
        case "k":
        case "ArrowUp":
        case "a":
          e.preventDefault();
          return setChoice("address");
        case "Enter": {
          // A button of the dialog that has the focus takes its own Enter.
          const active = document.activeElement;
          const popup = groupRef.current?.closest('[role="dialog"]');
          if (active instanceof HTMLButtonElement && popup?.contains(active)) return;
          e.preventDefault();
          return void blockRef.current();
        }
        default:
          return;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={groupRef} data-slot="block-sender" className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Block sender</DialogTitle>
          <DialogDescription>
            Its mail in every account&apos;s Inbox moves to Junk, now and as it arrives. Blocked senders (s b) unblocks it.
          </DialogDescription>
        </DialogHeader>
        <div
          ref={groupRef}
          role="radiogroup"
          aria-label="What to block"
          tabIndex={0}
          className="flex flex-col gap-0.5 rounded-lg outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {(["address", "domain"] as const).map((c) => (
            <div
              key={c}
              role="radio"
              aria-checked={choice === c}
              aria-label={`${dialog?.[c] ?? ""}, ${HINTS[c]}`}
              data-choice={c}
              onClick={() => setChoice(c)}
              className="flex cursor-default items-baseline justify-between gap-3 rounded-md px-2 py-1.5 aria-checked:bg-accent aria-checked:text-accent-foreground"
            >
              <span className="truncate font-medium">{dialog?.[c]}</span>
              <span className="shrink-0 text-xs text-muted-foreground">
                {HINTS[c]} <Kbd aria-hidden="true">{c === "address" ? "a" : "d"}</Kbd>
              </span>
            </div>
          ))}
        </div>
        <p role="alert" data-slot="block-sender-error" className="min-h-5 text-sm text-destructive">
          {error}
        </p>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
          <Button variant="destructive" disabled={busy || !entry} onClick={() => void block()}>
            <Ban aria-hidden="true" />
            Block {entry} <Kbd aria-hidden="true">↵</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
