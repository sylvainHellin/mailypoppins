# The desktop client's Rust layer

The Tauri Rust layer in `src-tauri/src/` is the GUI's only road to the daemon.
It links `mp-client`, `mp-protocol` and `mp-core`, never the root `mailypoppins` crate.
The frontend calls the commands below with `invoke` and listens on one ordered event channel; it never sends a daemon method name.

## Modules

| Module | Owns |
|---|---|
| `paths.rs` | Data, config, runtime, socket and log paths, resolved through `mp_core::config` like the binary |
| `connector.rs` | The handshake as `ClientKind::Gui`, the version check against the `mp` the app starts, the on-demand start through `mp daemon start`, `ConnectError`, `mp daemon restart` |
| `session.rs` | The one `Session`, the `StateTracker` watermark, the event pump, re-bootstrap, the awaited-operations table |
| `commands.rs` | The Tauri commands and their result types |
| `editor.rs` | The external editor a draft opens in, and the editor setting |
| `terminal.rs` | The embedded terminal editor: one PTY session per draft, its reader and pump threads, and the session table |
| `settings.rs` | `desktop.json`, the desktop's own settings: the editor template, the theme, the reader mode and the editor's colours |
| `attachments.rs` | Attachments, a draft's `attachments:` list, and the browser rendition |
| `calendar.rs` | The agenda, an agenda entry's `invite.ics` in the editor, a message's invitation, the RSVP, the Graph probe and a new invitation |
| `contacts.rs` | The ranked contacts with their recipient, the index rebuild, and a contact's vCard draft |
| `signatures.rs` | The Signatures dialog's reads and changes over `mp_core::signatures` |
| `daemon_files.rs` | The daemon's `config.toml` and its log file in the editor |
| `configuration.rs` | The Settings view's `config.get`, reload and password store, the account wizard's writes and the device-code sign-in |
| `reader.rs` | The `mpmsg` scheme serving `message.html` |
| `navigation.rs` | The webview's navigation allowlist and the intercepted-URL log |
| `fixture.rs` | The daemon stand-in behind `MP_DESKTOP_FIXTURE=1` |
| `menu.rs` | The native menus (App, File, Edit, View, Window, Help) and their `menu` event |
| `updates.rs` | The app's own updates: the check, the install, the relaunch, `update-state.json` |
| `error.rs` | `GuiError` |

## Conventions

Every argument and every result field is snake_case, so `invoke("message_text", { account, row_id })`.
Protocol types embedded in results (`Bootstrap`, `MessageListRow`, `DraftListing`, `EventEnvelope`, `OperationStatus`) are serialised exactly as they travel on the wire (`docs/daemon-protocol.md`).
A command that can fail rejects with a `GuiError`:

```ts
type GuiError =
  | { kind: "daemon_unavailable"; message: string; socket: string | null; log: string | null }
  | { kind: "version_mismatch"; message: string; daemon_version: string | null; client_protocol: { min: number; max: number } }
  | { kind: "timeout"; message: string }
  | { kind: "protocol"; message: string; code: number | null }
  | { kind: "not_found"; message: string; code: number | null }
  | { kind: "setup"; message: string }
  | { kind: "internal"; message: string };
```

`version_mismatch` is the blocking restart screen; its button calls `restart_daemon` after the user confirms.
The connect also refuses a daemon whose `app_version` is not the version of the `mp` the layer would start one with (`MP_DESKTOP_MP_BIN`, else the bundled sidecar, else `PATH`; see Environment), read off that binary's `mp --version` and read again when the binary's size or modification time changes.
That refusal is a `version_mismatch` too, its `why` naming both versions and the binary, so a daemon left by another install or by a `cargo install` with no `mp daemon restart` gets the same screen, and Restart replaces it with the matching one.
A binary that is missing or prints no version skips the check, with a warning in the log.
On a reconnect the same refusal turns `reconnecting` into `failed` with that error, pushed as soon as the session thread records it, while the thread keeps retrying; the next successful reconnect is `connected` again.
The CLI and the TUI compare only the protocol range, since each is itself the `mp` that would start the daemon.
The handshake requires every daemon method the layer calls (`REQUIRED_CAPABILITIES` in `connector.rs`), so a daemon that lacks one lands on that screen instead of failing at the first call; under test the fixture door panics on a method missing from the list, less the methods only the fixture answers (`FIXTURE_ONLY_METHODS` in `fixture.rs`: `signature.read`, `signature.create`, `signature.rename`, `signature.delete` and `signature.set_default`), whose work the layer does itself over a daemon.
`setup` is the desktop's own configuration: an editor that did not start, a settings file that does not read, or a setting value its key cannot hold; its message names what to change.

## Types

The TypeScript types are generated from the Rust ones with ts-rs 12 into `src/protocol/generated/`.
The wire types come from `mp-protocol`, which derives `TS` behind its `ts` feature, so the daemon and the CLI never compile ts-rs.
This layer's result, argument and event types go into `generated/gui/` and derive `TS` under `cfg(test)` only, with ts-rs a dev-dependency, so the app build does not compile it either.
The frontend imports them through `src/protocol/types.ts` and `src/lib/gui-types.ts`, which re-export the generated files.
The two files hand-write only what has no Rust type: the event payloads the daemon assembles inline, and `FixtureSimulation`.

`u64` and `i64` come out as `number`, the way `serde_json` hands them to JavaScript.
A field under `skip_serializing_if` becomes optional, and a `serde_json::Value` becomes `unknown`.
`send::OutboxCounts` (`open`, `failed`, `partial`, under an outbox listing) is exported as `OutboxListingCounts` by a `#[ts(rename)]` in `mp-protocol`, since the bootstrap's `state::OutboxCounts` (`queued`, `failed`) holds the name; only the TypeScript name moves, and the wire JSON of both is unchanged.

`pnpm gen:types` regenerates both directories.
`generated_bindings_are_current`, in `mp-protocol`'s `tests/ts_bindings.rs` and in `src-tauri/src/ts_bindings.rs`, fails when a Rust type changed and the committed files were not regenerated.
A second test fails when a type derives `TS` without being in the export list.
The type blocks in this document are for reading, and the generated files are the contract.

## Commands

| Command | Arguments | Resolves to |
|---|---|---|
| `subscribe_events` | `on_event: Channel<GuiEvent>` | nothing; see the event stream below |
| `connection_status` | none | `ConnectionStatus` |
| `retry_connect` | none | nothing; starts a new connect after a failure |
| `bootstrap` | none | `Bootstrap`, also sent on the channel as `rebootstrapped` with `cause: "requested"` |
| `list_accounts` | none | `AccountInfo[]` |
| `list_mailboxes` | `account` | `MailboxListing` |
| `list_messages` | `account`, `mailbox` (slug) | `MessageList` |
| `message_text` | `account`, `row_id` | `MessageText`: the stored plain text, `body: null` when the store holds none; `not_found` for an unknown row |
| `message_html_meta` | `account`, `row_id` | `MessageMeta` |
| `search_local` | `params: LocalSearchParams` | `LocalSearchHit[]` |
| `search_server_start` | `params: ServerSearchParams` | `{ operation_id }` |
| `search_server_cancel` | `operation_id` | `"cancelled" \| "already_settled"`; the search is no longer awaited |
| `operation_cancel` | `operation_id` | `"cancelled" \| "already_settled"`; the operation stays awaited until its finish (see Cancelling) |
| `message_archive` | `account`, `row_ids` | `MutationBatch` |
| `message_delete` | `account`, `row_ids` | `MutationBatch` |
| `message_move` | `account`, `row_ids`, `destination` (slug or label) | `MutationBatch` |
| `message_set_flag` | `account`, `row_ids`, `flagged` | `MutationBatch` |
| `message_set_read` | `account`, `row_ids`, `read` | `MutationBatch` |
| `draft_discard` | `account`, `ids` | `DraftDiscardBatch` |
| `draft_create` | `account`, `name`, `signature?`, `no_signature?`, `headers?: DraftHeaders`, `body?` | `DraftCreated` |
| `draft_reply` | `account`, `row_id`, `all`, `headers?`, `signature?`, `no_signature?` | `DraftCreated` |
| `draft_forward` | `account`, `row_id`, `headers?`, `signature?`, `no_signature?` | `DraftCreated` |
| `draft_from_message` | `account`, `kind: DraftKind`, `message: DraftMessage` | `DraftCreated` |
| `draft_path` | `account`, `id` | `DraftLocation` |
| `draft_approve` | `account`, `ids` | `DraftStatusBatch` |
| `draft_demote` | `account`, `ids` | `DraftStatusBatch` |
| `draft_validate` | `account`, `id` | `DraftValidation` |
| `draft_preview` | `account`, `id` | `DraftPreview` |
| `draft_set_recipients` | `account`, `id`, `to`, `cc`, `bcc`, `subject?` | `DraftLocation` |
| `signature_list` | `account` | `SignatureListing` |
| `signature_read` | `name` | `SignatureFile` |
| `signature_create` | `name` | `SignatureFile` |
| `signature_rename` | `account`, `old`, `new` | `SignatureListing` |
| `signature_delete` | `account`, `name` | `SignatureListing` |
| `signature_set_default` | `account`, `name` (or `null` to clear) | `SignatureListing` |
| `editor_open` | `path` | `EditorLaunch` |
| `editor_setting_get` | none | `EditorSetting`, with the route a draft takes |
| `editor_setting_set` | `editor` (or `null` to clear) | `EditorSetting` |
| `terminal_spawn` | `account`, `id`, `path`, `cols`, `rows`, `theme` (`dark` or `light`), `output: Channel` | `TerminalStarted`; see Terminal sessions |
| `terminal_write` | `session`, `data` (a string) | nothing, once the bytes are queued |
| `terminal_resize` | `session`, `cols`, `rows` | nothing |
| `terminal_kill` | `session` | nothing, once the exit frame has gone; an unknown session is fine |
| `setting_get` | `key` (`SettingKey`) | `string \| null`; an unknown key is `not_found` |
| `setting_set` | `key` (`SettingKey`), `value` (or `null` to remove) | `string \| null`, the value the key holds afterwards; an unknown key is `not_found`, a value the key cannot hold `setup` |
| `config_open` | none | `EditorLaunch`; `not_found` when the daemon has no `config.toml` |
| `log_open` | none | `EditorLaunch`; `not_found` when the daemon's log file does not exist yet |
| `config_get` | none | `ConfigSnapshot` |
| `config_reload` | none | `ConfigSwap`; a file that does not load is `protocol` with code -32007 and the daemon's sentence |
| `config_set_password` | `account`, `kind` (`"smtp"` or `"imap"`), `value` | `SecretStored` |
| `config_add_account` | `account` (`AccountDraft`) | `ConfigSwap`; a refusal is the daemon's sentence |
| `config_init` | `account` (`AccountDraft`) | `ConfigInitialised`; refused where a `config.toml` exists |
| `config_oauth2_login` | `account` | `OperationStarted`, awaited as `oauth2_login` |
| `send_hold_status` | `account` (or `null` for every account) | `HoldListing` |
| `send_cancel_hold` | `operation_id` | `HoldCancelled` |
| `send_draft` | `account`, `id`, `hold` | `SendStarted`; rejects with a `SendRefusal` |
| `send_approved` | `account`, `hold` | `SendStarted` |
| `outbox_list` | `account` | `OutboxListing` |
| `outbox_retry` | `account`, `row_id` | `{ operation_id }` |
| `outbox_discard` | `account`, `row_id` | `OutboxDiscarded` |
| `message_fetch` | `account`, `mailbox` (label or server name), `message_id` | `FetchOutcome`, once the fetch has ended |
| `attachment_open` | `account`, `row_id`, `part` | `OpenedFile` |
| `attachment_save` | `account`, `row_id`, `parts`, `dest_dir` (absolute or `~`) | `SavedAttachments` |
| `html_open` | `account`, `row_id` | `OpenedFile`, or `null` for a message with no HTML part |
| `hit_html_open` | `html` | `OpenedFile` |
| `draft_attachments` | `account`, `id` | `DraftAttachments` |
| `draft_attach` | `account`, `id`, `path` (absolute or `~`) | `DraftAttachments` |
| `draft_attachment_remove` | `account`, `id`, `index` | `DraftAttachments` |
| `draft_attachment_open` | `account`, `id`, `index` | `OpenedFile` |
| `calendar_events` | `account` | `AgendaEvent[]`, every row, past ones included |
| `invite_source_open` | `account`, `row_id` (an agenda row's) | `EditorLaunch`; `not_found` when the row has no `invite.ics` |
| `invite_get` | `account`, `row_id` | `EventFrontmatter`, or `null` for a message with no invitation |
| `calendar_rsvp` | `account`, `row_id`, `response: "accept" \| "tentative" \| "decline"` | `{ operation_id }` |
| `invite_refusal` | `account` | `InviteRefusal` |
| `send_invite` | `account`, `subject`, `start`, `to?`, `cc?`, `end?`, `duration?`, `location?`, `description?` | `{ operation_id }` |
| `contact_search` | `account`, `query` (empty for the whole index), `limit` | `ContactSearch` |
| `contact_rebuild` | `account` | `{ operation_id }` |
| `contact_vcard_draft` | `account`, `name` (the new draft's file name), `address`, `display_name` | `VcardDraft` |
| `sync_trigger` | `account`, `mode: "quick" \| "full"` | `{ operation_id }` |
| `restart_daemon` | none | nothing; runs `mp daemon restart` (fixture mode: simulates one) |
| `intercepted_urls` | none | `InterceptedUrl[]`, and the log is cleared |
| `open_external` | `url` (http, https or mailto) | nothing; opens it in the default handler |
| `version_info` | none | `VersionInfo` |
| `fixture_simulate` | `what` | nothing; fixture mode only |
| `update_check` | `manual` (boolean) | `UpdateCheck`; never rejects (see Updates) |
| `update_status` | none | `UpdateStatus`; reads managed state and `update-state.json`, never the network |
| `update_skip` | `version` | nothing; the silent check stops offering that version |
| `update_install` | `on_progress: Channel<UpdateProgress>` | nothing, once the new bundle is in place; rejects with a string |
| `update_restart` | none | nothing; stops the daemon and relaunches into the installed version |

```ts
type ConnectError = {
  kind: "unavailable" | "version_mismatch" | "identity_mismatch";
  why: string; socket: string; log: string; daemon_version: string | null;
};
type ConnectionStatus =
  | { state: "connecting" }
  | { state: "connected"; instance_id: string; daemon_version: string; protocol: number; fixture: boolean }
  | { state: "reconnecting"; reason: string; last_error: ConnectError | null }
  | { state: "failed"; error: ConnectError };

type AccountState = "opening" | "ready" | "blocked";
type SyncHealthState = "unknown" | "ok" | "failed";
type AccountInfo = {
  name: string; default: boolean; backend: string; store_state: string;
  runtime_state: AccountState; sync_health: SyncHealthState;
  outbox: { queued: number; failed: number };
};

type MailboxKind = "inbox" | "drafts" | "sent" | "archive" | "extra";
type MailboxInfo = { slug: string; label: string; role: string; kind: MailboxKind; total: number; unread: number; badge: number };
type MailboxListing = {
  account: string; mailboxes: MailboxInfo[]; total: number; unread: number;
  runtime_state: AccountState; sync_health: SyncHealthState;
};

type MessageList =
  | { kind: "messages"; account: string; mailbox: string; total: number; rows: MessageListRow[] }
  | { kind: "drafts"; account: string; listing: DraftListing };

type MessageText = { account: string; row_id: number; body: string | null };
type MessageMeta = {
  row_id: number; html_url: string; selector: string; account: string; mailbox: string;
  message_id: string; from: string | null; to: string | null; cc: string | null;
  subject: string | null; date: string | null;
  flags: string[]; invite: boolean; attachments: { name: string; size: number }[];
};

type LocalSearchParams = {
  account: string; query: string; mailbox?: string; limit?: number;
  from?: string; to?: string; cc?: string; subject?: string; body_query?: string;
  filename?: string; has_attachment?: boolean; after?: string; before?: string;
};
type LocalSearchHit = MessageListRow & { mailbox: string };
type ServerSearchParams = {
  account: string; query: string; mailboxes?: string[]; limit?: number; exclude_message_ids?: string[];
};

type MutationAck = {
  row_id: number; account: string; id: string; selector: string; mailbox: string;
  moved_to?: { mailbox: string; selector: string }; read?: boolean; flagged?: boolean;
};
type MutationBatch = { done: MutationAck[]; failed: { row_id: number; error: GuiError }[] };
type DraftDiscarded = { account: string; id: string; selector: string; status: string };
type DraftDiscardBatch = { done: DraftDiscarded[]; failed: { id: string; error: GuiError }[] };
type HoldCancelled = { cancelled: boolean; operation_id: string; revision: number };
type SendStarted = { operation_id: string; held: boolean; approved: boolean };
type SendRefusal = GuiError & { invalid?: DraftInvalid };

type DraftHeaders = { to: string; cc: string; bcc: string; subject: string };
type DraftStatusChanged = { account: string; id: string; status: string; path: string };
type DraftStatusBatch = {
  done: DraftStatusChanged[];
  failed: { id: string; error: GuiError; invalid?: DraftInvalid }[];
};
type SignatureListing = { account: string; names: string[]; default: string | null };
type EditorSource = "env" | "setting" | "visual" | "editor" | "terminal" | "probe" | "fallback";
type EditorLaunch = { editor: string; pid?: number; source: EditorSource; fixture: boolean };
type EditorRoute = "embedded" | "external";
type EditorSetting = {
  editor: string | null; file: string; env_override: string | null;
  effective: string; effective_source: EditorSource; route: EditorRoute;
};
type SyncMode = "quick" | "full";
type RsvpSettled = {
  account: string; selector: string; response: string; subject: string;
  organizer: string; message_id: string; delivered: boolean;
};
type InviteRefusal = { account: string; refusal: string | null };
type ContactRow = {
  address: string; display_name: string; sent_to: number; sent_cc: number; received: number;
  score: number; recipient: string;
};
type ContactSearch = { account: string; query: string; contacts: ContactRow[] };
type ContactRebuilt = { account: string; contacts: number; kept: number; saved: string; cache_path: string };
type VcardDraft = { draft: DraftCreated; vcf: string };

type InterceptedUrl = { url: string; at: number; source: "navigation" | "new_window" | "open_external_stub" };
type VersionInfo = {
  app_version: string; protocol_min: number; protocol_max: number; fixture: boolean;
  daemon: { daemon_version: string; protocol: number; instance_id: string } | null;
};
```

A `MessageMeta` header is `null` when the message did not carry it, where a `MessageListRow` of the same message has `""`.
`AccountInfo.runtime_state`, `sync_health` and `outbox` and `MailboxListing.runtime_state` and `sync_health` come from the latest bootstrap; later changes arrive as events.
`HoldListing` is the protocol's `{ holds: HoldStatus[] }`.
`DraftCreated`, `DraftLocation`, `DraftValidation`, `DraftPreview`, `DraftKind`, `DraftMessage` and `DraftInvalid` are the protocol's own types, and so are `SendOutcome`, `RecipientOutcome` and `ApprovedOutcome`, which a send's `result` carries.

## Mutations

The five message commands send one daemon call per row id, in the order given, each with `settle: false`, the TUI's contract.
The daemon commits the row change and the server op it owes in one transaction and answers at once; it drains the queue once the account's mutations have been quiet for 1.5 s.
`MutationAck` is the daemon's inline answer plus the `row_id` the call named; `id` and `selector` name the message before the mutation, and `moved_to` where an archive or a move put it.
`moved_to.selector` carries the stored Message-ID with its angle brackets percent-encoded (`mp://work/archive/%3C…%3E`), as the daemon answers, and so differs from the moved row's own `selector`, which carries the bare Message-ID.
A row id is an id in the named account's store, and the daemon acts on whichever row of that account holds it.
Keeping each row id paired with the account it was listed from is the frontend's job; the fixture refuses an id it does not hold in that account, and nothing in the GUI relies on that.
A row the daemon refused with `-32602` (no such row in that account, or a destination it cannot move to) lands in `failed` with its `GuiError`, and the next row goes ahead.
An error about the whole batch (no daemon, an unknown account, an account not ready, a store failure, a timeout) rejects the command when no row was done yet, and otherwise stops the batch and fills `failed` with every row not done, so the answer always says which rows changed.
`draft_discard` follows the same rules over draft ids and never forces: an approved draft is refused.
The daemon has no undo for any of them; a server refusal arrives later as `mutations.rolled_back { account, failed }`, which names no row, so the frontend re-reads the account's lists.

A mutation publishes no event of its own.
Once the drain runs, each mailbox whose counts moved gets a `state.invalidate` with scope `{ query: "counts" }`; a flag change moves no count and so publishes nothing.
`draft_discard` is followed by `state.remove` for `draft:<account>/<id>`.

`draft_approve` and `draft_demote` follow the same batch rules, and a draft whose file does not parse (`-32010` `draft_invalid`) also fails alone.
Its failure carries `invalid`, the `draft.invalid` payload the daemon sends as the refusal's `data`, the file and the parser's diagnostics, which `mp_client::session::refusal` reads off the call's error.

`send_cancel_hold` stops a hold whichever client armed it; a hold that already fired, or never existed, is `not_found`.
The countdown itself comes from the bootstrap's `holds` and the `send.hold_started`, `send.hold_tick`, `send.hold_fired` and `send.hold_cancelled` events, each carrying one `HoldStatus` with the daemon's `remaining_secs`.
`sync_trigger` starts `sync.quick` or `sync.full` and awaits it like a server search (`kind: "sync"`); the pass publishes `sync.completed` to every client before its `operation.finished`.

## Sends

`send_draft` is the TUI's `x` (`SND-03`, `clients/tui/src/actions.rs`, `Action::Send` and `validate_then_approve`), in this order:

1. `draft.list`, a fresh scan, for the draft's status;
2. `draft.validate` of the listed draft, and a report that is not valid stops here with a `protocol` refusal naming the reason, so the draft keeps its status;
3. `draft.approve` when the status is anything but `approved`, including a draft the listing does not show, whose refusal says why;
4. `send.draft {account, id, hold}`, awaited as `kind: "send"`.

A refused approve stops the send.
Its `SendRefusal` is the `GuiError`, and for a file that does not parse (`-32010` `draft_invalid`) `invalid` carries the `draft.invalid` payload, the refusal's `data`, as `draft_approve` does.
A `send.draft` the daemon refuses leaves the approval in place, as the TUI's does, and `approved` in the answer says whether this call approved the draft.
`send_approved` starts `send.approved {account, hold}`, awaited as `kind: "send_approved"`.

The frontend passes `hold: true` always, as the TUI does: the window is the daemon's `email.send_hold_secs`, and `0` arms none.
`held` in the answer is the daemon's `held`; with a hold, `send.hold_started` names the operation, and the countdown and the cancel are the hold machinery above.
A cancelled hold cancels the operation too, which then ends `cancelled`.

| Kind | Started by | Awaits | `result` on success |
|---|---|---|---|
| `server_search` | `search_server_start` | `message.search_server` | the search summary |
| `sync` | `sync_trigger` | `sync.quick`, `sync.full` | the sync outcome |
| `send` | `send_draft` | `send.draft` | `SendOutcome` |
| `send_approved` | `send_approved` | `send.approved` | `ApprovedOutcome` |
| `outbox_retry` | `outbox_retry` | `send.outbox_retry` | `OutboxRetryOutcome` |
| `rsvp` | `calendar_rsvp` | `calendar.rsvp` | `RsvpSettled` |
| `send_invite` | `send_invite` | `send.invite` | `SendOutcome` |
| `contact_rebuild` | `contact_rebuild` | `contact.rebuild` | `ContactRebuilt` |

Each ends one of three ways, as the event stream below says: `operation.finished` with `{operation_id, state, result?, error?}`, `operation_settled` with the whole `OperationStatus` after a re-query, or `operation_dropped` when the daemon restarted.
A send's `state` is `succeeded` once the submission ran, with each recipient's verdict in `recipients`, `failed` with the transport's error, or `cancelled`; a `succeeded` send every recipient refused is a failure to show.

### Cancelling

`operation_cancel {operation_id}` is `operation.cancel` for any operation the layer awaits: the activity area's Cancel of a contact rebuild, an RSVP and an invitation send, and the device-code dialog's Cancel of a sign-in ([shell.md](shell.md), "Actions, dialogs and the activity area").
The operation stays awaited, so its `operation.finished` with `state: "cancelled"` reaches the webview and ends it like any other end.
An id the daemon refuses with `-32602`, one that already ended or that it forgot, answers `already_settled`: its own end has come or will come, and says how it went.
`search_server_cancel` is the same call followed by `forget_operation`, since the search view ends the search itself on the answer; a held send is cancelled with `send_cancel_hold`, not with this.

The daemon settles a cancelled operation at once, but `contact.rebuild`, `calendar.rsvp` and `send.invite` never look at their token after the start: the rebuild may still write its index, and a reply or an invitation already on its way may still go out.
The frontend therefore words a cancelled end as having stopped waiting, never as undone (shell.md, "Actions, dialogs and the activity area").

## The outbox

`outbox_list` is `mp outbox list`: `send.outbox_list {account}`, answered as an `OutboxListing` with the account's unfinished rows in id order and the counts `open`, `failed` and `partial`.
An account that never queued anything answers `ever_used: false`, which is a fact and not an error.
An `OutboxRow` carries its `id`, its `state` as the store spells it (`pending_send`, `failed`, `sent_pending_append`, and `done` for a row listed only because it is `partial`), `never_submitted`, `message_id`, `target_mailbox`, `updated` (a unix timestamp), `last_error` (the partial-delivery note for a partial row), `rejected` as `[address, reason]` pairs, and the `outstanding` addresses.

`outbox_retry` is `mp outbox retry`: `send.outbox_retry {account, row_id}` is an operation, since it re-arms the row and then submits it and files its Sent copy over the network, and the layer awaits it as `kind: "outbox_retry"`.
The daemon admits a `failed` row, which it re-arms, and a `sent_pending_append` row whose APPEND was already attempted, which it re-drives behind a `Message-ID` search; anything else is refused with `-32602` before an operation starts, as a `protocol` error.
Its `OutboxRetryOutcome` is `{row_id, state, completed}`: the row's state after the drain, `null` when it finished and is gone, and how many Sent copies were filed on the way.
A transport that fails again still settles `succeeded`, with `state: "failed"`.

`outbox_discard` is `mp outbox discard`: `send.outbox_discard {account, row_id}` is one committed command, answered at once.

```ts
type OutboxDiscarded = { discarded: boolean; row_id: number; message_id: string; revision: number };
```

A row that is gone is `not_found` with code `-32602`.
Neither command publishes anything of its own: the daemon's `state.invalidate` of `outbox:<account>` follows every change to the outbox.

## The calendar

`calendar_events` is `calendar.events {account}`, read through `mp_client::queries::calendar_events`: the account's agenda, deduped, reply-folded and sorted by the daemon, undated rows last.
An `AgendaEvent` is the protocol's own type: `row_id` (the winning copy's), the `event` block (`EventFrontmatter`: method, sequence, summary, start and end in RFC3339, location, organizer, the user's own `rsvp`, `recurrence`, `attendees`, and the derived `cancelled`, `superseded` and `cancelled_instances`), `subject`, the UTC sort keys `start_sort` and `end_sort` (empty when unknown), `start_display` in the daemon's local time, `is_organizer` and `cancelled`.
The layer passes every row: the past/upcoming filter is the frontend's, as it is the TUI's.
An account whose store is not ready is refused with `protocol` code `-32006`, and an unknown one is `not_found` with `-32005`.

`invite_source_open` is the TUI's agenda Enter and `e` (`Action::OpenEventSource`).
It reads the row's `invite.ics` with `message.ics {account, row_id}` (`mp_client::queries::message_ics`, which decodes the base64 the wire carries), writes it to `renditions/invite-<account>-<row_id>.ics` under the app's cache directory, the directory 0700 and the file 0600, and opens that file with the external editor as `editor_open` does.
The file is a copy to read: an edit to it reaches nothing.
A row with no `invite.ics` is `not_found` with the TUI's sentence, "That event has no ics source in the store", and nothing is written or opened.

`invite_get` is `message.invite {account, row_id}` through `mp_client::queries::message_invite`: the `EventFrontmatter` a message carries, the reader's invitation card, or `null` for a row with no iMIP payload or one that does not parse.
A row the account does not hold is `not_found` with `-32602`.

`calendar_rsvp` starts `calendar.rsvp {account, row_id, response}` and awaits it as `kind: "rsvp"`.
A response other than `accept`, `tentative` or `decline` is a `protocol` refusal before any call.
A daemon refusal of the start comes back as the daemon's own sentence with its code, as `send_invite`'s does.
The reply goes through the daemon's durable outbox, so a transport that fails leaves an outbox row the outbox view shows.
`RsvpSettled` is the daemon's inline settle: `subject` is the reply's own (`Accepted: <summary>`), `organizer` whom it went to, and `delivered` whether any recipient took it; a reply nobody took yet waits in the outbox.
The daemon refuses nothing about the invitation itself: an RSVP to the user's own, a cancelled or a superseded invitation is the frontend's to prevent, as it is the TUI's.

`invite_refusal` answers whether an account can reply to or send invitations at all.
The daemon refuses both on a Graph account (`ANO-4`) before it looks at anything else, so the layer reads `account.list`, and for an account whose `backend` is `graph` it calls `calendar.rsvp {account}` alone.
That call is refused before an operation id exists, and `refusal` is the daemon's sentence, taken off the refusal text by `error::refusal_sentence` (the session keeps a refusal's `data` too, behind `mp_client::session::refusal`, and this one carries none a client needs).
An `imap` account answers `refusal: null` without that call, and an unknown one is `not_found`.

`send_invite` is `mp send --invite`: `send.invite` builds the `VEVENT` and the iMIP message and submits it through the durable outbox, awaited as `kind: "send_invite"` with a `SendOutcome`.
The layer sends only the fields that are not empty, and never `uid`, `signature` or a hold: the daemon mints the UID, and `send.invite` takes no hold.
The daemon's refusals are the CLI's (`--invite requires --subject (used as the event summary)`), so the three a form can check are checked before the call, in the form's words and in the daemon's order: "An invitation needs a subject", "An invitation needs a start", "An invitation needs at least one recipient in To or Cc".
Any other refusal, the Graph one first of all, comes back as a `protocol` error whose message is the daemon's sentence alone, with its code.

## The contacts

`contact_search` is `contact.search {account, query, limit}`: the account's contacts a query fuzzy-matches, best first, or for an empty query the whole index in rank order (sent To, then Cc, then received, then recency), at most `limit`.
The Contacts view asks with a `limit` of 1000, where the TUI lists the whole index from its cache file.
Each row is the daemon's inline `{address, display_name, sent_to, sent_cc, received, score}` plus `recipient`, which the layer formats with `mp_core::addresses::format_recipient`: `Name <address>`, the name quoted when it holds a character outside atext and spaces (`"Doe, Jane" <jane@example.com>`), or the bare address when no message named one.
The webview puts `recipient` in To as it is and never quotes a name itself.
`score` is the fuzzy match score of a query; every row of an empty query carries `4294967295` (`u32::MAX`), which the view does not show.
With no cache on disk the daemon builds the index inside the query, which walks the whole store, so a first search can take seconds; a rebuild the cache guard refuses there is only logged by the daemon.
An account whose store is not ready is refused with `protocol` code `-32006`, and an unknown one is `not_found` with `-32005`.

`contact_rebuild` starts `contact.rebuild {account}` and awaits it as `kind: "contact_rebuild"`.
The daemon reports one `operation.progress`, phase `contacts` with the account as its message, which reaches the webview because the operation is awaited.
`ContactRebuilt` is the daemon's inline settle: `contacts` the rebuild found, and `saved`, which is `written`, or `refused_empty` or `refused_shrunk` when the cache guard (#0067) kept the `kept` contacts it had instead.
No event says that an index changed, so the frontend reads the list again after a written rebuild and on every open of the view.

`contact_vcard_draft` is the TUI's `v`, done client-side since no daemon method exports a vCard:

1. `draft.create` of `name` with the headers `to: recipient` and `subject: "Contact: <name>"` (the display name, else the address's local part) and `no_signature`, the TUI's vCard draft carrying none;
2. `mp_core::contacts::contact_to_vcard` of the contact into `_vcards/` beside the new draft, named by `vcard_file_stem` (`doe-jane.vcf`), then `-1`, `-2` and on while the name is taken, the TUI's rule;
3. the `.vcf`'s absolute path appended to the draft's `attachments:` through `draft.attach`.

It answers the `DraftCreated` and the `.vcf`'s path, and opens nothing: the frontend hands the draft to the editor as a new draft's.
An empty address is a `protocol` refusal before any call.

## Signatures

A signature is the file `<config_dir>/signatures/<name>.md`, shared by every account, and each account's default lives in the app state file.
The daemon serves no `signature.*` method, so the signature commands call `mp_core::signatures` over a daemon door, as the TUI does, from the desktop's own config directory, which the handshake already takes to be the daemon's (`Identity.config_dir`).

```ts
type SignatureListing = { account: string; names: string[]; default: string | null };
type SignatureFile = { name: string; path: string; content: string };
```

- `signature_list` lists every valid name, sorted, and the account's default when its file still exists, through the daemon's `signature.list`.
- `signature_read` answers the file's path and content.
- `signature_create` writes an empty file and answers it; the frontend opens `path` in the editor next, as the TUI's `n` does.
- `signature_rename` moves the file and points every account default that named it at the new name.
- `signature_delete` removes the file and clears every default that named it.
- `signature_set_default` records the account's default, or clears it for `name: null`.

The last three answer the account's listing after the change.
No command writes content: the editor does, through `editor_open` on the file.
A refusal is `mp_core`'s sentence as it stands ("signature name '../x' cannot start with a dot", "a signature named 'work' already exists"), a `protocol` error with no code, or `not_found` for "no signature named '<name>'".

The daemon's watcher publishes `signature.changed {name, path}` when a signature file is written or created, including by this layer, and nothing when one is deleted, since no `signature.removed` exists.
So the frontend reads the listing again after each change it makes, and on every `signature.changed` while the Signatures dialog or the new-draft wizard is open ([shell.md](shell.md), "Signatures").

## config.toml and the daemon log

`config_open` is the TUI's `sc` and `log_open` its `sf` (INT-01, INT-02), in `daemon_files.rs`.
Neither computes a path: the daemon's answer names the file it uses.

- `config_open` calls `config.get` and opens its `path`; a `state` of `absent` is refused before any editor starts, as `not_found` "There is no config.toml yet; add an account first", and an `invalid` file opens, since the editor is where it gets fixed.
- `log_open` calls `diagnostic.log_path`, the daemon's dated log (`mailypoppins-<date>.log`), and refuses a file that does not exist yet with `not_found` "No log file found at <path>".

Both hand the path to `editor::open_on`, the resolver and spawn `editor_open` uses, with the setting read through `editor::read_setting_or_none`, so a fixture journals the editor and runs nothing; its refusal of a path that is no file now says "no file at <path>".
Neither is a draft, so neither takes the embedded route: a terminal editor in the setting, such as the `nvim` that runs drafts embedded, opens the file in a terminal window (see Drafts and the editor).
Both methods are in `REQUIRED_CAPABILITIES`.

## Configuration and secrets

`configuration.rs` serves the Settings view and the account wizard (ACC-01, ACC-02, ACC-05, ACC-06) with six daemon methods, all in `REQUIRED_CAPABILITIES`: `config.get`, `config.reload`, `config.set_password`, `config.init`, `config.add_account` and `config.oauth2_login`.
The daemon has no per-key writer, since a tenth `config.*` method is pinned shut, so a setting changes in `config.toml` and reaches the daemon through `config_reload`.

```ts
type ConfigSnapshot = { revision: number; path: string; state: "ok" | "absent" | "invalid"; config: EffectiveConfig };
type EffectiveConfig = { secrets_backend: string; email: { send_hold_secs: number }; accounts: ConfigAccount[] };
type ConfigAccount = {
  name: string;
  default_from: string;
  auth_method: string; // password, oauth2 or graph
  smtp: ConfigServer;
  imap: ConfigServer;
  oauth2: { client_id: string; tenant_id: string } | null;
};
type ConfigServer = { host: string; port: number; username: string };
type ConfigSwap = { added: string[]; updated: string[]; removed: string[] };
type SecretKind = "smtp" | "imap";
type SecretStored = { stored: boolean; account: string; kind: string; key: string };
```

`EffectiveConfig` keeps the keys the view shows out of the daemon's whole effective configuration, and every one of them defaults when the daemon leaves it out, so a daemon that adds or drops a key never breaks the decode.
`revision` is the configuration's own counter: 0 at daemon start and one more per swap, whatever the state revision does.

`config_reload` answers what the swap started, restarted and stopped, and the daemon publishes `config.changed` with the same lists.
A file that does not load leaves the daemon on the configuration it had: it publishes `config.invalid {path, line, message}` and refuses with `-32007`, which the command passes on as `protocol` with code -32007 and the daemon's sentence as the message.
The line travels only in the event.

`config_set_password` stores one password through the daemon's secrets backend and answers the backend's key, never the value; the daemon publishes no event for it.
The value crosses this layer once, and these rules keep it there:

- `SetPassword`, the struct that carries it, has a hand-written `Debug` that prints `<redacted>` for the value, as the daemon's `SetPasswordParams` does.
- The `tracing` lines name the account and the kind: "stored the imap password of work", or "storing the smtp password of work failed: <why>".
- A refusal is the daemon's sentence, which names the account and the kind and never the value.
- The fixture journals `{account, kind, value: "<redacted>"}`, and under test its record of every call holds the value redacted too.

`configuration.rs`'s tests check each rule: the `Debug`, a `tracing` subscriber that captures every level, the fixture's journal and call record, and a refusal's `GuiError` in its `Debug` and its JSON.

### Accounts

`config_add_account` appends one `[[accounts]]` block and `config_init` writes a whole file with one; both take an `AccountDraft`, the daemon's `account` parameter.

```ts
type AuthMethod = "password" | "oauth2" | "graph";
type AccountDraft = {
  name: string;
  default_from?: string;
  auth_method?: AuthMethod; // absent for a password account
  oauth2?: { client_id: string; tenant_id: string };
  smtp?: AccountDraftServer;
  imap?: AccountDraftServer;
  mailboxes?: { inbox: string; archive: string; sent: string; extra?: string[] };
};
type AccountDraftServer = { host?: string; port?: number; username?: string; accept_invalid_certs?: boolean };
type ConfigInitialised = { path: string; added: string[]; updated: string[]; removed: string[] };
```

Every key of a draft is one the daemon's `account_block` reads (src/daemon/methods/config.rs), and an absent key is left out of the wire, so the daemon's default applies.
The draft has no password key, and `deny_unknown_fields` refuses one, or any key the daemon would drop, when the webview sends it.
The wizard stores a password afterwards through `config_set_password`, the daemon's one path into the secrets backend.
`config_add_account` refuses a missing `config.toml` ("... write one with config.init first") and a taken name, and `config_init` an existing file ("a configuration already exists at <path>; edit it and call config.reload"), each as the daemon's sentence.
A block that does not load is `-32007`, and the file stays as it was.
Either write publishes `config.changed` naming the account as `added`, and the daemon starts its runtime before it answers.
`ConfigInitialised` is the daemon's flat answer, the swap with the file's `path`.
The tests serialise one draft per preset and hold every key path against `account_block`'s list.

### Sign-in

`config_oauth2_login` starts the device-code flow of an `oauth2` or `graph` account, awaited as `oauth2_login`.
The daemon refuses an unknown account ("Account '<name>' not found in config", `-32005`), a password account and an account with no client or tenant, each with its own sentence.
Its one `operation.progress` has phase `device_code`, `done: 0`, `total: null` and a message of two tokens, the verification URL and the user code, separated by their one space.
It reaches only the window that started the sign-in, as every progress does, and the operation's `operation.status` keeps it as `progress`.
It settles with an `OAuth2Stored`, never the token:

```ts
type OAuth2Stored = { stored: boolean; account: string; kind: string; key: string }; // kind oauth2 or graph, key oauth2-token-<account>
```

The dialog's Cancel is `operation_cancel`, which keeps the sign-in awaited, so its `cancelled` finish ends it the usual way; one already over answers `already_settled`.
The daemon never hands its cancel token to the provider's poll, so a sign-in finished in the browser after a cancel still caches its token.

## Desktop settings

`settings.rs` keeps the desktop's own settings in `desktop.json`, in the app config directory (`~/Library/Application Support/dev.mailypoppins.desktop/` on macOS).
None of them reaches the daemon.
The file is one JSON object, and each setting is a string under its key:

```ts
type SettingKey = "editor" | "theme" | "reader_mode" | "editor_colors";
```

- `editor` is the editor command template (see Drafts and the editor), refused with `setup` when it does not split.
- `theme` is `dark`, `light` or `system`, refused with `setup` otherwise; unset means dark ([shell.md](shell.md), "Settings").
- `reader_mode` is `html` or `text`, refused with `setup` otherwise; unset means html ([reader.md](reader.md), "Text mode").
- `editor_colors` is `app` (Neovim and Vim take the `mailypoppins` colorscheme in the app's palette) or `editor` (their own), refused with `setup` otherwise; unset means app (see Terminal sessions, "The look").
  `terminal_spawn` reads it at each spawn, and a file that does not read counts as `app`, logged.

`setting_get` and `setting_set` read and write one key, and `editor_setting_get` and `editor_setting_set` are the `editor` key with what it resolves to.
A key outside `SettingKey` is `not_found` naming the known keys, and nothing is written.
A missing file, a missing key, a value that is not a string and a blank value all read as `null`.
A write trims the value, removes the key on `null` or a blank value, keeps every other key of the file, unknown ones included, and creates the directory when it is missing.
A file that is not a JSON object is `setup` for every read and write.

## Drafts and the editor

A draft is a Markdown file with YAML frontmatter in the account's drafts directory, and every command that writes one answers its absolute `path`.
`draft_create` takes the file name; a name already taken is refused with `protocol` code `-32602`, and the message names the existing path.
It passes `headers` and a non-blank `body` to `draft.create`, which writes the file whole: the recipients and subject in the frontmatter, the body above the signature.
`draft_reply` and `draft_forward` address the source by `row_id` and pass `headers` to the daemon, which then needs all four fields; an empty string clears one.
They pass `signature` and `no_signature` as `draft_create` does, and neither means the account's default, which the daemon resolves.
`draft_from_message` builds a reply, reply-all or forward from a server-only search hit (`DraftMessage`, the hit's own field names), with no attachments.

`draft_set_recipients` is client-side, like the TUI's `ce`: it resolves the file through `draft.path` and rewrites the `to`, `cc`, `bcc` and `subject` lines with `mp_core::draft::rewrite_draft_recipients`, which leaves the body and every other field byte for byte.
An absent `subject` keeps the draft's own, and the signature is not re-spliced.
`signature_list` calls `signature.list`, which lists the signatures directory through `mp_core::signatures`, as the TUI does; `default` is the account's default whether or not `include_signature` is on (see Signatures).

Every change to a draft file, from the daemon, an editor or a client-side rewrite, reaches the frontend as the watcher's `draft.changed` or `draft.invalid`, and the commands publish nothing of their own.

`editor_open` opens a file in the user's editor and never waits for it to exit, since a `code`-style launcher exits at once while the edit goes on.
The editor is a command template, resolved in this order:

1. `MP_DESKTOP_EDITOR`;
2. the `editor` key of `desktop.json` in the app config directory (`~/Library/Application Support/dev.mailypoppins.desktop/` on macOS), read and written by `editor_setting_get` and `editor_setting_set`;
3. `$VISUAL`, then `$EDITOR`;
4. the first of `code`, `zed`, `subl` and `cursor` found in `/opt/homebrew/bin`, `/usr/local/bin` or `/usr/bin`;
5. `open -t` on macOS, which opens the default text editor, or `xdg-open` elsewhere.

Any of the first three that names a terminal editor (`vi`, `vim`, `nvim`, `hx`, `nano` and a few more) runs inside the first terminal emulator found, and is skipped when there is none.
The wrapped command keeps the source `env` or `setting`, so a failed launch names what to fix, and one from `$VISUAL` or `$EDITOR` has the source `terminal`.
A draft whose editor the embedded route finds never reaches `editor_open` (see Terminal sessions), but `config.toml`, the daemon's log, a signature and an `invite.ics` do: with the setting `nvim` they open in a terminal window, where a bare `nvim` with null stdio would have no terminal at all.

The template is split with shell-words rules and run without a shell.
`{path}` in any word is replaced by the path; a template without it gets the path as its last argument.
An app started from Finder inherits a `PATH` without Homebrew, so a bare program name is looked up on `PATH` and then in the three directories above.
The process gets null stdio and its own process group.
A spawn failure or a nonzero exit within 2 s rejects with `setup`, whose message names the variable or the setting to fix; the command answers when the launcher exits or after 2 s, whichever comes first.

The terminals are probed in this order, each on `PATH`, in the three directories above and, on macOS, as `/Applications/<App>.app/Contents/MacOS/<program>`:

1. Ghostty, as `open -na /Applications/Ghostty.app --args -e <editor> {path}` on macOS, whose `ghostty` binary refuses to start a terminal from the command line, and `ghostty -e <editor> {path}` elsewhere;
2. kitty, as `kitty -- <editor> {path}`;
3. Alacritty, as `alacritty -e <editor> {path}`;
4. WezTerm, as `wezterm start -- <editor> {path}`, last among the emulators since it is in maintenance and slow on macOS;
5. Terminal.app on macOS, found as `/System/Applications/Utilities/Terminal.app`, through `osascript` with a script that runs its arguments in a new window, each in single quotes with every backslash outside them so that sh, bash, zsh and fish all read it as one literal word, and `x-terminal-emulator -e <editor> {path}` elsewhere.

`<editor>` is the template with its own arguments, as in `nvim --clean`, and its program is looked up on `PATH` and in the three directories, since the terminal may not see the shell's `PATH`.
A value that carries its own `{path}` keeps it where it is.
A template whose program is not a terminal editor is taken verbatim, so one that names its terminal itself runs as written, for example `MP_DESKTOP_EDITOR="open -na Ghostty --args -e hx {path}"`.

`EditorSetting.route` says where a draft opens, and the frontend reads it before every open.
It is `embedded` exactly when `terminal_spawn` would accept the editor for an existing draft: `terminal::route` runs the same resolution and lookup as `terminal_spawn` (see Terminal sessions) without the draft file, so the two cannot disagree.
Both read the setting through `editor::read_setting_or_none`, as `editor_open` does: a `desktop.json` that does not read is logged and counts as no setting, so a corrupt file never fails `editor_setting_get` while the spawn goes ahead without it.
Anything else is `external`, and `effective` is what `editor_open` then runs: a GUI editor, a terminal editor found nowhere, or nothing found at all.
In fixture mode a terminal editor found nowhere and an empty slot are `embedded` too, as `terminal_spawn` journals them, and a GUI editor stays `external`.
The route needs the login shell's `PATH`, which the first call per process reads for up to 5 s, so `editor_setting_get` and `editor_setting_set` run off the main thread.

## Terminal sessions

`terminal_spawn` runs a terminal editor on a draft in a native PTY (`portable-pty`), which the webview renders with xterm.js; it is the embedded route of M5 (#0130), beside `editor_open`'s external one.

The editor is resolved before any wrapping in a terminal emulator:

1. `MP_DESKTOP_EDITOR`, then the `editor` setting, taken as they are;
2. else the first of `$VISUAL` and `$EDITOR` that names a terminal editor (`vi`, `vim`, `nvim`, `hx`, `nano` and the rest of `TERMINAL_EDITORS`);
3. else, when none of the four is set, the first of `nvim`, `vim` and `hx` found.

A GUI editor named there (`code -w`, `zed`) is refused with `setup`, naming it and where it came from; so is an explicit choice of one, and a `$VISUAL`/`$EDITOR` pair that names no terminal editor.
A bare program is looked for on the login shell's `PATH`, then in `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`, then in `~/.local/share/bob/nvim-bin`, and runs by its absolute path, so a Finder launch finds it; a program found nowhere is `setup`, and so is nothing found at all.
The login shell's `PATH` and `LANG` come from one `$SHELL -lc` call that prints a mark line, then `$PATH`, then `$LANG`, read after the last mark so whatever the startup files print is skipped; it runs once per process with a 5 s budget, and a shell that fails leaves the process's own `PATH` and no `LANG`, with a warning in the log.

The child runs with the template's words, `{path}` replaced by the draft path or the path appended, in the draft's directory, with the inherited environment plus `PATH` (the login shell's), `TERM=xterm-256color` and `COLORTERM=truecolor`.
When the app's environment has none of `LANG`, `LC_ALL` and `LC_CTYPE`, which is the case under a Finder launch, the child also gets `LANG`: the login shell's, else `en_US.UTF-8`; without it `/usr/bin/vim` runs in latin1 and splits an umlaut on `x` or `r`.
A relative path is `protocol` and a missing file `not_found`, as for `editor_open`; a PTY that does not open is `internal`, naming the OS error.

### The look

`theme` is the palette the webview paints at spawn, `dark` or `light` (a stored `system` is already resolved to one); anything else is `protocol`.
The child always gets `MP_DESKTOP_THEME=<theme>`, for every editor, so a user's own config can follow the app.
When the located program's file name, or that of the file it links to, is `nvim` or `vim` (`/usr/bin/vi` links to `vim` on macOS), these words go in right after the program, before the template's own arguments and the draft path:

```
nvim --cmd "set runtimepath^=<resources>/nvim" -c "set runtimepath^=<resources>/nvim" -c "set background=<theme>" -c "colorscheme mailypoppins" <args> <path>
```

The last two `-c` pairs come only while `editor_colors` is `app`; with `editor` the runtime path alone is added, so `:colorscheme mailypoppins` stays one command away.
Each word is one argv entry, with no shell quoting; the directory is escaped for `:set` (a space, `|`, `"` and a backslash) and its list (a comma).
Some characters in the directory cannot work, escaped or not, and are a known limit: a backslash, `$` (`:set` expands `$NAME` even after `\$`, and the runtime search expands it again), `'`, a backtick, `[...]` and `{...}`, which the runtime search reads as glob syntax.
With one of them the editor still starts, in its own colours, after an "Error in command line" prompt (E185); an app bundle's path holds none of them unless the user renamed a folder on it.
`--cmd` runs before the user's `init.lua` or `vimrc`, so the config itself can load the colorscheme; `-c` runs after the config and after the file is loaded, so it puts the directory back on a runtime path the config reset (lazy.nvim resets it by default, dropping what `--cmd` added) and the app's colours win over the config's colorscheme.
A plugin that sets a colorscheme later (on `VimEnter` or lazily) can still override it; `'runtimepath'` drops a duplicate entry, so the second prepend never doubles the first.
`hx` and every other editor get nothing but the variable.

`<resources>/nvim` is `app.path().resource_dir()` joined with `nvim`: `Contents/Resources/nvim` in the macOS bundle, and under `tauri dev` the executable's directory, `target/debug/nvim`, where tauri-build copies the resources on every build.
`tauri.conf.json` ships `src-tauri/resources/nvim/` as the bundle's `nvim/` resource (`bundle.resources`, the map form, so the path does not keep the `resources/` prefix).
Tauri takes the executable's directory for a dev build only when it sits in a directory named `target`; a `CARGO_TARGET_DIR` named otherwise makes it look for `../Resources`, which is not there, so a debug build falls back to `src-tauri/resources/nvim` in the source tree (`terminal::SOURCE_RUNTIME`).
With neither directory there Neovim and Vim go undressed, with a warning in the log.
The colorscheme, `resources/nvim/colors/mailypoppins.vim`, is Vimscript for Neovim and Vim alike; see [design-tokens.md](design-tokens.md), "Terminal".

```ts
type TerminalStarted = { session: number; pid: number | null; editor: string; source: EditorSource; fixture: boolean };
type TerminalExit = { code: number | null; signal: number | null };
type TerminalExitFrame = { exit: TerminalExit };
```

`output` is a `Channel` whose messages are, in order:

- PTY output as raw bytes (`InvokeResponseBody::Raw`, an `ArrayBuffer` in JavaScript), at most 64 KiB each, sent once 64 KiB is reached or 4 ms after the first unsent byte; a frame may end inside a UTF-8 sequence, so the webview decodes with a streaming `TextDecoder`;
- last, the exit as JSON (`InvokeResponseBody::Json`, an object in JavaScript): `{"exit":{"code":0,"signal":null}}`, `code` when the child exited and `signal` the signal number when one killed it (Unix), both `null` when the status could not be read.

The JavaScript `Channel` replays messages in the order Rust sent them whatever their body, so the exit never overtakes the last output; a frame is told apart by its type, `ArrayBuffer` or object.

`terminal_write` queues the string's UTF-8 bytes, as xterm's `onData` gives them, for the session's writer thread and answers at once, so a child that stops reading (Neovim in a long synchronous command, a `:!cmd`) blocks only that thread, and keystrokes keep their call order.
A write that fails (`EIO` once the child closed the terminal) ends the writer thread, is logged at debug, and drops the rest of the queue; `terminal_write` answers `Ok` for a session that is still in the table, exited or not.
`terminal_resize` sets the PTY's size and is dropped once the child exited; for both, an unknown session is `not_found`.
`terminal_kill` kills the child (SIGHUP, then SIGKILL after 200 ms), waits until the exit frame has gone, and drops the session; the frontend calls it after an exit frame too, which frees the PTY, and a second call is fine.
A child that exited while something it started keeps the PTY open gets its exit frame after 200 ms of quiet.
Every live child is killed when the window is destroyed and when the app exits.
The window's close request kills nothing, since a webview that listens for it decides whether the window closes.
The webview does listen ([shell.md](shell.md), "The embedded editor"): with an editor running it asks, kills the sessions, and destroys the window itself, which the main window's capability allows with `core:window:allow-destroy`.

In fixture mode nothing is spawned: the command is journaled as `editor_open`'s is, so `fixture_simulate("editor_save")` plays against the draft, and the answer has `fixture: true` and no `pid`.
No frame comes until `terminal_kill`, which sends the exit `{code: 0, signal: null}`.
A fixture with no terminal editor installed journals a bare `nvim`, and a GUI editor is refused as it is outside the fixture.

## Attachments

A received message's part and its browser rendition are files the daemon writes, one call each: `message.materialise_attachment {account, row_id, part}` and `message.materialise_html {account, row_id}` answer `{handle, path, name, bytes, expires_at}`, with the file at `<data_dir>/runtime/handles/<handle>/<name>` for ten minutes.
`part` is the zero-based index into `MessageMeta.attachments`, which is `message.get`'s list; the daemon serves no list method of its own.
The layer checks that `name` is one file in one directory (not empty, `.` or `..`, and without `/`, `\` or NUL) and that `path` is absolute and ends in `name`, before it opens or copies anything.

`attachment_open` and `html_open` hand the daemon's file to the system opener and leave the handle unreleased, since the viewer just launched holds the file (`ATT-01`, `ATT-05`).
The opener is `mp_core::parse::open_file_with_system`, the TUI's: `open <path>` on macOS and `xdg-open <path>` elsewhere, spawned with null stdio and reaped on a thread.
A fixture door journals the path instead, and `MP_DESKTOP_STUB_OPENER=1` logs `[open] stubbed: <path>` and opens nothing.
The rendition carries the charset, the `Content-Security-Policy` tag and the `cid:` images as `data:` URIs, all three written by the daemon.
A message whose sender wrote no markup is the daemon's `-32602`, which `html_open` answers as `null` and the frontend shows as "No HTML version available", the TUI's line; an unknown row reads the same, as it does in the TUI.

`attachment_save` copies each of `parts`, in the order given, into `dest_dir`, then releases the part's handle (`ATT-02`).
`dest_dir` is typed by the user: `~` and `~/…` expand against `$HOME`, a relative path is refused with `protocol`, since the app has no working directory a user could mean, and a missing directory is created.
The copy is `mp_core::parse::save_attachment`, so a name the directory already has becomes `name_1.ext`, then `name_2.ext`.
A part that fails lands in `failed` with its `GuiError`, and the others still go.

```ts
type OpenedFile = { name: string; path: string };
type SavedAttachments = {
  dir: string;
  saved: { part: number; name: string; path: string }[];
  failed: { part: number; error: GuiError }[];
};
```

A server-only search hit has no row, so there is nothing to materialise; its markup is the hit's `html_body`.
`hit_html_open` writes it with `mp_core::parse::ensure_utf8_charset` and `inject_csp_meta`, as the TUI's `html_temp_file` does, to `<app cache>/renditions/hit-<hash>/message.html`, one directory per markup, and opens it.
The app cache is Tauri's `app_cache_dir`, `~/Library/Caches/dev.mailypoppins.desktop/` on macOS; the fixture uses `cache/` under its run directory.
On unix `renditions/` and each `hit-<hash>/` are 0700, tightened if found wider (`mp_core::config::create_private_dir_all`), and `message.html` is 0600, as the daemon's handles and the TUI's temp files are.
Each write first removes the renditions older than a day.

A draft's attachments are the paths its `attachments:` frontmatter lists, and the daemon reads and rewrites them (#0131), so no command here writes the file:

- `draft_attachments` is `draft.attachments`: each entry as typed, where the send path finds it, and whether a file is there: `~` expands against the daemon's `$HOME`, and a relative entry resolves against the draft's own directory (`ATT-03`).
- `draft_attach` is `draft.attach`, the TUI's `ta`: `mp_core::draft::attach_checked` keeps the body and every other line byte for byte and stores the path as typed, `~` included.
  A blank path is refused here, before any call; the daemon refuses a relative path, a directory, a path with no file behind it ("No such file: <path>", the TUI's words) and a file the list already names ("<entry> is already attached"), each a `protocol` error carrying the daemon's sentence without the refusal's frame.
- `draft_attachment_remove` is `draft.detach`: item `index` of the block list goes and the file it named stays.
  `mp_core::draft::remove_draft_attachment` first checks that the list has one line per parsed entry, so a flow-style list or an entry that spans lines is refused rather than rewritten; an emptied list keeps its bare `attachments:` key, the skeleton's shape.
- `draft_attachment_open` opens entry `index` where `draft.attachments` resolved it, with the opener (`ATT-04`), and a missing file is `not_found`.

```ts
type DraftAttachments = {
  account: string; id: string; path: string;
  attachments: { index: number; entry: string; path: string; exists: boolean }[];
};
```

A write reaches the frontend as the watcher's `draft.changed`, like every other draft write.
An editor open on the same file can overwrite the change with its own buffer, as it can in the TUI.

`message_fetch` is the TUI search overlay's `f` (`LST-09`): `message.fetch {account, mailbox, message_id}` ingests a server-only message.
It is an operation, and one message is quick, so the command blocks on its end (`SessionHandle::await_operation`) and answers with its result; the frontend awaits one promise, and no `PendingKind` is registered.
The start is registered under the pump lock, so its `operation.finished` cannot overtake it, and the pump hands that payload to the waiting command instead of the channel; a re-bootstrap settles it from `operation.status`, and a daemon restart ends it as `daemon_unavailable`.
A fetch still running after 90 s is `timeout`, a `failed` operation is `protocol` with the daemon's reason, and a bad mailbox is `-32602` at the call.
A message the store already holds answers at once with `already_present: true`.
The new row reaches the lists through the counts `state.invalidate` the daemon publishes for its mailbox.

```ts
type FetchOutcome = { account: string; mailbox: string; uid: number; row_id: number; selector: string; already_present: boolean };
```

## The event stream

`subscribe_events` registers one `Channel`; a second call replaces the first.
The command claims a subscription number on the IPC thread, in call order, and attaches off it under the pump lock; a claim a later call superseded is dropped, so under React StrictMode's double mount the last call is the sink whichever attach runs first.
The channel first carries a `connection` event and, once connected, a `rebootstrapped` with `cause: "subscribed"`, taken while events are held back, so the frontend's model starts complete and every later event applies on top of it.

```ts
type BootstrapCause = "initial" | "subscribed" | "requested" | "resync" | "reconnected" | "instance_changed";
type PendingKind = "server_search" | "sync" | "send" | "send_approved" | "outbox_retry" | "rsvp" | "send_invite" | "contact_rebuild" | "oauth2_login";
type GuiEvent =
  | { type: "event"; event: { instance_id: string; revision: number; kind: string; payload: unknown } }
  | { type: "resync"; instance_id: string; reason: string }
  | { type: "disconnected"; reason: string }
  | { type: "reconnected"; instance_id: string }
  | { type: "rebootstrapped"; cause: BootstrapCause; bootstrap: Bootstrap }
  | { type: "operation_settled"; operation_id: string; kind: PendingKind; status: OperationStatus }
  | { type: "operation_dropped"; operation_id: string; kind: PendingKind; reason: string }
  | { type: "connection"; status: ConnectionStatus }
  | { type: "link_intercepted"; url: InterceptedUrl };
```

`event` carries the daemon's envelope verbatim and only when it applied above the watermark; duplicates are dropped in Rust.
`rebootstrapped` replaces the whole model: restore selection, focus and scroll by stable identifiers (account name, mailbox slug, `message_id` or `selector`, never `row_id` across a daemon restart).
A server search streams `message.server_hit` events and ends with `operation.finished`, both carrying its `operation_id`; a finish lost to a resync or a reconnect arrives as `operation_settled` instead, and a daemon restart turns every running search into `operation_dropped`.
A sync started by `sync_trigger`, a send started by `send_draft` or `send_approved`, a retry started by `outbox_retry`, an RSVP, an invitation, a contact index rebuild and a sign-in end the same three ways.
A hit, a progress report or a finish for an operation this layer no longer awaits (another window's, a cancelled one, or one a re-bootstrap already settled) is dropped in Rust, so nothing about an operation follows its `operation_settled`, `operation_dropped` or `operation.finished`.
`operation.progress` reaches the webview only for an operation this window awaits, so another client's contacts rebuild or sign-in code never shows here.
A progress report leaves its operation awaited; only the finish, a settle or a drop ends the wait.
The frontend keeps each awaited operation's last report (`progress` in the model, by `operation_id`) until the operation ends ([shell.md](shell.md), "Model").

## The reader

`message_html_meta` answers the headers and `html_url`, `mpmsg://localhost/<account>/<row_id>`.
The iframe loads that URL with `sandbox="allow-popups"` and no `allow-scripts`; the plan's section "Reading HTML bodies" explains both.
The frontend side, the refused-link notice and the guard's verification in a real webview are in [reader.md](reader.md).
The header is always `MESSAGE_CSP` and never a policy read out of the message: a sender can hide a meta-looking policy inside the doctype, ahead of the daemon's tag, and a header may carry `report-uri`.
The scheme answers:

- 200 with the rendition, the daemon's policy (`reader::MESSAGE_CSP`) as a `Content-Security-Policy` header, `X-Content-Type-Options: nosniff` and `X-Mp-Rendition: html`;
- 200 with the stored plain text in a minimal document and `X-Mp-Rendition: text` when the message has no markup, read through `message_text_on`;
- 404 for an unknown account or row, 400 for a malformed URL, 503 while no daemon answers, 504 on a timeout.

A rendition over 8 MiB goes through `message.materialise_html`, and the handle is released as soon as the file is read.

`message_text` answers the reader's text mode: `message.get {account, row_id, body: true}`, whose `body` is the stored plain text the TUI's preview shows (`mp_client::queries::message_body`).
A row the store has no body for answers `body: null`, and the frontend says so rather than switching modes; a row that does not exist is `not_found`.
The scheme's plain-text fallback reads the same command, so the two never disagree.

## Menus

`menu.rs` builds the menu bar with `tauri::menu`, no plugin: the Edit and Window menus are the predefined macOS items, the App menu is too apart from "Check for Updates…" and "Settings…", and File, View and Help carry our own.
Each of ours emits its id as the `menu` event (`listen("menu", …)`, allowed by `core:default`), and the frontend runs the same action its key runs (`MENU_ACTIONS` in `src/app/actions.ts`, pinned by a Rust test that reads that file):

| Id | Menu | Runs |
|---|---|---|
| `restart_daemon` | File | the confirm dialog, then `restart_daemon` |
| `toggle_sidebar` | View | collapse or expand the sidebar |
| `widen_list`, `narrow_list` | View | the list width, 40 px a step (the splitter's keyboard road) |
| `zoom_pane` | View | `z` |
| `command_palette` | View | `:` |
| `key_help` | View | `?` |
| `keyboard_shortcuts` | Help | `?` |

No item takes an accelerator the webview's keymap owns, since a menu key equivalent reaches AppKit before the page and a bare `z` would stop the user typing one.

"Check for Updates…", under About in the App menu, is not one of these: its id `check_for_updates` emits the event `update:check_requested` with no payload, and the frontend runs `update_check` with `manual: true` so its notice line shows the answer (see Updates).

## Updates

`updates.rs` keeps the app current from GitHub Releases through `tauri-plugin-updater` (#0139, stage 1).
The plugin reads `latest.json` from the one endpoint in `tauri.conf.json` (`plugins.updater.endpoints`, `https://github.com/sylvainHellin/mailypoppins/releases/latest/download/latest.json`) and checks the archive's minisign signature against `plugins.updater.pubkey`.
`requireSignedVersion` is on, so a signature whose trusted comment names another version than the manifest is refused; `tauri build` writes that version into every signature it makes.
On macOS the install untars the archive, renames the running bundle away and the new one into place, with an administrator prompt only when the bundle's directory is not writable.
The check and the install run in Rust: the plugin is registered for its Rust API, the capability grants no `updater:*` permission, and the CSP is unchanged.

### When it checks

Nothing checks or installs when `updates::gate` says no (`update_check` answers `disabled`, `update_install` rejects):

- a debug build (`cfg!(debug_assertions)`), which is `0.1.0` and would always see an update;
- fixture mode (`MP_DESKTOP_FIXTURE=1` or `--fixture`);
- an executable whose path does not contain `.app/Contents/MacOS/`, such as a release binary run from `target/`.

The silent check runs once, 10 s after setup, and only when the last successful check is 24 h old or more, none was recorded, or the clock went back past it.
A version it finds is held for `update_install`, and announced with the event `update:available` unless it is the skipped version.
A failed silent check is logged at warn and shown nowhere.

`current` in every answer is `package_info().version`, the bundle's version, which `pnpm bundle` stamps with `mp`'s and the updater compares with the manifest's.
It is not `version_info`'s `app_version`, the desktop crate's own `0.1.0`.

### update-state.json

The file sits in the app data directory (`~/Library/Application Support/dev.mailypoppins.desktop/` on macOS), beside `desktop.json`, whose commands accept only `SettingKey` names:

```json
{ "last_check": "2026-10-05T12:00:00Z", "skipped_version": "0.12.0", "pending_restart": { "from": "0.11.0", "to": "0.12.0" } }
```

- `last_check` is the last successful check, RFC 3339, written by the silent check and by `update_check` alike; a failed check leaves it.
- `skipped_version` is what `update_skip` recorded.
- `pending_restart` is written by a successful install; the next launch logs it and clears it 10 s after setup, and stage 2 will read it before then to restart a daemon of version `from` without the blocking screen.

Each key is `null` when unset.
A missing file, one that does not parse, and a key of the wrong type all read as empty, with a warning in the log for the last two; an unknown key is ignored.
A write goes to a temporary file in the same directory and is renamed over the old one, under a lock, so two writes never interleave and a crash leaves one whole file.

### Commands and events

```ts
type UpdateCheckState = "up_to_date" | "available" | "disabled" | "failed";
type UpdateCheck = {
  state: UpdateCheckState; current: string;
  version?: string; notes?: string; date?: string; // when available; date is RFC 3339
  reason?: string;                                 // when disabled or failed, a sentence to show
  last_check?: string;                             // RFC 3339, absent before the first successful check
};
type UpdateStatus = {
  current: string; enabled: boolean;
  reason?: string;     // when not enabled, the gate's sentence
  last_check?: string; // RFC 3339, absent before the first successful check
  available?: string;  // the version held in managed state, unless it is the skipped one
  installed?: string;  // the version this run installed, waiting for update_restart
};
type UpdateAvailable = { version: string; notes?: string; date?: string };
type UpdateProgress =
  | { type: "started"; content_length?: number }
  | { type: "progress"; downloaded: number; content_length?: number }
  | { type: "finished" };
```

`update_check { manual }` answers an `UpdateCheck` and never rejects.
With `manual: true` it always asks the endpoint and offers a skipped version too.
With `manual: false` it keeps the cooldown, answering inside it from memory (`available` when this run already found a version the user did not skip, else `up_to_date`), and a skipped version answers `up_to_date`.
A check that cannot run answers `disabled` with the gate's sentence ("Updates are off in a development build."), and one that fails answers `failed` with the plugin's error.
Every check that reaches the endpoint replaces the held update, or clears it when there is none.

`update_status` answers an `UpdateStatus` from what this run already knows: `package_info().version`, the gate, the state file's `last_check` and `skipped_version`, the update the last check holds and the version this run installed.
It never reaches the endpoint and never rejects.
Settings reads it each time it opens, for the version, the last check and the gate's sentence, and the window reads it once on mount, so a webview that reloaded still offers an update this run holds (`available`) or the restart into one it installed (`installed`).
A restart clears both, since they live in managed state; `pending_restart` in the file is not read here.

`update_skip { version }` records `skipped_version`; a manual check still offers that version.

`update_install { on_progress }` downloads, verifies and installs the held update, or the update a fresh check finds when none is held, and resolves once the new bundle is in place.
The channel carries `started` (with the archive's size when the server sent one), `progress` with the bytes downloaded so far at most every 100 ms and once at the end, and `finished` after the signature checked out and the bundle was replaced.
The app and its daemon keep running from the old binaries throughout, so a failed download or a refused signature leaves both as they were.
On success it records `pending_restart` and remembers the version for `update_restart`.
A second install while one runs is refused, and one of the version this run already installed sends `finished` at once.
It rejects with a plain string, the sentence to show, not a `GuiError`: the gate's sentence, "mailypoppins X is up to date." when there is nothing to install, or the plugin's error (network, signature, permissions).

`update_restart` needs an install from this run, and rejects with a string otherwise.
It holds off the session's on-demand start, sends `daemon.stop` over the app's session (a lifecycle method every daemon answers, not in `REQUIRED_CAPABILITIES`), waits up to 15 s for the daemon's socket to go, and calls `AppHandle::request_restart`, which runs the exit path (the terminals are killed) and starts the new bundle.
No daemon to stop is fine, and a daemon still there after the wait is logged and left to the new app's restart screen.
The new app starts a daemon of its own version on its first connect.
The frontend asks about open drafts first, as a window close does; this command asks nothing.

| Event | Payload | When |
|---|---|---|
| `update:available` | `UpdateAvailable` | the silent startup check found a version the user did not skip |
| `update:check_requested` | none (`null`) | the App menu's "Check for Updates…" |

Both are app events, heard with `listen` under `core:default`, like `menu`.

## Links

`on_navigation` admits the app origin (`tauri://localhost`, the dev server `http://localhost:1420` in a debug build), `mpmsg://localhost` and `about:blank` / `about:srcdoc`.
Every other navigation, and every `window.open` or `target=_blank`, is refused, appended to the intercepted-URL log, and sent as `link_intercepted`.
Nothing opens a browser except `open_external`, which the frontend calls only after an explicit user action.
With `MP_DESKTOP_STUB_OPENER=1`, `open_external` records the URL in the log with `source: "open_external_stub"` and opens nothing, which is what automated scenarios set.

## App CSP

The policy lives in `tauri.conf.json` under `app.security.csp`:

- `default-src 'self'`, `script-src 'self'`, `connect-src ipc: http://ipc.localhost`.
- `style-src 'self' 'unsafe-inline'`, because Vite injects CSS through `<style>` tags in development and Base UI positions popovers with inline `style` attributes.
- `img-src 'self' data:` and `font-src 'self' data:` for the shell itself, since Vite inlines assets under 4 KiB as `data:` URLs; the reader does not need them, its document carries its own policy.
- `frame-src mpmsg: http://mpmsg.localhost https: http:`: `https:` and `http:` are there only so that a link clicked in the reader reaches `on_navigation`, which refuses it; with `frame-src mpmsg:` alone the CSP blocks the navigation first and the click dies silently.
- `dangerousDisableAssetCspModification: ["style-src"]` stops Tauri from adding a nonce to `style-src` when `index.html` carries an inline `<style>`, which would switch `'unsafe-inline'` off.

The capability grants `core:default`, `core:window:allow-destroy` (the close of a window that asked first, see Terminal sessions), `opener:allow-open-url` scoped to `https://*`, `http://*` and `mailto:*`, and `dialog:allow-open`, the native file and folder picker of the Save and Attach file dialogs ([reader.md](reader.md), "Attachments").
`tauri-plugin-updater` is registered too, for its Rust API alone (see Updates), and the webview is granted none of its commands.
`tauri-plugin-dialog` is registered in the builder for that picker alone, which the webview calls through `@tauri-apps/plugin-dialog`; no command of this layer opens a dialog, and the plugin's save, message, ask and confirm stay unallowed.
The picker hands back a path, and since no `fs:` permission is granted the webview reads nothing through it: `attachment_save` and `draft_attach` read and write the disk themselves, as they do for a typed path.

## Environment

| Variable | Effect |
|---|---|
| `MP_DESKTOP_FIXTURE=1` (or `--fixture`) | Serve `clients/desktop/fixtures/*.json`, no daemon |
| `MP_DESKTOP_MP_BIN` | The `mp` binary to start the daemon with; else the sidecar next to the executable, then `PATH`, then `~/.cargo/bin/mp`, `/opt/homebrew/bin/mp`, `/usr/local/bin/mp` |
| `MP_DESKTOP_WINDOW_SIZE=WxH` | The initial window size, e.g. `950x800` for the medium layout or `600x820` for the narrow one; default `1400x900` |
| `MP_DESKTOP_STUB_OPENER=1` | `open_external` records instead of opening, and a file open only logs |
| `MP_DESKTOP_EDITOR` | The editor command template `editor_open` and `terminal_spawn` run; see Drafts and the editor, and Terminal sessions |
| `MP_DESKTOP_THEME` | Set by the desktop on the embedded editor's child, `dark` or `light`, the app's palette at spawn; see Terminal sessions, "The look" |
| `MP_DESKTOP_LOG` | `error` to `trace`, default `info`; to stderr and `<data>/logs/mp-desktop.log` |
| `MAILYPOPPINS_DATA_DIR`, `MAILYPOPPINS_CONFIG_DIR` | The same overrides the binary reads |
| `MAILYPOPPINS_DAEMON_AUTOSTART=0` | No on-demand start |
| `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS` | The start-and-connect budget, 12 s by default |

## Fixture mode

The fixtures hold 2 accounts, 6 mailboxes, 21 messages, 2 drafts, 3 HTML bodies, 1 armed send hold, 6 agenda events and 28 contacts.
Row 1021 has no `Subject:` and no `Date:`, so `message_html_meta` answers `null` for both.
Row 1006 is the hostile one: a policy with `report-uri` hidden inside the doctype, a script, a meta refresh, a `target=_blank` link, remote images, a form, an iframe and a lax CSP meta of its own.
Its meta refresh is kept on purpose, where the daemon would strip it, so the reader's own defences are what the fixture tests.
`fixture_simulate` drives `disconnect`, `reconnect`, `restart`, `resync`, `new_mail` and `shutdown` through the same pump a daemon feeds, and `rollback`, `hold`, `editor_save`, `editor_invalid`, `send_fail`, `send_partial`, `send_pending_append`, `send_hold:<secs>`, `invite_update`, `invite_cancel`, `rsvp_fail`, `rebuild_refused`, `signature_changed`, `config_invalid`, `config_absent`, `oauth_approve` and `oauth_deny` below; `send_fail` also fails the next invitation.

The five mutations change the fixture rows in memory: archive moves the row to `archive`, delete removes it, move puts it in the destination, and the flag and read commands set the row's flag.
Each answers like the daemon and publishes nothing; 1.5 s after the account's last mutation the fixture drains, one `state.invalidate` per mailbox whose counts moved.
A call without `settle: false` drains before it answers, as `mp archive` does.
`draft.discard` removes the draft file and publishes `state.remove`.
`sync.quick` and `sync.full` run for 0.8 s, drain, publish `sync.completed` and settle; `home` fails its login, as its sync health says.

`rollback` puts back every fixture mutation since the last rollback or restart, and `rollback:<n>` only the last `n`.
Each account then gets `mutations.rolled_back` with its count, followed by the counts that moved.
With nothing to roll back it is an error, since the daemon publishes nothing for a drain that failed nothing.

The bootstrap's seeded hold is on `work`'s first draft with a 60 s window, counting from the app's start.
`hold` arms another one, 10 s long, as the TUI would: `send.hold_started` at once, a `send.hold_tick` each second down to 1, then `send.hold_fired` (the fixture sends nothing).
`send_cancel_hold` stops either with `send.hold_cancelled`, and `restart` forgets every hold and the rollback journal.

`send.draft` and `send.approved` arm a hold through the same machinery when `hold` is true, with the fixture's `email.send_hold_secs`: 10 s, or what `send_hold:<secs>` set, where `0` arms none and the answer has no `held`.
The hold names the draft, or the batch's first approved draft; a batch with none arms nothing.
`send_cancel_hold` on it also ends the operation `cancelled`, and the draft stays as it was.
When the hold fires, or at once with none, the send runs 0.4 s later, as the daemon's does:

- a draft that is not approved fails the operation with "Email not approved for sending", and one that does not validate fails with its reason;
- otherwise the draft file is removed and published as `state.remove` of `draft:<account>/<id>`, an outbox row is kept, a copy lands at the top of Sent, and the operation settles with a `SendOutcome`;
- `send.approved` does that for every approved draft in listing order, with one `operation.progress` each, and settles with an `ApprovedOutcome` whose failed drafts are lines of it.

Each change of the outbox publishes `state.invalidate` of `outbox:<account>` with `{query: "counts"}`, and the filed copy the counts of Sent.
The outbox lives in memory and survives `restart`, as the daemon's store does; `send.outbox_list` answers it as an `OutboxListing`: the rows that are not `done`, and a `done` row that went to only some recipients, with the three counts.
The bootstrap's `outbox` counts are read from it, and `home`'s queued message is a `pending_send` row seeded at start.
`send_fail` fails the next send with a transport error, keeps the draft, and parks a `failed` row; `send_partial` refuses the last recipient (adding `nobody@refused.example` as a second one when the draft has one) and keeps a `partial` row; `send_pending_append` delivers but leaves the Sent copy owed, a `sent_pending_append` row and no copy in Sent.
Each simulation applies to the next send only.

`send.outbox_retry` refuses what the daemon refuses, with its words: a row it does not have, and a row that is neither `failed` nor `sent_pending_append`.
It re-arms a `failed` row to `pending_send`, publishes the invalidation, and 0.4 s later runs the row through the next send's simulation and settles with an `OutboxRetryOutcome`:

- with none, the row is delivered and gone, `state: null`, and `completed: 1` when it owes a Sent copy;
- `send_fail` leaves it `failed` with the transport's error, and a `sent_pending_append` row keeps its state;
- `send_partial` refuses its last outstanding recipient and leaves a partial `done` row;
- `send_pending_append` leaves it `sent_pending_append`.

The fixture files no Sent copy for a retried row, and it refuses a second retry of a re-armed row while the first runs, where the daemon admits it and settles on what it finds.
`send.outbox_discard` removes the row, publishes the invalidation, and answers `{discarded, row_id, message_id, revision}`; a row it does not have is `-32602`.

`message.materialise_attachment` writes a small file naming the part, and `message.materialise_html` the fixture's rendition of the row's HTML body, under `handles/<handle>/` in the run directory; a part the row does not have and a row with no HTML body are `-32602`.
`message.release_handle` removes the handle's directory.
`message.fetch` settles at once: a `Message-ID` a row of the account carries answers `already_present`, the fixture's server-only hit (`<server-only@fixture.example>`) lands at the top of the mailbox the hit names, by label or slug, with a counts `state.invalidate`, and any other message fails the operation as not on the server.
The system opener is never run in fixture mode: each file open is journalled, and the tests read the journal.

The drafts are real files in a per-run directory, `<temp>/mp-desktop-fixture-<pid>/drafts/<account>/`, written at start from `drafts.json`'s rows and `draft-bodies.json`.
Every call rescans that directory, as the daemon's draft queries do, so the listing, the counts and the bootstrap's drafts follow the files.
`draft.create`, `draft.reply`, `draft.forward` and `draft.create_from_message` write through the same `mp_core::draft` builders as the daemon; a reply or forward is renamed `<id>.md`, a created draft keeps its name, and a forward carries the source's attachments as small files under `attachments/`.
`signature.list` answers from `signatures.json`, whose `work` default is spliced into a new `work` draft.

The signatures are `signatures.json`'s `work` and `short`, with `work` the default of `work`.
They live in memory and are mirrored to `<temp>/mp-desktop-fixture-<pid>/signatures/<name>.md`, written at start and on every change, so an Edit opens a real file.
The pseudo-methods `signature.read {name}`, `signature.create {name}`, `signature.rename {account, old, new}`, `signature.delete {account, name}` and `signature.set_default {account, name}` do what `mp_core::signatures` does, and refuse with its sentences as `-32602`; an unknown account is `-32005`.
A create and a rename publish the watcher's `signature.changed` for the new file, and a delete and a default change publish nothing.
`signature_changed` appends "Edited behind the fixture's back." to `work` and its file and publishes `signature.changed`, as an edit in another window would; it is an error once `work` is gone.
Each write publishes `draft.changed`, and a file that does not parse is listed under `skipped`, is an `invalid` row in the bootstrap, and is refused by `draft.approve` and `draft.preview` with `-32010`.

`editor_open` spawns nothing in fixture mode: it journals the path and the resolved command, and answers `fixture: true` with no `pid`.
`editor_save` appends a line to the file the last `editor_open` named, which moves it to the top of the listing, and publishes `draft.changed`.
`editor_invalid` breaks that file's frontmatter and publishes `draft.invalid`.
Either is an error before any `editor_open`.

`calendar.events` answers each account's agenda from `calendar.json`, sorted as the daemon sorts it, and refuses an unknown account with `-32005` and one whose `account.list` state is not `ready` with `-32006`.
`work` has five rows: a past kick-off in 2020 (row 9101), the steering committee of the inbox invitation (row 1008, `REQUEST` in 2099 with an accepted, a pending and a declined attendee), a weekly team sync the user organizes (row 9103, `is_organizer`, one cancelled occurrence), a cancelled budget review (row 9102) and an undated offsite planning (row 9104); `home` has one upcoming appointment (row 9201).
The dates are 2020 and 2099 because the frontend filters on the real clock.
`message.ics` answers the base64 of the row's `invite.ics` from the same file, `null` for row 9104, and `-32602` for a row the account has neither in a mailbox nor in its agenda.

`invite_update` delivers a new version of the steering committee: row 1008's `sequence` goes up by one and its start and end move one day later (the 15th of October 2099 the first time), its `invite.ics` follows, and an "Updated invitation: Steering committee" email with that `invite.ics` lands at the top of `work`'s inbox.
`invite_cancel` marks row 1008 `cancelled`, in the row and in its `event`, and lands a "Cancelled: Steering committee" email carrying a `METHOD:CANCEL` `invite.ics` at the top of the inbox.
Both change the agenda row in place, where a daemon would fold the new email into it, and both publish `state.invalidate` of `mailbox:work/inbox`, as the sync that fetched the email would.
The reader's card of row 1008 follows, since `message.invite` reads the agenda row.

`message.invite` answers a row's `event` from the agenda row of the same id, else from the version an `invite_update` or `invite_cancel` email carried (the cancellation's is a `CANCEL`), else `null`; a row the account holds neither in a mailbox nor in its agenda is `-32602`.
`calendar.rsvp` refuses what the daemon refuses, in its order: an unknown parameter, an unknown account, the Graph account `home` with the daemon's sentence before anything else (so the probe's `{account}` alone gets it), a response word it does not take, a row it does not hold, and a row with no invitation.
Otherwise the operation runs 0.5 s: the agenda row's `rsvp` and the user's own attendee status become the reply, `state.invalidate` of `mailbox:<account>/sent` says a copy was filed, and it settles with the daemon's shape, `delivered: true`.
`rsvp_fail` fails the next RSVP instead with the transport error `send_fail` uses, after a `failed` outbox row naming the organizer and its `state.invalidate` of `outbox:<account>`; the agenda row keeps its reply.

`send.invite` refuses in `plan_invite`'s order with the daemon's words: `home` with the Graph sentence, then a missing subject, a missing start, both or neither of `end` and `duration`, and no recipient in `to` or `cc`; it does not parse the times.
Otherwise the operation runs for the send delay: the invitation joins the account's agenda as a row the user organizes (a wall-clock start read as its UTC sort key, every recipient `needs-action`), `state.invalidate` of `mailbox:<account>/sent` says its copy was filed, and it settles with a `SendOutcome` every recipient took.
After `send_fail` it fails instead with the transport error, after a `failed` outbox row naming the recipients.

`contact.search` answers from `contacts.json`: 25 ranked contacts for `work` and 3 for `home`, each with a fixed `score` that spreads from 1000 down.
`work` holds two display names with a comma (`Doe, Jane`, `Legal, Supplier Ltd`), one with an apostrophe (`Tom O'Brien`), one with an accent (`Sofia García`) and two addresses with no name.
A query matches the address or the display name as a case-insensitive substring, where the daemon's match is fuzzy; the answer is in score order, empty query included, and capped at `limit`, 20 when absent.
It refuses an unknown parameter, an unknown account (`-32005`) and one whose store is not ready (`-32006`), as the daemon does.
`contact.rebuild` publishes its one `operation.progress` (phase `contacts`, `done: 0`, the account as its message) and settles 0.6 s later with `saved: written`, `contacts` the account's index size and `kept: 0`; the index itself does not change.
`rebuild_refused` makes the next rebuild settle `refused_shrunk` instead, with 3 contacts found and the whole index kept (25 for `work`).

`config.get` answers `{revision, path, state: "ok", config}`, where `path` is `<temp>/mp-desktop-fixture-<pid>/config.toml`, written at start from the `fixtures/config.toml` template, whose accounts are `accounts.json`'s, and `config` is `fixtures/config.json`, the effective configuration with the passwords `<redacted>`.
`revision` starts at 0.
`diagnostic.log_path` answers `<root>/logs/mailypoppins-2026-09-30.log`, written at start with ten lines in the daemon's log format.
Both refuse an unknown parameter with `-32602`.

`config.reload` reads that file again, and refuses an unknown parameter too.
When it loads, `revision` goes up by one, `config.changed` is published with nothing added, updated or removed and that revision, and the answer is the three empty lists.
`config_invalid` appends the line `this line is not toml` to the file, and the next reload publishes `config.invalid` with the path, that line's number and "key with no value, expected `=`", then refuses with `-32007` and the same message; `revision` and `state` stay.
A reload succeeds again once that line is gone from the file.
`config.set_password` refuses an unknown parameter, a `kind` other than `smtp` or `imap`, and a missing `value` with `-32602`, and an account `config.json` does not configure with `-32005` "no account named <name> is configured".
Otherwise it journals `{account, kind, value: "<redacted>"}` and answers `{stored: true, account, kind, key}`, with the daemon's key `<kind>-password-<account>`.

`config.add_account` refuses an unknown parameter, a missing `config.toml`, a missing `account` object or `name`, and a name `config.json` already has, each with the daemon's sentence and `-32602`.
Otherwise it appends the daemon's `[[accounts]]` block for the object to `<root>/config.toml`, key for key what `account_block` writes, and serves the account at once.
The account is `ready` in `account.list` (`backend` `graph` for a Graph account, else `imap`, and `default` when it is the only one), in the bootstrap's accounts with sync health `ok`, and in `config.get` with the daemon's defaults and the passwords `<redacted>`.
Its mailboxes are an empty Inbox, Archive and Sent (slugs `inbox`, `archive`, `sent`), which `mailbox.list` answers.
Then `revision` goes up by one, `config.changed` is published with the account as `added`, and the answer is that swap.
The fixture publishes nothing else, where the daemon's swap also starts a runtime whose state changes follow; `config.changed` is what the window reads the account list and the new mailboxes on.

`config.init` does the same with a whole file, only where no `config.toml` exists, and adds `path` to its answer; otherwise it refuses with "a configuration already exists at <path>; edit it and call config.reload".
`config_absent` is a daemon restarted on an empty configuration directory: it removes `<root>/config.toml`, every account from every answer (the bootstrap's accounts, mailboxes, drafts, outbox counts, holds and diagnostics, `account.list` and `config.get`), sets `state` to `absent` and `revision` to 0, and then runs `restart`.

`config.oauth2_login` refuses as the daemon does: an unknown parameter, an account `config.json` does not configure (`-32005`), a password account, an account with no `oauth2` table, and one with an empty client or tenant, each with the daemon's sentence.
Otherwise it starts an operation and 0.3 s later publishes its one `operation.progress`, phase `device_code`, message `https://microsoft.com/devicelogin FXTR-CODE`, which `operation.status` keeps as its `progress`.
The sign-in then waits: `oauth_approve` settles every waiting one with `{stored: true, account, kind, key: "oauth2-token-<account>"}`, `kind` `oauth2` or `graph` after the account's `auth_method`, and `oauth_deny` fails it with the provider's "Authorization was declined by the user.".
Either is an error when no sign-in waits; `operation.cancel` ends one `cancelled`, and `restart` forgets it.

## Tests

```sh
export CARGO_TARGET_DIR=/var/tmp/mp-desktop-target
cd clients/desktop/src-tauri
cargo test                              # includes the stale check of src/protocol/generated/gui
cargo clippy --all-targets -- -D warnings
# the protocol bindings' stale check, from the repository root:
cargo test -p mp-protocol --features ts
# against a real daemon in a scratch data directory, including a restart:
cargo build --manifest-path ../../../Cargo.toml --bin mp
MP_DESKTOP_MP_BIN=$CARGO_TARGET_DIR/debug/mp cargo test -- --ignored live_daemon
```

`live_daemon_version_handshake` starts a daemon with that binary, then points `MP_DESKTOP_MP_BIN` at a wrapper whose `--version` says `99.0.0` and expects the connect refused as a `version_mismatch` carrying the daemon's version, then accepted again with the real binary.
