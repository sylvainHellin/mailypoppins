// A recipient field (To, Cc, Bcc) that completes contacts as the TUI's
// wizard does (`recompute_compose_suggestions`, `accept_suggestion`): the
// text after the last comma asks `contact_search`, and an accepted row
// replaces it with `Name <addr>, `.

import { useEffect, useRef, useState, type ComponentProps, type KeyboardEvent } from "react";
import { Command, CommandItem, CommandList } from "@/components/ui/command";
import { Input } from "@/components/ui/input";
import { acceptRecipient, recipientQuery } from "@/app/compose";
import { contactSearch } from "@/lib/commands";
import type { ContactRow } from "@/lib/gui-types";

/** The TUI's candidate count. */
export const RECIPIENT_LIMIT = 12;
/** How long the typing pauses before a query is asked. */
export const RECIPIENT_DEBOUNCE_MS = 120;
export const NO_CONTACTS_HINT = "No contacts yet: rebuild the index in Contacts";

export type RecipientsInputProps = Omit<ComponentProps<"input">, "value" | "onChange"> & {
  /** The account whose contacts complete; `null` completes nothing. */
  account: string | null;
  value: string;
  onValueChange: (value: string) => void;
};

/**
 * An `Input` with a contact list under it while the text after the last
 * comma matches: ArrowDown and ArrowUp move, Enter or Tab accepts, Escape
 * closes the list and keeps the text, and a click accepts. With no list,
 * every key goes to the caller, so the wizard's Enter still moves on.
 */
export function RecipientsInput({ account, value, onValueChange, onKeyDown, onFocus, onBlur, ...props }: RecipientsInputProps) {
  const [rows, setRows] = useState<ContactRow[] | null>(null);
  const [hint, setHint] = useState(false);
  const [active, setActive] = useState(0);
  const [listId, setListId] = useState<string | undefined>(undefined);
  const [activeId, setActiveId] = useState<string | undefined>(undefined);
  const listRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Every query, and every close, takes a new number; an answer applies
  // only while its number is still the latest.
  const seq = useRef(0);
  const focused = useRef(false);
  // Per focus: whether the empty-index hint showed, and whether the index is empty.
  const hinted = useRef(false);
  const empty = useRef<Promise<boolean> | null>(null);

  const items = rows ?? [];
  const open = items.length > 0;
  const shown = open || hint;

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  // cmdk names the list and its rows itself, and its own
  // `aria-activedescendant` follows only its own keys: mirror the list's id
  // and the selected row's onto the input.
  useEffect(() => {
    const list = listRef.current;
    if (!shown || !list) {
      setListId(undefined);
      setActiveId(undefined);
      return;
    }
    const sync = () => {
      const row = list.querySelector<HTMLElement>('[cmdk-item][aria-selected="true"]');
      setListId(list.id || undefined);
      setActiveId(row?.id || undefined);
      row?.scrollIntoView?.({ block: "nearest" });
    };
    sync();
    const watch = new MutationObserver(sync);
    watch.observe(list, { attributes: true, subtree: true, childList: true, attributeFilter: ["id", "aria-selected"] });
    return () => watch.disconnect();
  }, [shown]);

  const cancel = () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    seq.current += 1;
  };

  const close = () => {
    cancel();
    setRows(null);
    setHint(false);
  };

  const indexEmpty = (acct: string) => {
    empty.current ??= contactSearch(acct, "", 1).then(
      (s) => s.contacts.length === 0,
      () => false,
    );
    return empty.current;
  };

  const ask = (text: string) => {
    cancel();
    const query = recipientQuery(text);
    if (!account || !query) {
      setRows(null);
      setHint(false);
      return;
    }
    const mine = seq.current;
    const acct = account;
    const latest = () => mine === seq.current && focused.current;
    timer.current = setTimeout(() => {
      timer.current = null;
      void contactSearch(acct, query, RECIPIENT_LIMIT).then(
        async (search) => {
          if (!latest()) return;
          if (search.contacts.length > 0) {
            setRows(search.contacts);
            setActive(0);
            setHint(false);
            return;
          }
          setRows(null);
          // An empty index says so once per focus, and the hint stays
          // while the answers stay empty.
          if (hinted.current) return;
          const none = await indexEmpty(acct);
          if (!latest() || !none) return;
          hinted.current = true;
          setHint(true);
        },
        () => {
          if (latest()) setRows(null);
        },
      );
    }, RECIPIENT_DEBOUNCE_MS);
  };

  const accept = (row: ContactRow) => {
    close();
    onValueChange(acceptRecipient(value, row));
  };

  const keyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (!e.nativeEvent.isComposing && shown) {
      const plain = !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
        return;
      }
      if (open && plain && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
        e.preventDefault();
        const n = items.length;
        setActive((i) => (e.key === "ArrowDown" ? (i + 1) % n : (i + n - 1) % n));
        return;
      }
      if (open && plain && (e.key === "Enter" || e.key === "Tab")) {
        e.preventDefault();
        e.stopPropagation();
        accept(items[Math.min(active, items.length - 1)]);
        return;
      }
      // The hint takes no key: Enter moves on as with no list.
      if (!open && e.key === "Enter") setHint(false);
    }
    onKeyDown?.(e);
  };

  return (
    <div className="relative min-w-0">
      <Input
        {...props}
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={shown}
        aria-controls={shown ? listId : undefined}
        aria-activedescendant={open ? activeId : undefined}
        value={value}
        onChange={(e) => {
          onValueChange(e.currentTarget.value);
          ask(e.currentTarget.value);
        }}
        onKeyDown={keyDown}
        onFocus={(e) => {
          focused.current = true;
          hinted.current = false;
          empty.current = null;
          onFocus?.(e);
        }}
        onBlur={(e) => {
          focused.current = false;
          close();
          onBlur?.(e);
        }}
      />
      {shown ? (
        <Command
          shouldFilter={false}
          loop
          value={hint ? "hint" : String(active)}
          onValueChange={(v) => {
            if (open && v !== "hint") setActive(Number(v));
          }}
          // A click on a row must not take the focus from the field.
          onMouseDown={(e) => e.preventDefault()}
          data-slot="recipients-list"
          className="absolute top-full right-0 left-0 z-50 mt-1 h-auto rounded-lg! shadow-md ring-1 ring-foreground/10"
        >
          <CommandList ref={listRef} label="Contacts">
            {open ? (
              items.map((row, i) => (
                <CommandItem key={row.address} value={String(i)} onSelect={() => accept(row)}>
                  <span className="flex min-w-0 flex-1 items-baseline gap-2">
                    {row.display_name ? <span className="truncate">{row.display_name}</span> : null}
                    <span className={row.display_name ? "truncate text-xs text-muted-foreground" : "truncate"}>{row.address}</span>
                  </span>
                </CommandItem>
              ))
            ) : (
              <CommandItem value="hint" disabled data-slot="recipients-hint" className="text-muted-foreground opacity-100!">
                {NO_CONTACTS_HINT}
              </CommandItem>
            )}
          </CommandList>
        </Command>
      ) : null}
    </div>
  );
}
