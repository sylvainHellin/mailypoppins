# Architecture

How mailypoppins is put together. Read this before non-trivial changes.

## Project invariants

- TUI never implements email logic. It dispatches `Action` variants and handles `BgResult` callbacks; all IMAP/SMTP/MIME/parsing/auth code lives in the library. The TEA section below covers the mechanics.
- No migration paths until v1.0. When changing data formats, secret storage, or wire protocols, drop the old code and prompt the user to reconfigure. Do not write v1->v2 migrators.
- Windows is targeted via WSL only. No native-Windows code paths (registry, Credential Manager, etc.).

## Crate shape

Single crate, library + binary. All logic lives in `src/lib.rs` modules so the TUI can call them directly without subprocess spawning. Config types derive `Clone` so they can be moved into background threads.

## Big-picture design

- **Multi-account.** Config uses a `[[accounts]]` array. Each account has independent IMAP/SMTP/directories/signatures. The TUI shows one account at a time, switching via backtick or Ctrl+1-9. All accounts are watched for new mail simultaneously. CLI commands target an account via `--account` (defaults to first). Secret keys are namespaced: `smtp-password-{name}`, `imap-password-{name}`.
- **TUI follows The Elm Architecture (TEA).** `App::update()` is a pure state machine (`Message -> State`). Side effects (IMAP, SMTP, filesystem) are dispatched as `Action` variants and executed in `tui/actions.rs::handle_action()`. Background operations run on threads and report back via `mpsc` as `BgResult` variants (each tagged with `account_index`). Mutations (archive, delete) are optimistic: local state updates immediately, server follows async.
- **Account state proxy pattern.** `App` holds a `Vec<AccountState>` plus top-level proxy fields (mailboxes, list_index, etc.) that mirror the active account. `save_to_account()` / `load_from_account()` sync state on account switch. This avoids refactoring every key handler to use indirect access.
- **Emails are files.** Each email is a `.md` file with YAML frontmatter. Incoming HTML bodies are saved as a companion `.html` file. Attachments go in a `<stem>_attachments/` directory alongside the `.md`. The three companion files (`.md`, `.html`, `_attachments/`) are always moved/deleted together. In addition, every fetched attachment is hardlinked into a per-account stable mirror at `<account_dir>/attachments/<sanitized-message-id>/<file>` (copy fallback on filesystems that disallow hardlinks). The mirror exists so forward drafts keep working when the source email is later archived; it is removed only when the email disappears from every synced mailbox. See [tickets/0006-attachment-paths-after-archive.md](tickets/0006-attachment-paths-after-archive.md).
- **IMAP uses one session per operation.** `sync_mailboxes()` opens one TLS connection for the entire sync. A two-pass fetch (headers first, full body only for new UIDs) minimizes bandwidth. Core functions have `_on_session` variants for session reuse, plus thin wrappers for standalone use. IMAP supports both implicit TLS (port 993) and STARTTLS (any other port, e.g. 1143 for Proton Bridge). The `ImapStream` wrapper injects a fake greeting for STARTTLS connections since `async_imap` expects one.
- **Sending is per-recipient.** Each recipient gets an individual SMTP envelope, while the visible To/Cc headers are preserved for all. This gives per-recipient success/failure tracking. After a successful send, the raw RFC 822 message is appended to the server Sent folder (best-effort, non-fatal on failure). SMTP uses implicit TLS for port 465, STARTTLS for other ports. Both paths support `accept_invalid_certs` for self-signed certificates (e.g. Proton Bridge); the flag only applies to loopback hosts -- for any remote host it is refused at connect time.
- **Two transports.** IMAP/SMTP for password and OAuth2-XOAUTH2 accounts; Microsoft Graph REST for tenants that block IMAP/SMTP. TUI actions branch on `app.is_graph()`. See [auth.md](auth.md).

## Module map

| File | Responsibility |
|------|---------------|
| `src/types.rs` | Shared types: `EmailStatus`, `EmailFrontmatter`, `EmailDraft`, `InboxFrontmatter`, `SaveFrontmatter`, `collapse_hyphens` |
| `src/config.rs` | Config loading (`~/.config/email/config.toml`), secrets-backend dispatch, app data dir helpers (`mailypoppins_data_dir`, `account_dir`, `mailbox_dir`, `drafts_dir`, `tokens_dir`, `logs_dir`, `contacts_cache_path`), legacy-config rejection, logging init |
| `src/secrets.rs` | Machine-bound encrypted secrets store (ChaCha20-Poly1305 + HKDF-SHA256). `SecretsBackend` trait with `EncryptedFileBackend` (default) and `KeyringBackend` (opt-in). `encrypt_blob` / `decrypt_blob` reused by `oauth2.rs`. See [secrets.md](secrets.md). |
| `src/oauth2.rs` | OAuth2 device-code flow, encrypted token cache at `tokens_dir()/<account>.enc`, refresh, XOAUTH2 SASL string builder. Scope-parameterised (`IMAP_SMTP_SCOPES` vs `GRAPH_SCOPES`). |
| `src/graph.rs` | Microsoft Graph REST client: list folders, fetch / sync / send / archive / delete / mark-read / search. Used when `auth_method = "graph"`. |
| `src/contacts/` + `src/contacts_cmd.rs` | Contact mining from local mail, frecency ranking, per-account cache at `account_dir(name)/contacts-cache.json`. CLI: `email contacts {rebuild,stats,list}`. |
| `src/config_cmd/` | Config subcommands: init wizard, add-account, show, set-password, oauth2-login, reset-secrets, path |
| `src/parse.rs` | RFC822 parsing, saving emails to disk, attachment extraction, `open_file_with_system()`, `attachments_dir_for()`, `stable_attachments_dir()`, `link_or_copy()`, `account_dir_for_email()`, `ensure_utf8_charset()` |
| `src/draft.rs` | Draft parsing/validation, reply/forward creation, status transitions |
| `src/send.rs` | `markdown_to_html`, per-recipient `send_email`, IMAP APPEND to Sent |
| `src/sync.rs` | Local file scanning, mailbox dir resolution, reconciliation helpers |
| `src/timing.rs` | `TimingSpan` -- emits `[TIMING]` log lines with millisecond precision. Filter logs with `rg '\[TIMING\]'`. |
| **`src/imap_client/`** | |
| `mod.rs` | `ImapStream` wrapper, `open_imap_session()`, re-exports |
| `fetch.rs` | `fetch_emails*`, `fetch_new_emails*`, `fetch_server_message_ids*` |
| `sync.rs` | `sync_mailboxes()`, `list_mailboxes()`, `SyncTarget`, `SyncResult`, `MailboxState`, `MessageIdIndex` |
| `search.rs` | `parse_search_query()`, `build_imap_search_query()`, `FetchCriteria` |
| `watch.rs` | `watch_mailbox()` (IMAP IDLE) |
| `ops.rs` | `archive_email_on_server/locally`, `delete_email_on_server/locally`, `append_to_sent_folder`, `mark_read/unread_on_server`, `update_read_status_locally`, `get_message_id_from_file` |
| `batch.rs` | `batch_archive_emails_locally`, `batch_delete_emails_locally` |
| **`src/tui/`** | |
| `mod.rs` | Event loop (`run_loop`), watcher spawn, bg result drain |
| `actions.rs` | `handle_action()` -- side-effect dispatch for all `Action` variants. Branches on `is_graph()`. |
| `bg.rs` | `handle_bg_result()` -- process background task completions |
| `helpers.rs` | Terminal suspend/resume, editor, clipboard, watcher loop, `lib_do_sync`, `lib_do_sync_graph`, `resolve_send_account` |
| `event.rs` | Crossterm event polling |
| `theme.rs` | Catppuccin Mocha palette constants |
| **`src/tui/app/`** | |
| `mod.rs` | `App` struct, `new()`, `update()`, account sync, core state helpers |
| `types.rs` | `EmailEntry`, `AccountState`, `BgResult`, `Action`, `Focus`, `MailboxKind`, mailbox builders |
| `keys.rs` | `handle_key()` dispatch + all `handle_*_key()` methods |
| **`src/tui/ui/`** | |
| `mod.rs` | `view()` -- top-level layout dispatch |
| `sidebar.rs` | `render_sidebar()` |
| `activity.rs` | `render_activity_log()` |
| `list.rs` | `render_email_list()` |
| `headers.rs` | `render_headers()`, `header_line()` |
| `preview.rs` | `render_body()`, markdown parsing + word wrap |
| `compose.rs` | Compose-related rendering |
| `status.rs` | `render_status_bar()` |
| `overlays.rs` | Confirm dialog, attachment picker, persistent error, help overlay |
| `search.rs` | Server search overlay + sub-renderers |
| `util.rs` | `pane_border_style`, `hint_span`, `desc_span`, `truncate` |

## Performance-critical invariants

These exist for measured reasons; do not regress them without re-measuring.

- **In-memory message-ID index.** `AccountState.message_id_index` is a `HashMap<PathBuf, HashMap<String, PathBuf>>` mapping local dir to {message_id -> file_path}. Built once at startup from disk. Quick sync uses it instead of re-scanning directories (zero disk I/O for known-ID checks). Updated incrementally on save, archive, delete, and reconciliation. Full sync rebuilds from disk.
- **IMAP mailbox state caching.** `AccountState.mailbox_states` stores `MailboxState { uid_validity, uid_next, exists }` per role from the last sync's SELECT response. On quick sync, the new SELECT response is compared to the cached state to skip reconciliation when only pure additions occurred (most common case). Reconciliation only runs when `exists` decreased or `uid_next`/`exists` deltas indicate moves/deletes.
- **Two-pass fetch.** Pass 1 fetches `BODY.PEEK[HEADER.FIELDS (Message-ID)] FLAGS` only (~50 bytes / msg) over the full quick-sync window. Pass 2 fetches full `RFC822` for unknown Message-IDs and is skipped entirely when nothing is new. Both passes share one IMAP session. Pass 1 must always cover the full window: its `\Seen` flags are the only server->local read-status channel, so any "probe fewer UIDs first and bail early" optimization silently breaks read/unread sync (this happened -- ticket #0004).
- **Queued mutations.** Fetch/sync are deferred while mutations are in-flight (`bg_mutations > 0`) and auto-triggered on completion.
- **Per-account watchers.** One IMAP IDLE thread per password/OAuth2 account, plus one 60s-poll thread per Graph account. All emit to a shared channel tagged with `account_index`. Non-active account changes set `has_unseen` (badge in status bar).

## Reconciliation scope

- Only **INBOX** and **Archive** participate in server-driven reconciliation.
- The **Sent** directory is never reconciled -- locally-authored files are the source of truth. Sent `.md` files store `message_id` in frontmatter, and sync skips uploading emails already present on the server by Message-ID.

## Data and config layout

User-owned config:

- **Config:** `~/.config/email/config.toml` (multi-account `[[accounts]]` array). User-edited; references signature paths and account-level settings.
- **Secrets:** `~/.config/email/secrets.enc` (machine-bound encrypted; see [secrets.md](secrets.md))

App-managed data, all under `mailypoppins_data_dir()`:

| Platform          | Default `mailypoppins_data_dir()`                          |
|-------------------|------------------------------------------------------------|
| macOS             | `~/Library/Application Support/mailypoppins`               |
| Linux (incl. WSL) | `$XDG_DATA_HOME/mailypoppins` (def. `~/.local/share/mailypoppins`) |

Layout under the data dir:

```
<data_dir>/
  accounts/<name>/{inbox,archive,sent,drafts,<extra_slug>}/
  accounts/<name>/attachments/<sanitized-message-id>/   # stable hardlink mirror (#0006)
  accounts/<name>/contacts-cache.json
  tokens/<name>.enc       # OAuth2 / Graph encrypted refresh tokens
  logs/mailypoppins-YYYY-MM-DD.log
```

Override via the `MAILYPOPPINS_DATA_DIR` env var (used by tests; also a single escape hatch for power users who need a portable / non-default location). Users who want the mail tree visible inside an Obsidian vault should symlink `accounts/<name>/` into their vault.

The OS keyring service name (when keyring backend is opted into) remains `email-cli` (constant `KEYRING_SERVICE` in `src/secrets.rs`).

## Testing

- **252 tests** (210 unit, 42 integration). All run offline in <0.5s. `cargo test`.
- Unit tests are inline `#[cfg(test)] mod tests` in each module.
- Integration tests live in `tests/` and use `tempfile::tempdir()` for isolation.
- `insta` snapshot tests for `markdown_to_html` output. `cargo insta review` to approve changes.
- Some private helpers are `pub(crate)` for testability: `ensure_utf8_charset`, `sanitize_attachment_filename`, `floor_char_boundary`, `build_imap_search_query`, `parse_date_to_imap`, `parse_message_id_from_header_bytes`, `scan_mailbox_message_ids`, `collapse_hyphens`. `update_read_status_locally`, `get_message_id_from_file` are `pub` for TUI use.
- No IMAP/SMTP mock server yet -- only pure logic and filesystem tests.

## TUI invariants

- The TUI is a human-facing interface only. It must **never** implement email logic directly. ALL email operations (send, fetch, archive, delete, move, search) live in library modules. The TUI dispatches `Action` variants and handles `BgResult` callbacks.
- No email protocol code (SMTP, IMAP, MIME, Graph REST) belongs in `tui/app/` or `tui/ui/`. If you find yourself writing email logic in a TUI component, stop and put it in the appropriate library module instead.
- `tui/actions.rs::handle_action()` is the boundary: it may call library functions to perform side effects, but `app/` (state) and `ui/` (rendering) must stay pure.
