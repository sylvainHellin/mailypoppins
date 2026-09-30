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

  // The selected mailbox's list.
  const { account, mailbox } = state.selection;
  const messages = state.messages;
  useEffect(() => {
    if (!hasBootstrap || !account || !mailbox || messages.key === null || !isStale(messages)) return;
    const key = messages.key;
    const gen = messages.gen;
    run(`messages:${key}@${gen}`, () =>
      cmd
        .listMessages(account, mailbox)
        .then((list) => dispatch({ type: "messages_loaded", key, gen, list }))
        .catch((e: unknown) => dispatch({ type: "messages_failed", key, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, account, mailbox, messages, dispatch]);

  // The reader: headers and the plain-text body of a verified selection.
  const message = state.selection.message;
  const reader = state.reader;
  useEffect(() => {
    if (!hasBootstrap || !account || !message || !message.verified) return;
    const key = readerKey(account, message.row_id);
    if (reader.key === key && !isStale(reader.load)) return;
    const gen = reader.load.gen;
    const rowId = message.row_id;
    run(`reader:${key}@${gen}`, () =>
      Promise.all([cmd.messageHtmlMeta(account, rowId), cmd.messageText(account, rowId)])
        .then(([meta, text]) => dispatch({ type: "reader_loaded", key, gen, meta, text }))
        .catch((e: unknown) => dispatch({ type: "reader_failed", key, gen, error: asGuiError(e) })),
    );
  }, [hasBootstrap, account, message, reader, dispatch]);
}
