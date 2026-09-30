# The desktop client's Rust layer

The Tauri Rust layer in `src-tauri/src/` is the GUI's only road to the daemon.
It links `mp-client`, `mp-protocol` and `mp-core`, never the root `mailypoppins` crate.
The frontend calls the commands below with `invoke` and listens on one ordered event channel; it never sends a daemon method name.

## Modules

| Module | Owns |
|---|---|
| `paths.rs` | Data, config, runtime, socket and log paths, resolved through `mp_core::config` like the binary |
| `connector.rs` | The handshake as `ClientKind::Gui`, the on-demand start through `mp daemon start`, `ConnectError`, `mp daemon restart` |
| `session.rs` | The one `Session`, the `StateTracker` watermark, the event pump, re-bootstrap, the awaited-operations table |
| `commands.rs` | The Tauri commands and their result types |
| `editor.rs` | The external editor a draft opens in, and the editor setting |
| `attachments.rs` | Attachments, a draft's `attachments:` list, and the browser rendition |
| `reader.rs` | The `mpmsg` scheme serving `message.html` |
| `navigation.rs` | The webview's navigation allowlist and the intercepted-URL log |
| `fixture.rs` | The daemon stand-in behind `MP_DESKTOP_FIXTURE=1` |
| `menu.rs` | The native menus (App, File, Edit, View, Window, Help) and their `menu` event |
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
`setup` is the desktop's own configuration: an editor that did not start, or a settings file that does not read; its message names what to change.

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
| `message_text` | `account`, `row_id` | `MessageText` |
| `message_html_meta` | `account`, `row_id` | `MessageMeta` |
| `search_local` | `params: LocalSearchParams` | `LocalSearchHit[]` |
| `search_server_start` | `params: ServerSearchParams` | `{ operation_id }` |
| `search_server_cancel` | `operation_id` | `"cancelled" \| "already_settled"` |
| `message_archive` | `account`, `row_ids` | `MutationBatch` |
| `message_delete` | `account`, `row_ids` | `MutationBatch` |
| `message_move` | `account`, `row_ids`, `destination` (slug or label) | `MutationBatch` |
| `message_set_flag` | `account`, `row_ids`, `flagged` | `MutationBatch` |
| `message_set_read` | `account`, `row_ids`, `read` | `MutationBatch` |
| `draft_discard` | `account`, `ids` | `DraftDiscardBatch` |
| `draft_create` | `account`, `name`, `signature?`, `no_signature?`, `headers?: DraftHeaders` | `DraftCreated` |
| `draft_reply` | `account`, `row_id`, `all`, `headers?` | `DraftCreated` |
| `draft_forward` | `account`, `row_id`, `headers?` | `DraftCreated` |
| `draft_from_message` | `account`, `kind: DraftKind`, `message: DraftMessage` | `DraftCreated` |
| `draft_path` | `account`, `id` | `DraftLocation` |
| `draft_approve` | `account`, `ids` | `DraftStatusBatch` |
| `draft_demote` | `account`, `ids` | `DraftStatusBatch` |
| `draft_validate` | `account`, `id` | `DraftValidation` |
| `draft_preview` | `account`, `id` | `DraftPreview` |
| `draft_set_recipients` | `account`, `id`, `to`, `cc`, `bcc`, `subject?` | `DraftLocation` |
| `signature_list` | `account` | `SignatureListing` |
| `editor_open` | `path` | `EditorLaunch` |
| `editor_setting_get` | none | `EditorSetting` |
| `editor_setting_set` | `editor` (or `null` to clear) | `EditorSetting` |
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
| `sync_trigger` | `account`, `mode: "quick" \| "full"` | `{ operation_id }` |
| `restart_daemon` | none | nothing; runs `mp daemon restart` (fixture mode: simulates one) |
| `intercepted_urls` | none | `InterceptedUrl[]`, and the log is cleared |
| `open_external` | `url` (http, https or mailto) | nothing; opens it in the default handler |
| `version_info` | none | `VersionInfo` |
| `fixture_simulate` | `what` | nothing; fixture mode only |

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
type EditorSource = "env" | "setting" | "visual" | "editor" | "probe" | "fallback";
type EditorLaunch = { editor: string; pid?: number; source: EditorSource };
type EditorSetting = {
  editor: string | null; file: string; env_override: string | null;
  effective: string; effective_source: EditorSource;
};
type SyncMode = "quick" | "full";

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
Its failure carries `invalid`, the `draft.invalid` payload: the session keeps a refusal's text but not its `data`, so the path comes from `draft.list`'s skipped file under that stem and the one diagnostic is the refusal's message.

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
Its `SendRefusal` is the `GuiError`, and for a file that does not parse (`-32010` `draft_invalid`) `invalid` carries the `draft.invalid` payload, rebuilt from the listing's skipped file as `draft_approve` does.
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

Each ends one of three ways, as the event stream below says: `operation.finished` with `{operation_id, state, result?, error?}`, `operation_settled` with the whole `OperationStatus` after a re-query, or `operation_dropped` when the daemon restarted.
A send's `state` is `succeeded` once the submission ran, with each recipient's verdict in `recipients`, `failed` with the transport's error, or `cancelled`; a `succeeded` send every recipient refused is a failure to show.

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

## Drafts and the editor

A draft is a Markdown file with YAML frontmatter in the account's drafts directory, and every command that writes one answers its absolute `path`.
`draft_create` takes the file name; a name already taken is refused with `protocol` code `-32602`, and the message names the existing path.
`draft.create` itself takes no recipients, so `headers` are written into the new file client-side.
`draft_reply` and `draft_forward` address the source by `row_id` and pass `headers` to the daemon, which then needs all four fields; an empty string clears one.
`draft_from_message` builds a reply, reply-all or forward from a server-only search hit (`DraftMessage`, the hit's own field names), with no attachments.

`draft_set_recipients` is client-side, like the TUI's `ce`: it resolves the file through `draft.path` and rewrites the `to`, `cc`, `bcc` and `subject` lines with `mp_core::draft::rewrite_draft_recipients`, which leaves the body and every other field byte for byte.
An absent `subject` keeps the draft's own, and the signature is not re-spliced.
The daemon serves no `signature.list`, so `signature_list` reads the signatures directory through `mp_core::signatures`, as the TUI does; `default` is the account's default whether or not `include_signature` is on.

Every change to a draft file, from the daemon, an editor or a client-side rewrite, reaches the frontend as the watcher's `draft.changed` or `draft.invalid`, and the commands publish nothing of their own.

`editor_open` opens a file in the user's editor and never waits for it to exit, since a `code`-style launcher exits at once while the edit goes on.
The editor is a command template, resolved in this order:

1. `MP_DESKTOP_EDITOR`;
2. the `editor` key of `desktop.json` in the app config directory (`~/Library/Application Support/dev.mailypoppins.desktop/` on macOS), read and written by `editor_setting_get` and `editor_setting_set`;
3. `$VISUAL`, then `$EDITOR`, each skipped when it names a terminal editor (`vi`, `vim`, `nvim`, `hx`, `nano` and a few more);
4. the first of `code`, `zed`, `subl` and `cursor` found in `/opt/homebrew/bin`, `/usr/local/bin` or `/usr/bin`;
5. `open -t` on macOS, which opens the default text editor, or `xdg-open` elsewhere.

The template is split with shell-words rules and run without a shell.
`{path}` in any word is replaced by the path; a template without it gets the path as its last argument.
An app started from Finder inherits a `PATH` without Homebrew, so a bare program name is looked up on `PATH` and then in the three directories above.
The process gets null stdio and its own process group.
A spawn failure or a nonzero exit within 2 s rejects with `setup`, whose message names the variable or the setting to fix; the command answers when the launcher exits or after 2 s, whichever comes first.
A terminal editor needs a terminal to run in, so it goes through a terminal command, for example `MP_DESKTOP_EDITOR="wezterm start -- hx {path}"`.

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
Each write first removes the renditions older than a day.

A draft's attachments are the paths its `attachments:` frontmatter lists, and the daemon serves neither `draft.attach` nor a removal, so all four draft commands work on the file, which `draft.path` resolves fresh:

- `draft_attachments` parses the file and answers each entry as typed, where the send path finds it, and whether a file is there: `~` expands against `$HOME`, and a relative entry resolves against the draft's own directory (`ATT-03`).
- `draft_attach` appends with `mp_core::draft::append_draft_attachment`, the TUI's `ta`, which keeps the body and every other line byte for byte and stores the path as typed, `~` included.
  It refuses a blank or relative path and a directory with `protocol`, a path with no file behind it with `not_found` ("No such file: <path>", the TUI's words), and a file the list already names with `protocol` ("<entry> is already attached").
- `draft_attachment_remove` drops item `index` of the block list and leaves the file it named alone.
  The rewrite is this layer's own, line by line, and it first checks that the list has one line per parsed entry, so a flow-style list or an entry that spans lines is refused rather than rewritten; an emptied list keeps its bare `attachments:` key, the skeleton's shape.
- `draft_attachment_open` opens entry `index` with the opener (`ATT-04`), and a missing file is `not_found`.

```ts
type DraftAttachments = {
  account: string; id: string; path: string;
  attachments: { index: number; entry: string; path: string; exists: boolean }[];
};
```

A write reaches the frontend as the watcher's `draft.changed`, like every other client-side rewrite.
An editor open on the same file can overwrite the change with its own buffer, as it can in the TUI.

`message_fetch` is the TUI search overlay's `f` (`LST-09`): `message.fetch {account, mailbox, message_id}` ingests a server-only message.
It is an operation, and one message is quick, so the command reads `operation.status` every 100 ms until it ends and answers with its result; the frontend awaits one promise, and no `PendingKind` is registered.
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
type PendingKind = "server_search" | "sync" | "send" | "send_approved" | "outbox_retry";
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
A sync started by `sync_trigger`, a send started by `send_draft` or `send_approved`, and a retry started by `outbox_retry` end the same three ways.
A hit or a finish for an operation this layer no longer awaits (another window's, a cancelled one, or one a re-bootstrap already settled) is dropped in Rust, so nothing about an operation follows its `operation_settled`, `operation_dropped` or `operation.finished`.

## The reader

`message_html_meta` answers the headers and `html_url`, `mpmsg://localhost/<account>/<row_id>`.
The iframe loads that URL with `sandbox="allow-popups"` and no `allow-scripts`; the plan's section "Reading HTML bodies" explains both.
The frontend side, the refused-link notice and the guard's verification in a real webview are in [reader.md](reader.md).
The header is always `MESSAGE_CSP` and never a policy read out of the message: a sender can hide a meta-looking policy inside the doctype, ahead of the daemon's tag, and a header may carry `report-uri`.
The scheme answers:

- 200 with the rendition, the daemon's policy (`reader::MESSAGE_CSP`) as a `Content-Security-Policy` header, `X-Content-Type-Options: nosniff` and `X-Mp-Rendition: html`;
- 200 with the stored plain text in a minimal document and `X-Mp-Rendition: text` when the message has no markup;
- 404 for an unknown account or row, 400 for a malformed URL, 503 while no daemon answers, 504 on a timeout.

A rendition over 8 MiB goes through `message.materialise_html`, and the handle is released as soon as the file is read.

## Menus

`menu.rs` builds the menu bar with `tauri::menu`, no plugin: the App, Edit and Window menus are the predefined macOS items, and File, View and Help carry our own.
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

The capability grants `core:default` and `opener:allow-open-url` scoped to `https://*`, `http://*` and `mailto:*`.

## Environment

| Variable | Effect |
|---|---|
| `MP_DESKTOP_FIXTURE=1` (or `--fixture`) | Serve `clients/desktop/fixtures/*.json`, no daemon |
| `MP_DESKTOP_MP_BIN` | The `mp` binary to start the daemon with; else the sidecar next to the executable, then `PATH`, then `~/.cargo/bin/mp`, `/opt/homebrew/bin/mp`, `/usr/local/bin/mp` |
| `MP_DESKTOP_WINDOW_SIZE=WxH` | The initial window size, e.g. `950x800` for the medium layout or `600x820` for the narrow one; default `1400x900` |
| `MP_DESKTOP_STUB_OPENER=1` | `open_external` records instead of opening, and a file open only logs |
| `MP_DESKTOP_EDITOR` | The editor command template `editor_open` runs; see Drafts and the editor |
| `MP_DESKTOP_LOG` | `error` to `trace`, default `info`; to stderr and `<data>/logs/mp-desktop.log` |
| `MAILYPOPPINS_DATA_DIR`, `MAILYPOPPINS_CONFIG_DIR` | The same overrides the binary reads |
| `MAILYPOPPINS_DAEMON_AUTOSTART=0` | No on-demand start |
| `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS` | The start-and-connect budget, 12 s by default |

## Fixture mode

The fixtures hold 2 accounts, 6 mailboxes, 21 messages, 2 drafts, 3 HTML bodies and 1 armed send hold.
Row 1021 has no `Subject:` and no `Date:`, so `message_html_meta` answers `null` for both.
Row 1006 is the hostile one: a policy with `report-uri` hidden inside the doctype, a script, a meta refresh, a `target=_blank` link, remote images, a form, an iframe and a lax CSP meta of its own.
Its meta refresh is kept on purpose, where the daemon would strip it, so the reader's own defences are what the fixture tests.
`fixture_simulate` drives `disconnect`, `reconnect`, `restart`, `resync`, `new_mail` and `shutdown` through the same pump a daemon feeds, and `rollback`, `hold`, `editor_save`, `editor_invalid`, `send_fail`, `send_partial`, `send_pending_append` and `send_hold:<secs>` below.

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
Each write publishes `draft.changed`, and a file that does not parse is listed under `skipped`, is an `invalid` row in the bootstrap, and is refused by `draft.approve` and `draft.preview` with `-32010`.

`editor_open` spawns nothing in fixture mode: it journals the path and the resolved command.
`editor_save` appends a line to the file the last `editor_open` named, which moves it to the top of the listing, and publishes `draft.changed`.
`editor_invalid` breaks that file's frontmatter and publishes `draft.invalid`.
Either is an error before any `editor_open`.

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
