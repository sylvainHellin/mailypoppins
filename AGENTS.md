# Email CLI

## Project Scope

Rust CLI + TUI for managing emails as Markdown files with YAML frontmatter. Drafts follow an auditable workflow (`draft → approved → sent`). Inbox emails can be fetched, viewed, replied to, forwarded, and archived. The TUI is the primary interface; all CLI commands are also available non-interactively.

---

## Design Overview

**Single crate, library + binary.** All logic lives in `src/lib.rs` modules. The TUI and CLI share the same library -- no subprocess spawning. Config types derive `Clone` so they can be moved into background threads.

**Multi-account.** Config uses a `[[accounts]]` array. Each account has independent IMAP/SMTP/directories/signatures. The TUI shows one account at a time, switching via backtick or Ctrl+1-9. All accounts are watched for new mail simultaneously. CLI commands target an account via `--account` flag (defaults to first). Keyring keys are namespaced: `smtp-password-{name}`, `imap-password-{name}`.

**TUI follows The Elm Architecture (TEA).** `App::update()` is a pure state machine (`Message -> State`). Side effects (IMAP, SMTP, filesystem) are dispatched as `Action` variants and executed in `tui/mod.rs::handle_action()`. Background operations run on threads and report back via `mpsc` as `BgResult` variants (each tagged with `account_index`). Mutations (archive, delete) are optimistic: local state updates immediately, server follows async.

**Account state proxy pattern.** `App` holds a `Vec<AccountState>` plus top-level proxy fields (mailboxes, list_index, etc.) that mirror the active account. `save_to_account()` / `load_from_account()` sync state on account switch. This avoids refactoring every key handler to use indirect access.

**Emails are files.** Each email is a `.md` file with YAML frontmatter. Incoming HTML bodies are saved as a companion `.html` file. Attachments go in a `<stem>_attachments/` directory alongside the `.md`. The three companion files (`.md`, `.html`, `_attachments/`) are always moved/deleted together.

**IMAP uses a single session per operation.** `sync_mailboxes()` opens one TLS connection for the entire sync. A two-pass fetch (headers first, full body only for new UIDs) minimizes bandwidth. Core functions have `_on_session` variants for session reuse, plus thin wrappers for standalone use. IMAP supports both implicit TLS (port 993) and STARTTLS (any other port, e.g. 1143 for Proton Bridge). The `ImapStream` wrapper injects a fake greeting for STARTTLS connections since `async_imap` expects one.

**Sending is per-recipient.** Each recipient gets an individual SMTP envelope, while the visible To/Cc headers are preserved for all. This gives per-recipient success/failure tracking. After a successful send, the raw RFC 822 message is appended to the server Sent folder (best-effort, non-fatal on failure). SMTP uses implicit TLS for port 465, STARTTLS for other ports. Both paths support `accept_invalid_certs` for self-signed certificates (e.g. Proton Bridge).

### Module Map

| File | Responsibility |
|------|---------------|
| `src/types.rs` | Shared types: `EmailStatus`, `EmailFrontmatter`, `EmailDraft`, `InboxFrontmatter`, `SaveFrontmatter`, `collapse_hyphens` |
| `src/config.rs` | Config loading (`~/.config/email/config.toml`), keyring, mailbox/drafts dir resolution |
| `src/config_cmd.rs` | Config subcommands: init wizard, add-account, show, set-password, migrate, path |
| `src/parse.rs` | RFC822 parsing, saving emails to disk, attachment extraction, `open_file_with_system()` |
| `src/draft.rs` | Draft parsing/validation, reply/forward creation, status transitions |
| `src/send.rs` | `markdown_to_html`, per-recipient `send_email`, IMAP APPEND to Sent |
| `src/sync.rs` | Local file scanning, mailbox dir resolution, reconciliation helpers |
| **`src/imap_client/`** | |
| `mod.rs` | `ImapStream` wrapper, `open_imap_session()`, re-exports |
| `fetch.rs` | `fetch_emails*`, `fetch_new_emails*`, `fetch_server_message_ids*` |
| `sync.rs` | `sync_mailboxes()`, `list_mailboxes()`, `SyncTarget`, `SyncResult` |
| `search.rs` | `parse_search_query()`, `build_imap_search_query()`, `FetchCriteria` |
| `watch.rs` | `watch_mailbox()` (IMAP IDLE) |
| `ops.rs` | `archive_email_on_server/locally`, `delete_email_on_server/locally`, `append_to_sent_folder`, `mark_read/unread_on_server`, `update_read_status_locally`, `get_message_id_from_file` |
| `batch.rs` | `batch_archive_emails_locally`, `batch_delete_emails_locally` |
| **`src/tui/`** | |
| `mod.rs` | Event loop (`run_loop`), watcher spawn, bg result drain |
| `actions.rs` | `handle_action()` -- side-effect dispatch for all `Action` variants |
| `bg.rs` | `handle_bg_result()` -- process background task completions |
| `helpers.rs` | Terminal suspend/resume, editor, clipboard, watcher loop, `lib_do_sync`, `resolve_send_account` |
| `event.rs` | Crossterm event polling |
| `theme.rs` | Catppuccin Mocha palette constants |
| **`src/tui/app/`** | |
| `mod.rs` | `App` struct, `new()`, `update()`, account sync, core state helpers |
| `types.rs` | `EmailEntry`, `AccountState`, `BgResult`, `Action`, `Focus`, `MailboxKind`, mailbox builders |
| `keys.rs` | `handle_key()` dispatch + all `handle_*_key()` methods |
| **`src/tui/ui/`** | |
| `mod.rs` | `view()` -- top-level layout dispatch |
| `sidebar.rs` | `render_sidebar()`, `render_activity_log()` |
| `list.rs` | `render_email_list()` |
| `headers.rs` | `render_headers()`, `header_line()` |
| `preview.rs` | `render_body()`, markdown parsing + word wrap |
| `status.rs` | `render_status_bar()` |
| `overlays.rs` | Confirm dialog, attachment picker, persistent error, help overlay |
| `search.rs` | Server search overlay + sub-renderers |
| `util.rs` | `pane_border_style`, `hint_span`, `desc_span`, `truncate` |

---

## Implementation Notes

- **HTML charset:** Incoming HTML bodies are saved as raw bytes from the server. Always inject `<meta charset="UTF-8">` before writing to disk (see `ensure_utf8_charset()` in `parse.rs`) -- browsers default to latin-1 otherwise, breaking umlauts and other non-ASCII characters.
- **Signature placement:** Reply/forward drafts contain a `{{SIGNATURE}}` placeholder. `markdown_to_html` replaces it at send time. If removed, signature falls back to end of body.
- **Sent folder dedup:** Sent `.md` files store `message_id` in frontmatter. Sync skips uploading emails already present on the server by Message-ID. The Sent directory is never reconciled -- local files are source of truth.
- **Reconciliation scope:** Only INBOX and Archive participate in server-driven reconciliation. Sent is excluded.
- **Send account resolution:** At send time, the draft's `from:` address is matched against each account's `default_from` to select the correct SMTP/IMAP config. This is done in `resolve_send_account()` in `tui/helpers.rs`. Draft-creation commands (reply, forward, new) auto-insert the active account's `default_from`.
- **Queued actions:** Fetch/sync are deferred while mutations are in-flight (`bg_mutations > 0`) and auto-triggered on completion.
- **Per-account watchers:** One IMAP IDLE thread per account. All send events to a shared channel tagged with `account_index`. Non-active account changes set `has_unseen` (badge in status bar).
- **Output style (CLI):** Use `colored` crate. `✓` success (green), `✗` error (red), `⚠` warning (yellow), `ℹ` info (blue). No emoji -- use Unicode or Nerd Font icons.

---

## TUI Design

- The TUI is a human-facing interface only. It must **never** implement email logic directly.
- ALL email operations (send, fetch, archive, delete, move, search, etc.) must live in the library modules (`imap_client.rs`, `send.rs`, `draft.rs`, `sync.rs`, `parse.rs`). The TUI dispatches `Action` variants and handles `BgResult` callbacks -- it never calls IMAP/SMTP/MIME code inline.
- No email protocol code (SMTP, IMAP, MIME, etc.) belongs in `tui/app.rs` or `tui/ui.rs`. If you find yourself writing email logic in a TUI component, stop and put it in the appropriate library module instead.
- `tui/actions.rs::handle_action()` is the boundary: it may call library functions to perform side effects, but `app/` (state) and `ui/` (rendering) must stay pure.

---

## Planning

- **[BACKLOG.md](BACKLOG.md)** -- prioritized work items in Now / Next / Later buckets. Check this at the start of each session.
- **[docs/plans/](docs/plans/)** -- design briefs for non-trivial features. Read the relevant plan before starting work.
- **[CHANGELOG.md](CHANGELOG.md)** -- what shipped, by version.

---

## Testing

- **252 tests** (210 unit, 42 integration). All run offline in <0.5s. Run: `cargo test`
- Unit tests are inline `#[cfg(test)] mod tests` in each module
- Integration tests live in `tests/` and use `tempfile::tempdir()` for isolation
- `insta` snapshot tests for `markdown_to_html` output. Run `cargo insta review` to approve changes.
- Some private helpers are `pub(crate)` for testability: `ensure_utf8_charset`, `sanitize_attachment_filename`, `floor_char_boundary`, `build_imap_search_query`, `parse_date_to_imap`, `parse_message_id_from_header_bytes`, `scan_mailbox_message_ids`, `collapse_hyphens`
- `update_read_status_locally`, `get_message_id_from_file` are `pub` for TUI use and tested directly
- No IMAP/SMTP mock server yet -- only pure logic and filesystem tests

---

## Instructions

- **No emoji** in code, output, or UI. Use Unicode symbols or Nerd Font icons.
- **After every code change:** verify it works, then install the updated binary:
  ```
  cargo install --path .
  ```
- **Keep this file current.** When making architectural or design changes, update the relevant section of `AGENTS.md` to reflect the new state.
- **Keep BACKLOG.md current.** When completing a feature, move it out of the backlog and into CHANGELOG.md. When discovering new work, add it to the appropriate bucket.
