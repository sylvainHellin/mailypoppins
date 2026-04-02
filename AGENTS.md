# Email CLI

## Project Scope

Rust CLI + TUI for managing emails as Markdown files with YAML frontmatter. Drafts follow an auditable workflow (`draft → approved → sent`). Inbox emails can be fetched, viewed, replied to, forwarded, and archived. The TUI is the primary interface; all CLI commands are also available non-interactively.

---

## Design Overview

**Single crate, library + binary.** All logic lives in `src/lib.rs` modules. The TUI and CLI share the same library -- no subprocess spawning. Config types derive `Clone` so they can be moved into background threads.

**TUI follows The Elm Architecture (TEA).** `App::update()` is a pure state machine (`Message → State`). Side effects (IMAP, SMTP, filesystem) are dispatched as `Action` variants and executed in `tui/mod.rs::handle_action()`. Background operations run on threads and report back via `mpsc` as `BgResult` variants. Mutations (archive, delete) are optimistic: local state updates immediately, server follows async.

**Emails are files.** Each email is a `.md` file with YAML frontmatter. Incoming HTML bodies are saved as a companion `.html` file. Attachments go in a `<stem>_attachments/` directory alongside the `.md`. The three companion files (`.md`, `.html`, `_attachments/`) are always moved/deleted together.

**IMAP uses a single session per operation.** `sync_mailboxes()` opens one TLS connection for the entire sync. A two-pass fetch (headers first, full body only for new UIDs) minimizes bandwidth. Core functions have `_on_session` variants for session reuse, plus thin wrappers for standalone use.

**Sending is per-recipient.** Each recipient gets an individual SMTP envelope, while the visible To/Cc headers are preserved for all. This gives per-recipient success/failure tracking. After a successful send, the raw RFC 822 message is appended to the server Sent folder (best-effort, non-fatal on failure).

### Module Map

| File | Responsibility |
|------|---------------|
| `src/types.rs` | Shared types: `EmailStatus`, `EmailFrontmatter`, `EmailDraft`, `InboxFrontmatter` |
| `src/config.rs` | Config loading (`~/.config/email/config.toml`), keyring, mailbox/drafts dir resolution |
| `src/config_cmd.rs` | Config subcommands: init wizard, show, set-password, path |
| `src/parse.rs` | RFC822 parsing, saving emails to disk, attachment extraction, `open_file_with_system()` |
| `src/imap_client.rs` | All IMAP operations: fetch, sync, watch (IDLE), archive, delete, search |
| `src/draft.rs` | Draft parsing/validation, reply/forward creation, status transitions |
| `src/send.rs` | `markdown_to_html`, per-recipient `send_email`, IMAP APPEND to Sent |
| `src/sync.rs` | Local file scanning, mailbox dir resolution, reconciliation helpers |
| `src/tui/mod.rs` | Event loop, action dispatcher, background threads, IMAP watcher |
| `src/tui/app.rs` | App state, TEA update cycle, key handlers |
| `src/tui/ui.rs` | Rendering: sidebar, list, headers pane, preview pane, overlays |
| `src/tui/theme.rs` | Catppuccin Mocha palette constants |

---

## Implementation Notes

- **HTML charset:** Incoming HTML bodies are saved as raw bytes from the server. Always inject `<meta charset="UTF-8">` before writing to disk (see `ensure_utf8_charset()` in `parse.rs`) -- browsers default to latin-1 otherwise, breaking umlauts and other non-ASCII characters.
- **Signature placement:** Reply/forward drafts contain a `{{SIGNATURE}}` placeholder. `markdown_to_html` replaces it at send time. If removed, signature falls back to end of body.
- **Sent folder dedup:** Sent `.md` files store `message_id` in frontmatter. Sync skips uploading emails already present on the server by Message-ID. The Sent directory is never reconciled -- local files are source of truth.
- **Reconciliation scope:** Only INBOX and Archive participate in server-driven reconciliation. Sent is excluded.
- **Queued actions:** Fetch/sync are deferred while mutations are in-flight (`bg_mutations > 0`) and auto-triggered on completion.
- **Output style (CLI):** Use `colored` crate. `✓` success (green), `✗` error (red), `⚠` warning (yellow), `ℹ` info (blue). No emoji -- use Unicode or Nerd Font icons.

---

## Instructions

- **No emoji** in code, output, or UI. Use Unicode symbols or Nerd Font icons.
- **After every code change:** verify it works, then install the updated binary:
  ```
  cargo install --path .
  ```
- **Keep this file current.** When making architectural or design changes, update the relevant section of `agent.md` to reflect the new state.
