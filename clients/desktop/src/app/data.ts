// The loaders: they watch the model for missing or stale answers and fetch
// them through the typed commands. Every answer carries the generation it was
// requested at, so an invalidation that lands mid-fetch triggers a refetch.

import { useEffect, useRef, type Dispatch } from "react";
import * as cmd from "@/lib/commands";
import { asGuiError, type GuiEvent } from "@/lib/gui-types";
import { onMenu, subscribe } from "@/lib/events";
import type { Action } from "@/app/reducer";
import { accountNames, isStale, readerKey, type AppState } from "@/app/state";

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
