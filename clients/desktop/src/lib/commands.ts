// One typed wrapper per Tauri command (clients/desktop/docs/rust-layer.md).
// Arguments and results are snake_case exactly as the Rust layer takes and
// answers them. A rejected promise carries a GuiError (see asGuiError).

import { invoke, type Channel } from "@/lib/tauri";
import type { Bootstrap, HoldListing } from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  DraftDiscardBatch,
  FixtureSimulation,
  GuiEvent,
  HoldCancelled,
  InterceptedUrl,
  LocalSearchHit,
  LocalSearchParams,
  MailboxListing,
  MessageList,
  MessageMeta,
  MessageText,
  MutationBatch,
  OperationStarted,
  ServerSearchParams,
  SyncMode,
  VersionInfo,
} from "@/lib/gui-types";

export const subscribeEvents = (on_event: Channel<GuiEvent>): Promise<void> =>
  invoke<void>("subscribe_events", { on_event });

export const connectionStatus = (): Promise<ConnectionStatus> =>
  invoke<ConnectionStatus>("connection_status");

export const retryConnect = (): Promise<void> => invoke<void>("retry_connect");

export const bootstrap = (): Promise<Bootstrap> => invoke<Bootstrap>("bootstrap");

export const listAccounts = (): Promise<AccountInfo[]> => invoke<AccountInfo[]>("list_accounts");

export const listMailboxes = (account: string): Promise<MailboxListing> =>
  invoke<MailboxListing>("list_mailboxes", { account });

export const listMessages = (account: string, mailbox: string): Promise<MessageList> =>
  invoke<MessageList>("list_messages", { account, mailbox });

export const messageText = (account: string, row_id: number): Promise<MessageText> =>
  invoke<MessageText>("message_text", { account, row_id });

export const messageHtmlMeta = (account: string, row_id: number): Promise<MessageMeta> =>
  invoke<MessageMeta>("message_html_meta", { account, row_id });

export const searchLocal = (params: LocalSearchParams): Promise<LocalSearchHit[]> =>
  invoke<LocalSearchHit[]>("search_local", { params });

export const searchServerStart = (params: ServerSearchParams): Promise<{ operation_id: string }> =>
  invoke<{ operation_id: string }>("search_server_start", { params });

export const searchServerCancel = (
  operation_id: string,
): Promise<"cancelled" | "already_settled"> =>
  invoke<"cancelled" | "already_settled">("search_server_cancel", { operation_id });

// Mutations take a list of row ids, one call per id in the list's order, all with
// `settle: false`; a row the daemon refused is in `failed` and the rest go ahead.

export const messageArchive = (account: string, row_ids: number[]): Promise<MutationBatch> =>
  invoke<MutationBatch>("message_archive", { account, row_ids });

export const messageDelete = (account: string, row_ids: number[]): Promise<MutationBatch> =>
  invoke<MutationBatch>("message_delete", { account, row_ids });

export const messageMove = (
  account: string,
  row_ids: number[],
  destination: string,
): Promise<MutationBatch> =>
  invoke<MutationBatch>("message_move", { account, row_ids, destination });

export const messageSetFlag = (
  account: string,
  row_ids: number[],
  flagged: boolean,
): Promise<MutationBatch> =>
  invoke<MutationBatch>("message_set_flag", { account, row_ids, flagged });

export const messageSetRead = (
  account: string,
  row_ids: number[],
  read: boolean,
): Promise<MutationBatch> =>
  invoke<MutationBatch>("message_set_read", { account, row_ids, read });

export const draftDiscard = (account: string, ids: string[]): Promise<DraftDiscardBatch> =>
  invoke<DraftDiscardBatch>("draft_discard", { account, ids });

export const sendHoldStatus = (account?: string): Promise<HoldListing> =>
  invoke<HoldListing>("send_hold_status", { account: account ?? null });

export const sendCancelHold = (operation_id: string): Promise<HoldCancelled> =>
  invoke<HoldCancelled>("send_cancel_hold", { operation_id });

export const syncTrigger = (account: string, mode: SyncMode): Promise<OperationStarted> =>
  invoke<OperationStarted>("sync_trigger", { account, mode });

export const restartDaemon = (): Promise<void> => invoke<void>("restart_daemon");

export const interceptedUrls = (): Promise<InterceptedUrl[]> =>
  invoke<InterceptedUrl[]>("intercepted_urls");

export const openExternal = (url: string): Promise<void> => invoke<void>("open_external", { url });

export const versionInfo = (): Promise<VersionInfo> => invoke<VersionInfo>("version_info");

export const fixtureSimulate = (what: FixtureSimulation): Promise<void> =>
  invoke<void>("fixture_simulate", { what });
