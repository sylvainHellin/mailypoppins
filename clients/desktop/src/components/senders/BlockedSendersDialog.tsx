// The Blocked senders dialog (`sb`, PERSO-261), the TUI's list: every entry
// of the daemon's one list, live on `sender.blocklist_changed`, and `u` or
// Unblock to take the cursor entry off it.

import { useEffect, useId, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cursorAfterUnblock, unblockSender } from "@/app/senders";
import { useAppState, useDispatch } from "@/app/store";

export type BlockedSendersDialogProps = { open: boolean; onOpenChange: (open: boolean) => void };

/**
 * A single-select listbox of the entries, the first under the cursor on
 * open: `j`/Down and `k`/Up move, `u` unblocks the cursor entry, Escape or
 * `q` closes. Nothing moves back from Junk on an unblock.
 */
export function BlockedSendersDialog({ open, onOpenChange }: BlockedSendersDialogProps) {
  const dispatch = useDispatch();
  const state = useAppState();
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const loadable = state.blockedSenders;
  const data = loadable.data;
  const entries = data ?? [];
  const [cursor, setCursor] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // A new opening starts on the first row.
  useEffect(() => {
    setCursor(null);
    setBusy(false);
    setError(null);
  }, [open]);

  // The cursor stays on its entry while the list changes, else the first row.
  useEffect(() => {
    const listed = data ?? [];
    setCursor((c) => (c !== null && listed.includes(c) ? c : (listed[0] ?? null)));
  }, [data]);

  const latest = useRef({ cursor, entries, busy });
  latest.current = { cursor, entries, busy };

  const actions = {
    move(by: 1 | -1) {
      const { entries: es, cursor: c } = latest.current;
      if (es.length === 0) return;
      const at = c === null ? -1 : es.indexOf(c);
      setCursor(es[Math.max(0, Math.min(es.length - 1, at + by))]);
    },
    async unblock() {
      const { cursor: c, entries: es, busy: b } = latest.current;
      if (b) return;
      if (!c) return setError("No blocked sender to unblock");
      setBusy(true);
      const out = await unblockSender(dispatch, c);
      setBusy(false);
      setError(out.ok ? null : out.error);
      if (out.ok) setCursor(cursorAfterUnblock(es, c));
    },
  };
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const run = actionsRef.current;
      switch (e.key) {
        case "j":
        case "ArrowDown":
          e.preventDefault();
          return run.move(1);
        case "k":
        case "ArrowUp":
          e.preventDefault();
          return run.move(-1);
        case "u":
          e.preventDefault();
          return void run.unblock();
        case "q":
          e.preventDefault();
          return onOpenChange(false);
        default:
          return;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onOpenChange]);

  const optionId = (entry: string) => `${id}-option-${entries.indexOf(entry)}`;
  const empty = data !== null && entries.length === 0;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={listRef} data-slot="blocked-senders" className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Blocked senders</DialogTitle>
          <DialogDescription>Every account moves their mail from its Inbox to Junk. Unblocking moves nothing back.</DialogDescription>
        </DialogHeader>
        <div
          ref={listRef}
          role="listbox"
          aria-label="Blocked senders"
          tabIndex={0}
          aria-activedescendant={cursor !== null && entries.includes(cursor) ? optionId(cursor) : undefined}
          className="flex max-h-72 min-h-16 flex-col gap-0.5 overflow-y-auto rounded-lg outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {data === null && loadable.error ? <p className="px-2 py-1.5 text-sm text-destructive">{loadable.error.message}</p> : null}
          {empty ? <p className="px-2 py-1.5 text-sm text-muted-foreground">No blocked senders; B on a message blocks its sender</p> : null}
          {entries.map((entry) => (
            <div
              key={entry}
              id={optionId(entry)}
              role="option"
              aria-selected={entry === cursor}
              data-blocked={entry}
              onClick={() => setCursor(entry)}
              className="cursor-default truncate rounded-md px-2 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
            >
              {entry}
            </div>
          ))}
        </div>
        <p role="alert" data-slot="blocked-senders-error" className="min-h-5 text-sm text-destructive">
          {error}
        </p>
        <DialogFooter>
          <Button variant="outline" disabled={busy || cursor === null} onClick={() => void actions.unblock()}>
            Unblock <Kbd aria-hidden="true">u</Kbd>
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
