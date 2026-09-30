// One typed wrapper per Tauri command (clients/desktop/docs/rust-layer.md).
// Arguments and results are snake_case exactly as the Rust layer takes and
// answers them. A rejected promise carries a GuiError (see asGuiError).

import { invoke, type Channel } from "@/lib/tauri";
import type {
  AgendaEvent,
  Bootstrap,
  DraftCreated,
  DraftKind,
  DraftLocation,
  DraftMessage,
  DraftPreview,
  DraftValidation,
  EventFrontmatter,
  HoldListing,
  OutboxListing,
} from "@/protocol/types";
import type {
  AccountInfo,
  ConnectionStatus,
  ContactSearch,
  DraftAttachments,
  DraftDiscardBatch,
  DraftHeaders,
  DraftStatusBatch,
  EditorLaunch,
  EditorSetting,
  FetchOutcome,
  FixtureSimulation,
  GuiEvent,
  HoldCancelled,
  InterceptedUrl,
  InviteRefusal,
  LocalSearchHit,
  LocalSearchParams,
  MailboxListing,
  MessageList,
  MessageMeta,
  MessageText,
  MutationBatch,
  OpenedFile,
  OperationStarted,
  OutboxDiscarded,
  SavedAttachments,
  SendStarted,
  ServerSearchParams,
  SignatureListing,
  SyncMode,
  VcardDraft,
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

/**
 * Send one draft; a `draft` status is validated and approved first. Rejects
 * with a `SendRefusal`: a GuiError, with `invalid` for a file that does not parse.
 */
export const sendDraft = (account: string, id: string, hold: boolean): Promise<SendStarted> =>
  invoke<SendStarted>("send_draft", { account, id, hold });

/** Send every approved draft of `account`, one operation. */
export const sendApproved = (account: string, hold: boolean): Promise<SendStarted> =>
  invoke<SendStarted>("send_approved", { account, hold });

// The outbox (docs/rust-layer.md, "The outbox"): the listing, a retry the
// Rust layer awaits as `outbox_retry`, and a discard that answers at once.

export const outboxList = (account: string): Promise<OutboxListing> =>
  invoke<OutboxListing>("outbox_list", { account });

/** Rejects with `-32602` for a row the daemon will not retry; the end arrives as `outbox_retry`. */
export const outboxRetry = (account: string, row_id: number): Promise<OperationStarted> =>
  invoke<OperationStarted>("outbox_retry", { account, row_id });

export const outboxDiscard = (account: string, row_id: number): Promise<OutboxDiscarded> =>
  invoke<OutboxDiscarded>("outbox_discard", { account, row_id });

// The agenda (docs/rust-layer.md, "The calendar").

/** Every row, past ones included: the past/upcoming filter is the frontend's. Rejects with code -32006 for a store not ready yet. */
export const calendarEvents = (account: string): Promise<AgendaEvent[]> =>
  invoke<AgendaEvent[]>("calendar_events", { account });

/** Open an agenda row's `invite.ics` in the editor; `not_found` when the row has none. */
export const inviteSourceOpen = (account: string, row_id: number): Promise<EditorLaunch> =>
  invoke<EditorLaunch>("invite_source_open", { account, row_id });

// The invitations (docs/rust-layer.md, "The calendar"): a message's event,
// an RSVP the Rust layer awaits as `rsvp`, and the Graph probe.

/** The event a message carries, or null for one with no invitation. */
export const inviteGet = (account: string, row_id: number): Promise<EventFrontmatter | null> =>
  invoke<EventFrontmatter | null>("invite_get", { account, row_id });

/** Reply to an invitation; the end arrives as `rsvp`, its result an `RsvpSettled`. */
export const calendarRsvp = (account: string, row_id: number, response: "accept" | "tentative" | "decline"): Promise<OperationStarted> =>
  invoke<OperationStarted>("calendar_rsvp", { account, row_id, response });

/** The daemon's Graph sentence for a Graph account, null for one that can reply and send. */
export const inviteRefusal = (account: string): Promise<InviteRefusal> =>
  invoke<InviteRefusal>("invite_refusal", { account });

/** The New invitation form's fields; an empty one is not sent, and `end` and `duration` are exclusive. */
export type InviteFields = {
  subject: string;
  start: string;
  to?: string | null;
  cc?: string | null;
  end?: string | null;
  duration?: string | null;
  location?: string | null;
  description?: string | null;
};

/** Send a new invitation; the end arrives as `send_invite`, its result a `SendOutcome`. A refusal's message is the daemon's sentence. */
export const sendInvite = (account: string, fields: InviteFields): Promise<OperationStarted> =>
  invoke<OperationStarted>("send_invite", { account, ...fields });

// The contacts (docs/rust-layer.md, "The contacts").

/** `account`'s contacts matching `query` (all of them for an empty one), at most `limit`, best first. */
export const contactSearch = (account: string, query: string, limit: number): Promise<ContactSearch> =>
  invoke<ContactSearch>("contact_search", { account, query, limit });

/** Rebuild `account`'s contact index; the end arrives as `contact_rebuild`, its result a `ContactRebuilt`. */
export const contactRebuild = (account: string): Promise<OperationStarted> =>
  invoke<OperationStarted>("contact_rebuild", { account });

/** A new draft named `name` to the contact, its vCard written beside the drafts and attached. */
export const contactVcardDraft = (account: string, name: string, address: string, display_name: string): Promise<VcardDraft> =>
  invoke<VcardDraft>("contact_vcard_draft", { account, name, address, display_name });

export const syncTrigger = (account: string, mode: SyncMode): Promise<OperationStarted> =>
  invoke<OperationStarted>("sync_trigger", { account, mode });

// Attachments and the browser rendition (docs/rust-layer.md, "Attachments").

/** Open part `part` of a stored message with the system opener. */
export const attachmentOpen = (account: string, row_id: number, part: number): Promise<OpenedFile> =>
  invoke<OpenedFile>("attachment_open", { account, row_id, part });

/** Copy `parts` into `dest_dir` (absolute or `~`), with the `_1` rule for a name already taken. */
export const attachmentSave = (account: string, row_id: number, parts: number[], dest_dir: string): Promise<SavedAttachments> =>
  invoke<SavedAttachments>("attachment_save", { account, row_id, parts, dest_dir });

/** The daemon's browser rendition, opened; null for a message with no HTML part. */
export const htmlOpen = (account: string, row_id: number): Promise<OpenedFile | null> =>
  invoke<OpenedFile | null>("html_open", { account, row_id });

/** A server-only hit's markup, written into the app cache and opened in the browser. */
export const hitHtmlOpen = (html: string): Promise<OpenedFile> => invoke<OpenedFile>("hit_html_open", { html });

export const draftAttachments = (account: string, id: string): Promise<DraftAttachments> =>
  invoke<DraftAttachments>("draft_attachments", { account, id });

/** Append an absolute or `~` path to the draft's `attachments:`; a missing file or a duplicate is refused. */
export const draftAttach = (account: string, id: string, path: string): Promise<DraftAttachments> =>
  invoke<DraftAttachments>("draft_attach", { account, id, path });

export const draftAttachmentRemove = (account: string, id: string, index: number): Promise<DraftAttachments> =>
  invoke<DraftAttachments>("draft_attachment_remove", { account, id, index });

export const draftAttachmentOpen = (account: string, id: string, index: number): Promise<OpenedFile> =>
  invoke<OpenedFile>("draft_attachment_open", { account, id, index });

/** Fetch a server-only message into the store; answers once the fetch has ended. */
export const messageFetch = (account: string, mailbox: string, message_id: string): Promise<FetchOutcome> =>
  invoke<FetchOutcome>("message_fetch", { account, mailbox, message_id });

export const restartDaemon = (): Promise<void> => invoke<void>("restart_daemon");

export const interceptedUrls = (): Promise<InterceptedUrl[]> =>
  invoke<InterceptedUrl[]>("intercepted_urls");

export const openExternal = (url: string): Promise<void> => invoke<void>("open_external", { url });

export const versionInfo = (): Promise<VersionInfo> => invoke<VersionInfo>("version_info");

export const fixtureSimulate = (what: FixtureSimulation): Promise<void> =>
  invoke<void>("fixture_simulate", { what });
