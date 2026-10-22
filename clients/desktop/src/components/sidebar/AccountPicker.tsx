// The account picker `ga` opens (ticket 0147), the TUI's `Overlay::Account`: every
// account in config order, the selected one current, each with its inbox's
// unread count. A plain list with digits and no filter field, so `j` and `k`
// move rather than type.

import { useCallback, useEffect, useRef, useState } from "react";
import { Check } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { sidebarModel } from "@/components/sidebar/AppSidebar";
import { useAppState, useDispatch } from "@/app/store";

export type AccountPickerProps = { open: boolean; onOpenChange: (open: boolean) => void };

/**
 * j/k and the arrows move the cursor and the focus between the accounts and
 * clamp at the ends; Enter or a click switches, 1 to 9 switch to the nth
 * account, q and Escape close. Enter picks the cursor's account itself, so
 * it works before the dialog has moved the focus onto the list. Picking the current account
 * only closes the picker, so an open search survives it.
 */
export function AccountPicker({ open, onOpenChange }: AccountPickerProps) {
  const s = useAppState();
  const dispatch = useDispatch();
  const accounts = sidebarModel(s);
  const current = s.selection.account;
  const listRef = useRef<HTMLUListElement>(null);
  const currentRef = useRef<HTMLButtonElement>(null);
  const [cursor, setCursor] = useState(current);
  useEffect(() => {
    if (open) setCursor(current);
  }, [open, current]);

  const pick = useCallback(
    (account: string) => {
      onOpenChange(false);
      if (account !== current) dispatch({ type: "select_account", account });
    },
    [onOpenChange, current, dispatch],
  );

  // On the window, so the keys work before the popup has taken focus, and in
  // the capture phase, since the popup stops the arrow keys on their way up.
  const names = accounts.map((a) => a.name).join("\n");
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      if (e.key === "q") {
        e.preventDefault();
        onOpenChange(false);
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        if (cursor) pick(cursor);
        return;
      }
      if (/^[1-9]$/.test(e.key)) {
        e.preventDefault();
        const account = names.split("\n")[Number(e.key) - 1];
        if (account) pick(account);
        return;
      }
      const down = e.key === "j" || e.key === "ArrowDown";
      const up = e.key === "k" || e.key === "ArrowUp";
      if (!down && !up) return;
      e.preventDefault();
      const buttons = Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>("button") ?? []);
      if (buttons.length === 0) return;
      const focused = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const at = focused !== -1 ? focused : buttons.findIndex((b) => b.dataset.account === cursor);
      const next = at === -1 ? 0 : Math.max(0, Math.min(buttons.length - 1, at + (down ? 1 : -1)));
      setCursor(buttons[next].dataset.account ?? null);
      buttons[next].focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, names, cursor, pick, onOpenChange]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={currentRef} data-slot="account-picker" className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>Switch account</DialogTitle>
          <DialogDescription>j and k move, Enter switches, 1 to 9 jump, Escape cancels</DialogDescription>
        </DialogHeader>
        <ul ref={listRef} aria-label="Accounts" className="flex min-w-0 flex-col gap-1">
          {accounts.map((a, i) => {
            const isCurrent = a.name === current;
            const digit = i < 9 ? i + 1 : null;
            const unread = a.inboxUnread ?? 0;
            const label = [
              a.name,
              isCurrent ? "current" : null,
              a.runtime !== "ready" ? a.runtime : null,
              `${unread} unread in the inbox`,
              digit ? `key ${digit}` : null,
            ]
              .filter(Boolean)
              .join(", ");
            return (
              <li key={a.name}>
                <Button
                  type="button"
                  variant="ghost"
                  ref={isCurrent ? currentRef : undefined}
                  data-account={a.name}
                  data-selected={a.name === cursor ? "" : undefined}
                  aria-current={isCurrent ? "true" : undefined}
                  aria-label={label}
                  className="w-full justify-start gap-2 focus-visible:border-transparent focus-visible:ring-0 data-selected:bg-selection data-selected:text-selection-foreground data-selected:hover:bg-selection data-selected:hover:text-selection-foreground dark:data-selected:hover:bg-selection data-selected:*:[svg]:text-selection-foreground"
                  onFocus={() => setCursor(a.name)}
                  onClick={() => pick(a.name)}
                >
                  <Kbd aria-hidden="true" className="font-mono">
                    {digit ?? ""}
                  </Kbd>
                  <span className="min-w-0 flex-1 truncate text-left">{a.name}</span>
                  {a.runtime !== "ready" ? <span className="text-muted-foreground">{a.runtime}</span> : null}
                  {unread > 0 ? (
                    <span aria-hidden="true" className="shrink-0 font-semibold tabular-nums">
                      {unread}
                    </span>
                  ) : null}
                  {isCurrent ? <Check aria-hidden="true" /> : <span aria-hidden="true" className="size-4 shrink-0" />}
                </Button>
              </li>
            );
          })}
        </ul>
      </DialogContent>
    </Dialog>
  );
}
