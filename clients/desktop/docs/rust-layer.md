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
  | { kind: "internal"; message: string };
```

`version_mismatch` is the blocking restart screen; its button calls `restart_daemon` after the user confirms.

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
  name: string; default: boolean; backend: "imap" | "graph"; store_state: string;
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

type InterceptedUrl = { url: string; at: number; source: "navigation" | "new_window" | "open_external_stub" };
type VersionInfo = {
  app_version: string; protocol_min: number; protocol_max: number; fixture: boolean;
  daemon: { daemon_version: string; protocol: number; instance_id: string } | null;
};
```

A `MessageMeta` header is `null` when the message did not carry it, where a `MessageListRow` of the same message has `""`.
`AccountInfo.runtime_state`, `sync_health` and `outbox` and `MailboxListing.runtime_state` and `sync_health` come from the latest bootstrap; later changes arrive as events.

## The event stream

`subscribe_events` registers one `Channel`; a second call replaces the first.
The command claims a subscription number on the IPC thread, in call order, and attaches off it under the pump lock; a claim a later call superseded is dropped, so under React StrictMode's double mount the last call is the sink whichever attach runs first.
The channel first carries a `connection` event and, once connected, a `rebootstrapped` with `cause: "subscribed"`, taken while events are held back, so the frontend's model starts complete and every later event applies on top of it.

```ts
type BootstrapCause = "initial" | "subscribed" | "requested" | "resync" | "reconnected" | "instance_changed";
type GuiEvent =
  | { type: "event"; event: { instance_id: string; revision: number; kind: string; payload: unknown } }
  | { type: "resync"; instance_id: string; reason: string }
  | { type: "disconnected"; reason: string }
  | { type: "reconnected"; instance_id: string }
  | { type: "rebootstrapped"; cause: BootstrapCause; bootstrap: Bootstrap }
  | { type: "operation_settled"; operation_id: string; kind: "server_search"; status: OperationStatus }
  | { type: "operation_dropped"; operation_id: string; kind: "server_search"; reason: string }
  | { type: "connection"; status: ConnectionStatus }
  | { type: "link_intercepted"; url: InterceptedUrl };
```

`event` carries the daemon's envelope verbatim and only when it applied above the watermark; duplicates are dropped in Rust.
`rebootstrapped` replaces the whole model: restore selection, focus and scroll by stable identifiers (account name, mailbox slug, `message_id` or `selector`, never `row_id` across a daemon restart).
A server search streams `message.server_hit` events and ends with `operation.finished`, both carrying its `operation_id`; a finish lost to a resync or a reconnect arrives as `operation_settled` instead, and a daemon restart turns every running search into `operation_dropped`.
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
| `MP_DESKTOP_STUB_OPENER=1` | `open_external` records instead of opening |
| `MP_DESKTOP_LOG` | `error` to `trace`, default `info`; to stderr and `<data>/logs/mp-desktop.log` |
| `MAILYPOPPINS_DATA_DIR`, `MAILYPOPPINS_CONFIG_DIR` | The same overrides the binary reads |
| `MAILYPOPPINS_DAEMON_AUTOSTART=0` | No on-demand start |
| `MAILYPOPPINS_DAEMON_AUTOSTART_TIMEOUT_MS` | The start-and-connect budget, 12 s by default |

## Fixture mode

The fixtures hold 2 accounts, 6 mailboxes, 21 messages, 2 drafts and 3 HTML bodies.
Row 1021 has no `Subject:` and no `Date:`, so `message_html_meta` answers `null` for both.
Row 1006 is the hostile one: a policy with `report-uri` hidden inside the doctype, a script, a meta refresh, a `target=_blank` link, remote images, a form, an iframe and a lax CSP meta of its own.
Its meta refresh is kept on purpose, where the daemon would strip it, so the reader's own defences are what the fixture tests.
`fixture_simulate` drives `disconnect`, `reconnect`, `restart`, `resync`, `new_mail` and `shutdown` through the same pump a daemon feeds.

## Tests

```sh
export CARGO_TARGET_DIR=/var/tmp/mp-desktop-target
cd clients/desktop/src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
# against a real daemon in a scratch data directory, including a restart:
cargo build --manifest-path ../../../Cargo.toml --bin mp
MP_DESKTOP_MP_BIN=$CARGO_TARGET_DIR/debug/mp cargo test -- --ignored live_daemon
```
