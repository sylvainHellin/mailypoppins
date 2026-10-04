import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { ArrowLeft, Contact, Copy, Paperclip, RotateCw, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { ContactList } from "@/components/contacts/ContactList";
import { runAction } from "@/app/actions";
import {
  contactRows,
  CONTACTS_SEARCH_ID,
  cursorContact,
  EMPTY_INDEX,
  NO_MATCH,
  rebuildOf,
  SEARCH_DEBOUNCE_MS,
} from "@/app/contacts";
import { useAppState, useDispatch } from "@/app/store";
import { usePaneFocused } from "@/hooks/use-pane-focused";
import type { ActionId } from "@/keymap/catalog";

/** The daemon's `AccountNotReady`: the account has no local store to read yet. */
const ACCOUNT_NOT_READY = -32006;

/**
 * The Contacts view (clients/desktop/docs/shell.md, "Contacts"): the
 * selection account's ranked contacts, searched through the daemon as the
 * search field's typing pauses. Enter or `n` composes to the cursor contact,
 * `v` sends it as a vCard, `c` copies its address, `r` rebuilds the index,
 * `/` focuses the search field and Escape leaves it with its query kept.
 */
export function ContactsView() {
  const s = useAppState();
  const dispatch = useDispatch();
  const focused = usePaneFocused("list");
  const view = s.contactsView;
  const account = view?.account ?? null;
  const l = account ? s.contacts[account] : undefined;
  const search = l?.data ?? null;
  const rows = contactRows(s);
  const current = cursorContact(s, rows);
  const rebuild = account ? rebuildOf(s, account) : null;
  const progress = rebuild?.operation_id ? s.progress[rebuild.operation_id] : undefined;
  const latest = useRef(s);
  latest.current = s;
  const run = useCallback((id: ActionId) => runAction(id, latest.current, dispatch), [dispatch]);

  // The field's text; the list is asked for it once the typing pauses.
  const [text, setText] = useState(view?.query ?? "");
  const query = view?.query ?? "";
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const commit = useCallback(
    (q: string) => {
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
      dispatch({ type: "contacts_query", query: q });
    },
    [dispatch],
  );
  useEffect(() => {
    if (text === query) return;
    timer.current = setTimeout(() => commit(text), SEARCH_DEBOUNCE_MS);
    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, [text, query, commit]);

  const onFieldKey = (e: KeyboardEvent<HTMLInputElement>) => {
    // Enter asks now and hands the keys back to the list, the query kept.
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    e.preventDefault();
    commit(text);
    e.currentTarget.blur();
    dispatch({ type: "focus", pane: "list" });
  };

  const select = useCallback((address: string) => dispatch({ type: "contacts_select", address }), [dispatch]);
  // A double-click composes to the row clicked, which the model's cursor may not be yet.
  const open = useCallback(
    (address: string) => {
      dispatch({ type: "contacts_select", address });
      const now = latest.current;
      const v = now.contactsView;
      if (v) runAction("contacts_compose", { ...now, contactsView: { ...v, cursor: address } }, dispatch);
    },
    [dispatch],
  );

  const count = search ? `${rows.length} ${rows.length === 1 ? "contact" : "contacts"}${query ? ` matching ${query}` : ""}` : "loading";
  const status = rebuild
    ? `Rebuilding the contact index of ${account}${progress?.message && progress.message !== account ? `: ${progress.message}` : ""}…`
    : account
      ? `${account}: ${count}`
      : "\u00a0";

  return (
    <section
      aria-label="Contacts"
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-focused={focused}
      data-view="contacts"
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 flex-col gap-2 border-b border-border px-3 py-2">
        <div className="flex items-center justify-between gap-2">
          <h2 className="truncate text-sm font-semibold">Contacts</h2>
          <span className="flex shrink-0 flex-wrap justify-end gap-1">
            {/* Only with a contact under the cursor: a view switch focuses the
                first control with tabindex 0, which a disabled button keeps. */}
            {current ? (
              <>
                <Button size="xs" variant="ghost" onClick={() => run("contacts_compose")} title="Compose to contact (Enter, n)">
                  <Contact aria-hidden="true" />
                  Compose
                </Button>
                <Button size="xs" variant="ghost" onClick={() => run("contacts_vcard")} title="Send contact as vCard (v)">
                  <Paperclip aria-hidden="true" />
                  Send vCard
                </Button>
                <Button size="xs" variant="ghost" onClick={() => run("contacts_copy")} title="Copy email address (c)">
                  <Copy aria-hidden="true" />
                  Copy address
                </Button>
              </>
            ) : null}
            <Button
              size="xs"
              variant="ghost"
              onClick={() => run("contacts_rebuild")}
              title="Refresh contact index (r)"
              disabled={!account}
              aria-busy={rebuild !== null || undefined}
            >
              <RotateCw aria-hidden="true" className={rebuild ? "animate-spin" : undefined} />
              Rebuild index
            </Button>
            <Button size="xs" variant="ghost" onClick={() => dispatch({ type: "switch_view", view: "mail" })} title="Back to Mail (Esc)">
              <ArrowLeft aria-hidden="true" />
              Mail
            </Button>
          </span>
        </div>
        <div className="relative">
          <Search aria-hidden="true" className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            id={CONTACTS_SEARCH_ID}
            aria-label="Search contacts"
            placeholder="Type / to search"
            autoComplete="off"
            spellCheck={false}
            className="pl-7"
            value={text}
            disabled={!account}
            onChange={(e) => setText(e.currentTarget.value)}
            onKeyDown={onFieldKey}
            onFocus={() => dispatch({ type: "contacts_searching", searching: true })}
            onBlur={() => dispatch({ type: "contacts_searching", searching: false })}
          />
        </div>
        <p data-slot="contacts-status" role="status" className="text-xs text-muted-foreground tabular-nums">
          {status}
        </p>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {!account ? (
          <p className="p-4 text-sm text-muted-foreground">No account is selected.</p>
        ) : l?.error && !search ? (
          "code" in l.error && l.error.code === ACCOUNT_NOT_READY ? (
            <p className="p-4 text-sm text-muted-foreground">
              {account} has no local store yet, so it has no contacts; they appear after the account's first sync.
            </p>
          ) : (
            <p role="alert" className="p-4 text-sm text-destructive">
              The contacts did not load: {l.error.message}
            </p>
          )
        ) : !search ? (
          <div aria-busy="true" aria-label="Loading the contacts" className="flex flex-col gap-2 p-3">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-9 w-full" />
            ))}
          </div>
        ) : rows.length === 0 ? (
          <p data-slot="contacts-empty" className="p-4 text-sm text-muted-foreground">
            {search.query.trim() ? NO_MATCH : EMPTY_INDEX}
          </p>
        ) : (
          <ContactList rows={rows} cursor={current?.address ?? null} focusSeq={s.focusSeq} onSelect={select} onOpen={open} />
        )}
      </div>
    </section>
  );
}
