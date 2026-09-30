// One typed wrapper per Tauri command (clients/desktop/docs/rust-layer.md).
// Arguments and results are snake_case exactly as the Rust layer takes and
// answers them. A rejected promise carries a GuiError (see asGuiError).

import { invoke, type Channel } from "@/lib/tauri";
import type {
  Bootstrap,
  DraftCreated,
  DraftKind,
  DraftLocation,
  DraftMessage,
  DraftPreview,
  DraftValidation,
  HoldListing,
} from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  DraftDiscardBatch,
  DraftHeaders,
  DraftStatusBatch,
  EditorLaunch,
  EditorSetting,
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
  SignatureListing,
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

// Drafts and the editor (docs/rust-layer.md, "Drafts and the editor"). Every
// write answers the file's absolute path; the watcher's `draft.changed` or
// `draft.invalid` then reaches the frontend, and the commands publish nothing.

/** `name` is the file name; `headers` are written into the new file client-side. */
export const draftCreate = (
  account: string,
  name: string,
  opts: { signature?: string; no_signature?: boolean; headers?: DraftHeaders } = {},
): Promise<DraftCreated> =>
  invoke<DraftCreated>("draft_create", {
    account,
    name,
    signature: opts.signature ?? null,
    no_signature: opts.no_signature ?? null,
    headers: opts.headers ?? null,
  });

export const draftReply = (
  account: string,
  row_id: number,
  all: boolean,
  headers?: DraftHeaders,
): Promise<DraftCreated> =>
  invoke<DraftCreated>("draft_reply", { account, row_id, all, headers: headers ?? null });

export const draftForward = (account: string, row_id: number, headers?: DraftHeaders): Promise<DraftCreated> =>
  invoke<DraftCreated>("draft_forward", { account, row_id, headers: headers ?? null });

/** A reply, reply-all or forward of a server-only search hit, with no attachments. */
export const draftFromMessage = (account: string, kind: DraftKind, message: DraftMessage): Promise<DraftCreated> =>
  invoke<DraftCreated>("draft_from_message", { account, kind, message });

export const draftPath = (account: string, id: string): Promise<DraftLocation> =>
  invoke<DraftLocation>("draft_path", { account, id });

export const draftApprove = (account: string, ids: string[]): Promise<DraftStatusBatch> =>
  invoke<DraftStatusBatch>("draft_approve", { account, ids });

export const draftDemote = (account: string, ids: string[]): Promise<DraftStatusBatch> =>
  invoke<DraftStatusBatch>("draft_demote", { account, ids });

export const draftValidate = (account: string, id: string): Promise<DraftValidation> =>
  invoke<DraftValidation>("draft_validate", { account, id });

export const draftPreview = (account: string, id: string): Promise<DraftPreview> =>
  invoke<DraftPreview>("draft_preview", { account, id });

/** Rewrites the recipient lines; `subject` absent keeps the draft's own. */
export const draftSetRecipients = (
  account: string,
  id: string,
  fields: { to: string; cc: string; bcc: string; subject?: string },
): Promise<DraftLocation> =>
  invoke<DraftLocation>("draft_set_recipients", {
    account,
    id,
    to: fields.to,
    cc: fields.cc,
    bcc: fields.bcc,
    subject: fields.subject ?? null,
  });

export const signatureList = (account: string): Promise<SignatureListing> =>
  invoke<SignatureListing>("signature_list", { account });

/** Opens the file in the resolved editor and never waits for it; a launch failure is `setup`. */
export const editorOpen = (path: string): Promise<EditorLaunch> => invoke<EditorLaunch>("editor_open", { path });

export const editorSettingGet = (): Promise<EditorSetting> => invoke<EditorSetting>("editor_setting_get");

/** `null` clears the setting. */
export const editorSettingSet = (editor: string | null): Promise<EditorSetting> =>
  invoke<EditorSetting>("editor_setting_set", { editor });

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
