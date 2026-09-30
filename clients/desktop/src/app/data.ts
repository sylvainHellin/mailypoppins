// The loaders: they watch the model for missing or stale answers and fetch
// them through the typed commands. Every answer carries the generation it was
// requested at, so an invalidation that lands mid-fetch triggers a refetch.

import { useEffect, useRef, type Dispatch } from "react";
import * as cmd from "@/lib/commands";
import { asGuiError, type GuiEvent } from "@/lib/gui-types";
import { onMenu, subscribe } from "@/lib/events";
import type { Action } from "@/app/reducer";
import { accountNames, isStale, readerKey, type AppState } from "@/app/state";
import { refusalsWanted } from "@/app/rsvp";
import { CONTACT_LIMIT } from "@/app/contacts";
import { signaturesWanted } from "@/app/signatures";

/** Subscribe once, read the connection and version, and route menu items. */
export function useBoot(dispatch: Dispatch<Action>, onMenuItem: (id: string) => void): void {
  const menuRef = useRef(onMenuItem);
  menuRef.current = onMenuItem;
  useEffect(() => {
    let live = true;
    const onEvent = (event: GuiEvent) => {
      if (live) dispatch({ type: "gui_event", event });
    };
    subscribe(onEvent).catch((e: unknown) => dispatch({ type: "error", error: asGuiError(e) }));
    cmd
      .connectionStatus()
      .then((status) => live && dispatch({ type: "connection_status", status }))
      .catch(() => {});
    const unlisten = onMenu((id) => menuRef.current(id)).catch(() => undefined);
    return () => {
      live = false;
      void unlisten.then((f) => f?.());
    };
  }, [dispatch]);
}

/** Refresh version_info whenever the connection state moves. */
export function useVersionInfo(state: AppState, dispatch: Dispatch<Action>): void {
  const connState = state.connection.state;
  const instance = state.bootstrap?.instance_id;
  useEffect(() => {
    let live = true;
    cmd
      .versionInfo()
      .then((info) => live && dispatch({ type: "version_info", info }))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [connState, instance, dispatch]);
}

export function useDataSync(state: AppState, dispatch: Dispatch<Action>): void {
  const inflight = useRef(new Set<string>());

  function run(tag: string, job: () => Promise<void>) {
    if (inflight.current.has(tag)) return;
    inflight.current.add(tag);
    void job().finally(() => inflight.current.delete(tag));
  }

  const hasBootstrap = state.bootstrap !== null;

  // Accounts.
  const accounts = state.accounts;
  useEffect(() => {
    if (!hasBootstrap || !isStale(accounts)) return;
    const gen = accounts.gen;
    run(`accounts@${gen}`, () =>
      cmd
        .listAccounts()
        .then((list) => dispatch({ type: "accounts_loaded", gen, accounts: list }))
        .catch((e: unknown) => dispatch({ type: "accounts_failed", gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, accounts, dispatch]);

  // Mailboxes, per account.
  const names = accountNames(state).join("\u0000");
  const mailboxes = state.mailboxes;
  useEffect(() => {
    if (!hasBootstrap) return;
    for (const account of names ? names.split("\u0000") : []) {
      const l = mailboxes[account];
      const gen = l ? l.gen : 1;
      if (l && !isStale(l)) continue;
      run(`mailboxes:${account}@${gen}`, () =>
        cmd
          .listMailboxes(account)
          .then((listing) => dispatch({ type: "mailboxes_loaded", account, gen, listing }))
          .catch((e: unknown) =>
            dispatch({ type: "mailboxes_failed", account, gen, error: asGuiError(e) }),
          ),
      );
    }
  }, [hasBootstrap, names, mailboxes, dispatch]);

  // Each outbox this window reads: created on the first open of the view
  // or the first `state.invalidate` of `outbox:<account>`, and stale again
  // on the next one and on every bootstrap.
  const outbox = state.outbox;
  useEffect(() => {
    if (!hasBootstrap) return;
    for (const [account, l] of Object.entries(outbox)) {
      if (!isStale(l)) continue;
      const gen = l.gen;
      run(`outbox:${account}@${gen}`, () =>
        cmd
          .outboxList(account)
          .then((listing) => dispatch({ type: "outbox_loaded", account, gen, listing }))
          .catch((e: unknown) => dispatch({ type: "outbox_failed", account, gen, error: asGuiError(e) })),
      );
    }
  }, [hasBootstrap, outbox, dispatch]);

  // The agenda the Calendar view shows, and only while it shows: an agenda
  // that went stale behind another view is read when the view comes back.
  const calendarAccount = state.view === "calendar" ? (state.calendarView?.account ?? null) : null;
  const agenda = calendarAccount ? state.calendar[calendarAccount] : undefined;
  useEffect(() => {
    if (!hasBootstrap || !calendarAccount || !agenda || !isStale(agenda)) return;
    const gen = agenda.gen;
    const account = calendarAccount;
    run(`calendar:${account}@${gen}`, () =>
      cmd
        .calendarEvents(account)
        .then((events) => dispatch({ type: "calendar_loaded", account, gen, events }))
        .catch((e: unknown) => dispatch({ type: "calendar_failed", account, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, calendarAccount, agenda, dispatch]);

  // The contacts the Contacts view shows, and only while it shows, for the
  // view's query; a list that went stale behind another view is read when
  // the view comes back.
  const contactsAccount = state.view === "contacts" ? (state.contactsView?.account ?? null) : null;
  const contactsQuery = state.contactsView?.query ?? "";
  const contacts = contactsAccount ? state.contacts[contactsAccount] : undefined;
  useEffect(() => {
    if (!hasBootstrap || !contactsAccount || !contacts || !isStale(contacts)) return;
    const gen = contacts.gen;
    const account = contactsAccount;
    run(`contacts:${account}@${gen}`, () =>
      cmd
        .contactSearch(account, contactsQuery, CONTACT_LIMIT)
        .then((search) => dispatch({ type: "contacts_loaded", account, gen, search }))
        .catch((e: unknown) => dispatch({ type: "contacts_failed", account, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, contactsAccount, contactsQuery, contacts, dispatch]);

  // The daemon's configuration, while the Settings view shows: read on
  // every open and again after each `config.changed`.
  const settingsShown = state.view === "settings";
  const config = state.config;
  useEffect(() => {
    if (!hasBootstrap || !settingsShown || !isStale(config)) return;
    const gen = config.gen;
    run(`config@${gen}`, () =>
      cmd
        .configGet()
        .then((snapshot) => dispatch({ type: "config_loaded", gen, snapshot }))
        .catch((e: unknown) => dispatch({ type: "config_failed", gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, settingsShown, config, dispatch]);

  // The signature listing the Signatures dialog or the new-draft wizard
  // shows, and only while one of them is open: a listing that went stale
  // behind a closed one is read when it opens again.
  const signaturesAccount = signaturesWanted(state);
  const signatures = signaturesAccount ? state.signatures[signaturesAccount] : undefined;
  useEffect(() => {
    if (!hasBootstrap || !signaturesAccount || !signatures || !isStale(signatures)) return;
    const gen = signatures.gen;
    const account = signaturesAccount;
    run(`signatures:${account}@${gen}`, () =>
      cmd
        .signatureList(account)
        .then((listing) => dispatch({ type: "signatures_loaded", account, gen, listing }))
        .catch((e: unknown) => dispatch({ type: "signatures_failed", account, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, signaturesAccount, signatures, dispatch]);

  // The selected mailbox's list.
  // The answer carries the list generation it was asked at, so a reload that
  // started before an optimistic mutation is dropped when it lands.
  const { account, mailbox } = state.selection;
  const messages = state.messages;
  const listGenRef = useRef(state.listGen);
  listGenRef.current = state.listGen;
  useEffect(() => {
    if (!hasBootstrap || !account || !mailbox || messages.key === null || !isStale(messages)) return;
    const key = messages.key;
    const gen = messages.gen;
    const lgen = listGenRef.current[key] ?? 0;
    run(`messages:${key}@${gen}`, () =>
      cmd
        .listMessages(account, mailbox)
        .then((list) => dispatch({ type: "messages_loaded", key, gen, lgen, list }))
        .catch((e: unknown) => dispatch({ type: "messages_failed", key, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, account, mailbox, messages, dispatch]);

  // The reader: the headers of a verified selection. The body is the
  // `mpmsg` document the reader's iframe loads by itself.
  const message = state.selection.message;
  const reader = state.reader;
  useEffect(() => {
    if (!hasBootstrap || !account || !message || !message.verified) return;
    const key = readerKey(account, message.row_id);
    if (reader.key === key && !isStale(reader.load)) return;
    const gen = reader.load.gen;
    const rowId = message.row_id;
    run(`reader:${key}@${gen}`, () =>
      cmd
        .messageHtmlMeta(account, rowId)
        .then((meta) => dispatch({ type: "reader_loaded", key, gen, meta }))
        .catch((e: unknown) => dispatch({ type: "reader_failed", key, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, account, message, reader, dispatch]);

  // The reader's invitation card, while the reader shows that invitation:
  // created by `reader_loaded`, stale again with the account's agenda.
  const meta = state.view === "mail" && state.reader.meta?.invite ? state.reader.meta : null;
  const inviteKey = meta ? readerKey(meta.account, meta.row_id) : null;
  const invite = inviteKey ? state.invites[inviteKey] : undefined;
  useEffect(() => {
    if (!hasBootstrap || !meta || !inviteKey || !invite || !isStale(invite)) return;
    const gen = invite.gen;
    const key = inviteKey;
    const { account, row_id: rowId } = meta;
    run(`invite:${key}@${gen}`, () =>
      cmd
        .inviteGet(account, rowId)
        .then((event) => dispatch({ type: "invite_loaded", key, gen, event }))
        .catch((e: unknown) => dispatch({ type: "invite_failed", key, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, meta, inviteKey, invite, dispatch]);

  // Each account whose Graph refusal a shown invitation or agenda needs, once.
  const refusals = refusalsWanted(state).join("\u0000");
  useEffect(() => {
    if (!hasBootstrap || !refusals) return;
    for (const account of refusals.split("\u0000")) {
      run(`refusal:${account}`, () =>
        cmd
          .inviteRefusal(account)
          .then((answer) => dispatch({ type: "invite_refusal_loaded", account, refusal: answer.refusal }))
          // A probe that failed says nothing: the daemon still refuses the RSVP itself.
          .catch(() => dispatch({ type: "invite_refusal_loaded", account, refusal: null })),
      );
    }
  }, [hasBootstrap, refusals, dispatch]);

  useSearchSync(state, dispatch);
}

/**
 * Runs what the search state asks for: the local query, the server start,
 * and the cancel of a server search the user walked away from (a new query,
 * Escape, another mailbox) while it still ran.
 */
function useSearchSync(state: AppState, dispatch: Dispatch<Action>): void {
  const search = state.search;
  const current = useRef(search);
  current.current = search;

  const want =
    search && search.status === "searching"
      ? { mode: search.mode, seq: search.seq, account: search.account, query: search.query }
      : null;
  const wantKey = want ? `${want.mode}:${want.seq}` : null;
  const exclude =
    search && search.mode === "server" && search.status === "searching"
      ? search.hits.flatMap((h) => (h.message_id === null ? [] : [h.message_id]))
      : [];
  const excludeRef = useRef(exclude);
  excludeRef.current = exclude;

  const started = useRef<string | null>(null);
  useEffect(() => {
    if (!want || started.current === wantKey) return;
    started.current = wantKey;
    const { mode, seq, account, query } = want;
    if (mode === "local") {
      cmd
        .searchLocal({ account, query })
        .then((hits) => dispatch({ type: "search_local_loaded", seq, hits }))
        .catch((e: unknown) => dispatch({ type: "search_local_failed", seq, error: asGuiError(e) }));
      return;
    }
    const ids = excludeRef.current;
    cmd
      .searchServerStart({ account, query, ...(ids.length ? { exclude_message_ids: ids } : {}) })
      .then(({ operation_id }) => {
        const now = current.current;
        if (!now || now.seq !== seq || now.mode !== "server") {
          // Superseded before it answered: nobody will read its hits.
          void cmd.searchServerCancel(operation_id).catch(() => {});
          return;
        }
        dispatch({ type: "search_server_started", seq, operation_id });
      })
      .catch((e: unknown) => dispatch({ type: "search_server_failed", seq, error: asGuiError(e) }));
    // `want` is keyed by wantKey: its fields do not change under one key.
  }, [wantKey, dispatch]);

  // A running operation the search no longer shows is cancelled.
  const running = search && search.status === "running" ? search.operationId : null;
  const shownOp = search?.operationId ?? null;
  const prev = useRef<string | null>(null);
  useEffect(() => {
    const was = prev.current;
    prev.current = running;
    if (was && was !== running && was !== shownOp) {
      void cmd.searchServerCancel(was).catch(() => {});
    }
  }, [running, shownOp]);
}
