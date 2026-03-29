# Email CLI - Project Instructions

## Overview

CLI tool and TUI for managing email drafts written in Markdown with YAML frontmatter. Supports sending via SMTP, fetching via IMAP, archiving via IMAP, and an auditable draft workflow (`draft → approved → sent`). Inbox emails can be archived (`inbox → archived`). Running `email` with no arguments launches a ratatui-based TUI for interactive email management.

## Tech Stack

- **Language:** Rust (Edition 2021)
- **CLI parsing:** `clap` (derive macros)
- **TUI:** `ratatui` + `crossterm` (alternate screen, raw mode, event loop)
- **Clipboard:** `arboard` (copy-to-clipboard in TUI)
- **SMTP:** `lettre` (async, TLS)
- **IMAP:** `async-imap` + `async-native-tls`
- **Email parsing:** `mailparse`
- **HTML → plaintext:** `html2text`
- **Frontmatter:** `gray_matter` + `serde_yaml`
- **Markdown → HTML:** `pulldown-cmark`
- **Terminal output:** `colored` (CLI mode)
- **Async runtime:** `tokio`
- **Error handling:** `anyhow`
- **Logging:** `log` + `simplelog` (file-based, daily rotation)
- **Secrets:** `keyring` (OS-native: macOS Keychain, Windows Credential Manager, Linux Secret Service)
- **Interactive prompts:** `dialoguer` (fuzzy-select, multi-select, password input)

## Architecture

The crate is both a library (`src/lib.rs`) and a binary (`src/main.rs`). The library exposes all modules so the TUI can call them directly without subprocess spawning.

### Modules

| File | Responsibility |
|------|---------------|
| `src/lib.rs` | Library root, re-exports all modules |
| `src/main.rs` | CLI definition (`Cli`, `Commands`), `main()`, command dispatch; launches TUI when no args given |
| `src/types.rs` | Shared types: `EmailStatus`, `EmailFrontmatter`, `EmailDraft`, `InboxFrontmatter` |
| `src/config.rs` | Global config loading (`~/.config/email/config.toml`), keyring integration, mailbox resolution, logging. All config types derive `Clone` for TUI thread sharing. |
| `src/config_cmd.rs` | Config subcommands: init wizard (credential testing, mailbox discovery), show, set-password, path |
| `src/parse.rs` | Email body parsing, slugifying, `FetchedEmail`, RFC822 parsing, attachment extraction, save/display, local message-ID scanning |
| `src/imap_client.rs` | All IMAP operations: fetch, list, append, watch, archive, delete, unified `sync_mailboxes()` orchestrator with two-pass fetch |
| `src/draft.rs` | Draft parsing, validation, preview, reply/forward creation, status updates |
| `src/send.rs` | `RecipientRole`, `SendResult`, `markdown_to_html`, `send_email` |
| `src/sync.rs` | Local file scanning, reconciliation, mailbox dir resolution |
| `src/tui/mod.rs` | TUI entry point (`tui::run()`), main event loop, action dispatcher, background task handling, terminal management, IMAP watcher thread |
| `src/tui/app.rs` | TUI state (`App`), TEA-pattern message/update cycle, `EmailEntry` type, mailbox model, key handlers |
| `src/tui/ui.rs` | TUI rendering (sidebar, email list, headers pane, preview pane, status bar, overlays) |
| `src/tui/event.rs` | Terminal event polling (crossterm), tick rate management |
| `src/tui/theme.rs` | Catppuccin Mocha color palette constants |

### Dependency graph (no cycles)

```
types       --> (none)
config      --> (none)
parse       --> types
imap_client --> config, parse, sync, types
draft       --> config, parse, types
send        --> config, types
sync        --> parse
config_cmd  --> config, imap_client
tui         --> config, draft, imap_client, parse, send, sync, types
main        --> all modules (via lib)
```

## Commands

| Command | Description |
|---------|-------------|
| `email` | Launch the TUI (no arguments) |
| `email <file>` | Preview a draft (dry-run) |
| `email send <file> [--yes]` | Send an approved email (`-y` skips confirmation) |
| `email send-approved [dir] [--yes]` | Batch send all approved drafts (`-y` skips confirmation) |
| `email list [dir]` | List drafts grouped by status |
| `email validate [file\|dir]` | Validate frontmatter and fields |
| `email mark-approved <file>` | Change status from draft to approved |
| `email new <name>` | Create a new draft from template |
| `email reply [file] [--all]` | Create a reply draft from a received email |
| `email forward [file]` | Create a forward draft from a received email |
| `email list-mailboxes` | List available IMAP mailboxes |
| `email fetch [filters]` | Fetch emails via IMAP |
| `email sync [--limit N] [--mailbox ...] [--reconcile]` | Additive sync (Message-ID dedup); `--reconcile` detects server moves/deletes |
| `email watch [--mailbox] [--timeout]` | Watch mailbox for changes via IMAP IDLE |
| `email archive <file>` | Archive an inbox email (server + local) |
| `email delete <file>` | Delete an inbox email (server + local) |
| `email config init` | Interactive setup wizard with credential testing |
| `email config show` | Show current configuration (passwords masked) |
| `email config set-password` | Store SMTP or IMAP password in OS keyring |
| `email config path` | Print config file path |

## Configuration

All configuration lives in a single global file: **`~/.config/email/config.toml`**.

- **`[email]`** -- Font family/size, signature toggle.
- **`[smtp]`** -- Host, port, username, default_from. Password stored in OS keyring (key: `smtp-password`).
- **`[imap]`** -- Host, port, username (all fall back to SMTP values if omitted). Password in keyring (key: `imap-password`, falls back to `smtp-password`).
- **`[directories]`** -- `root` (base path, e.g. `~/notes/email`) and `drafts` (relative to root or absolute).
- **`[mailboxes.inbox]`, `[mailboxes.archive]`, `[mailboxes.sent]`** -- Three special-role mailboxes, each with `server` (IMAP name) and `local` (directory, relative to root).
- **`[[mailboxes.extra]]`** -- Additional mailboxes to sync (same `server`/`local` structure).
- **`[signatures]`** -- Default signature name and named entries with `path` (absolute or `~`-expanded).

### Mailbox resolution
`resolve_mailbox_dir(config, name)` looks up by IMAP server name (case-insensitive) or by role name (`inbox`, `archive`, `sent`). Local paths relative to `directories.root`.

### Drafts directory resolution
`list`, `send-approved`, and `reply` auto-resolve via `resolve_drafts_dir`: explicit CLI arg -> `config.directories.drafts` (resolved against root) -> `"."` fallback.

### Config subcommands
- `email config init` -- Interactive wizard: tests SMTP/IMAP credentials, discovers server mailboxes, assigns roles, writes config.
- `email config show` -- Displays resolved config with masked passwords.
- `email config set-password smtp|imap` -- Stores password in OS keyring.
- `email config path` -- Prints config file path.

## Logging

Logs are written to `~/.email-cli/logs/email-cli-YYYY-MM-DD.log` (append mode, one file per day). Logging is initialized at startup via `init_logging()` and is non-fatal — if the log directory or file can't be created, a warning is printed and execution continues.

**Log levels used:**
- `INFO`: Command invoked, email sending started, per-recipient success, IMAP operations, archive operations
- `WARN`: Partial send failures, missing config fallbacks
- `ERROR`: SMTP failures (per recipient), IMAP connection failures, invalid addresses
- `DEBUG`: Config paths, recipient list details

## Per-Recipient Sending

`send_email` sends to each recipient individually via `send_raw` with single-recipient envelopes, while preserving visible To/Cc headers for all recipients. This provides per-recipient success/failure tracking. Recipients are deduplicated by address. Return type is `(SendResult, Vec<u8>, Option<String>)` -- results, raw RFC 822 message, and extracted Message-ID.

**Caller behavior:**
- **All succeeded** → mark as sent, IMAP APPEND to Sent folder (best-effort)
- **Partial success** → mark as sent + warn, IMAP APPEND to Sent folder (best-effort)
- **All failed** → return error, do not mark as sent

**IMAP APPEND:** After a successful send, the raw message is appended to the server-side Sent folder (configured via `mailboxes.sent.server`) with `\Seen` flag. This is best-effort -- APPEND failure is logged as a warning but does not fail the send. The `message_id` is stored in the local sent file's frontmatter for sync dedup.

## Email Draft Format

```markdown
---
to: recipient@example.com
cc: optional@example.com
subject: "Subject line"
status: draft
---

Email body in **Markdown**.
```

Status workflow: `draft` → `approved` (via `mark-approved`) → `sent` (via `send`). Inbox emails: `inbox` → `archived` (via `archive` command).

Sent files include additional fields: `sent_at`, `sent_via`, and `message_id` (the RFC 822 Message-ID, used for sync dedup).

### Reply Drafts & Signature Placement

Reply drafts (created via `email reply`) include a `{{SIGNATURE}}` placeholder between the reply area and the quoted conversation. When sent, `markdown_to_html` replaces this placeholder with the signature HTML, ensuring the signature appears between the reply and the quoted text. If the placeholder is removed, the signature falls back to being appended at the end (same as regular drafts).

### Forward Drafts

Forward drafts (created via `email forward` or `w` in TUI) include:
- `to: ""` (empty, to be filled by the user)
- `subject: "Fwd: ..."` prefix
- `attachments:` with absolute paths to source attachment files (reuses existing `send.rs` attachment logic)
- `{{SIGNATURE}}` placeholder between the reply area and the forwarded message block
- A "---------- Forwarded message ----------" header block with original From/Date/Subject/To, followed by the original body unquoted

### Attachment Storage

Incoming attachments are saved next to the `.md` file in a `_attachments/` subdirectory:

```
inbox/2024-01-01-1200_sender_subject.md
inbox/2024-01-01-1200_sender_subject.html
inbox/2024-01-01-1200_sender_subject_attachments/
  document.pdf
  image.png
```

Frontmatter records filenames (not paths):

```yaml
has_attachments: true
attachments:
  - "document.pdf"
  - "image.png"
```

The `_attachments/` directory is moved/deleted alongside `.md` and `.html` companion files during archive, delete, sync move, and reconciliation operations. The `attachments_dir_for(md_path)` helper in `parse.rs` computes the directory path from any `.md` file path.

## TUI (v0.5.0)

Running `email` with no arguments launches an interactive TUI built with `ratatui` and `crossterm`.

### Layout

Four panes: sidebar (mailbox list), email list, headers, and body preview. Panes adapt to terminal width (sidebar hidden below 40 cols, right panes hidden below 80 cols).

### TUI Architecture (TEA pattern)

The TUI follows The Elm Architecture: `Message -> update -> Action`. `App::update()` is a pure state transition. Side-effects (IMAP, SMTP, filesystem) are handled in `handle_action()` in `tui/mod.rs`, dispatched via `Action` variants. Background operations (send, sync, archive, delete, fetch, reconcile) run on dedicated threads and report results through an `mpsc` channel as `BgResult` variants.

### Background Operations

- **IMAP watcher**: A long-lived thread runs IMAP IDLE on INBOX and sends `WatchEvent::Changed` when new mail arrives, triggering an automatic fetch.
- **Mutations** (archive, delete): Optimistic -- local file removed from list immediately, server operation runs in background. On failure, caches are invalidated and the list reloads.
- **Queued actions**: Fetch/sync/reconcile are deferred if mutations are in progress (`bg_mutations > 0`), then auto-executed when mutations complete.

### Key Bindings (List focus)

| Key | Action |
|-----|--------|
| `j`/`k` | Navigate up/down |
| `g g`/`G` | Jump to top/bottom |
| `Space` | Toggle selection |
| `Ctrl+a` | Select all visible |
| `Esc` | Clear selection |
| `Enter`/`e` | Open in `$EDITOR` |
| `r`/`R` | Reply / Reply All |
| `w` | Forward |
| `a` | Archive selected or cursor (with confirmation) |
| `d` | Delete selected or cursor (with confirmation) |
| `A` | Mark as approved |
| `x` | Send (with confirmation) |
| `X` | Send all approved (with confirmation) |
| `n` | New draft |
| `y` | Copy file path to clipboard |
| `f` | Fetch (all mailboxes) |
| `F` | Sync (fetch + reconcile) |
| `/` | Search (subject/contact/date) |
| `\` | Search (includes body) |
| `s` | Focus sidebar |
| `1`-`9` | Jump to mailbox by index |
| `Tab`/`Shift-Tab` | Cycle pane focus |
| `?` | Help overlay |
| `q` | Quit |

### TUI Direct Library Calls

The TUI calls library functions directly (e.g., `send_email`, `fetch_emails`, `archive_email_locally`) rather than spawning `email` subprocesses. This is why `src/lib.rs` exists and why config types derive `Clone` -- they need to be moved into background threads.

## Style Rules

- **No emojis in code or output.** Use Unicode symbols or Nerd Font icons instead. The user has a Nerd Font installed.

## Output Conventions (CLI mode)

- `colored` crate for terminal styling
- `✓` green for success, `✗` red for errors, `⚠` yellow for warnings, `ℹ` blue for info
- Header keys in `fetch` display use distinct colors: From (green), To/Cc (blue), Subject (yellow), Date (magenta)

## Sync Architecture

`email sync` uses a unified `sync_mailboxes()` orchestrator in `imap_client.rs` that opens a single IMAP connection for the entire operation.

### Two-pass fetch (connection reuse + bandwidth optimization)

For each mailbox, `sync_mailboxes()` uses a two-pass approach via `fetch_new_emails_on_session()`:

1. **Pass 1 (headers only)**: Fetch `BODY.PEEK[HEADER.FIELDS (Message-ID)]` for the last N UIDs (~50 bytes/msg). Compare against locally-known message IDs (from `scan_existing_message_ids()`).
2. **Pass 2 (full download, new only)**: Fetch `RFC822` only for UIDs whose Message-ID is not already local. This skips full downloads for already-synced emails.

All passes reuse the same IMAP session (1 TLS handshake total, vs. 5+ previously).

### Session-passing variants

Core IMAP functions have `_on_session` variants (`fetch_emails_on_session`, `fetch_server_message_ids_on_session`) that accept an existing `&mut ImapSession`. The original functions (`fetch_emails`, `fetch_server_message_ids`) are thin wrappers that open/close their own session for backward compatibility.

### Key types

- `SyncTarget { role, server_name, local_dir, status }` -- describes a mailbox to sync
- `SyncResult { saved, skipped, moved, removed }` -- aggregate results

### Reconciliation

Pass `--reconcile` to also detect server-side moves and deletes (uses the same single connection):

1. **Additive pass** (always): Two-pass fetch for each configured mailbox (default: INBOX, Archive, Sent, plus any `mailboxes.extra`).
2. **Reconciliation pass** (`--reconcile` only): Fetch all Message-ID headers from INBOX and Archive via `fetch_server_message_ids_on_session()`. Compare against local files. Move/delete locally to match server state.

**Important constraints:**
- The Sent directory is never reconciled -- locally-authored files are the source of truth.
- Only INBOX and Archive participate in reconciliation.

## After Each Change

1. **Test the app** — Run the binary yourself to verify the change works as expected.
2. **Update the binary** — If it works, install the updated binary via `cargo install --path .` so it's available at `~/.cargo/bin/email`.
