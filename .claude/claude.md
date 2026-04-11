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
| `src/config_cmd/` | Config subcommands module: `show.rs` (show, path), `password.rs` (set-password), `init.rs` (init wizard, add-account, TOML builders), `migrate.rs` (old-to-new format migration), `helpers.rs` (prompt_input, select_mailbox, connection tests) |
| `src/parse.rs` | Email body parsing, slugifying, `FetchedEmail`, RFC822 parsing, attachment extraction, save/display, local message-ID scanning, `list_attachments()`, `open_file_with_system()` |
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
config_cmd/ --> config, imap_client
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
| `email sync [--limit N] [--mailbox ...] [--reconcile] [--dry-run]` | Additive sync (Message-ID dedup); `--reconcile` detects server moves/deletes; `--dry-run` shows what would change without modifying files |
| `email watch [--mailbox] [--timeout]` | Watch mailbox for changes via IMAP IDLE |
| `email archive <file>` | Archive an inbox email (server + local) |
| `email delete <file>` | Delete an inbox email (server + local) |
| `email open <file>` | Open an attachment from an email in the default application (fuzzy-select if multiple) |
| `email save <file> [--output <dir>]` | Save attachment(s) from an email to a directory (multi-select, default: current dir) |
| `email search <query>` | Search emails on the IMAP server; supports `from:`, `to:`, `subject:`, `body:`, `since:`, `before:`, `in:` prefixes; `--mailbox`, `--limit` (`-n`), `--full` flags |
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

## TUI (v0.6.0)

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
| `o` | Open attachment(s) of the selected email in the default application |
| `O` | Save attachment(s) to a chosen directory (multi-select + dir picker with zoxide/browser) |
| `b` | Open the HTML version of the selected email in the browser |
| `S` | Open server search overlay |
| `!` | Toggle activity log panel |
| `Tab`/`Shift-Tab` | Cycle pane focus |
| `?` | Help overlay |
| `q` | Quit |

### Activity Log Panel

A persistent panel showing timestamped status entries for all background operations. Toggled with `!`. Capped at 100 entries (`STATUS_LOG_CAPACITY`). Each entry carries a `StatusLevel` (`Info`, `Success`, `Warning`, `Error`, `Progress`) used for color coding. Entries are written via `App::push_status()` (raw) or the convenience wrappers `set_status()` and `set_status_level()`.

### Server Search Overlay

Activated with `S`. A full-screen overlay with two focus modes (`SearchOverlayFocus::Input` / `SearchOverlayFocus::List`) toggled with `Tab`/`Down`.

**Query syntax** (parsed by `imap_client::parse_search_query()`):

| Prefix | Maps to |
|--------|---------|
| `from:` | IMAP FROM |
| `to:` | IMAP TO |
| `cc:` | IMAP CC |
| `subject:` | IMAP SUBJECT |
| `body:` | IMAP BODY |
| `since:` | IMAP SINCE (YYYY-MM-DD) |
| `before:` | IMAP BEFORE (YYYY-MM-DD) |
| `in:` | Routing: restrict to one mailbox (not sent to IMAP) |
| bare text | IMAP TEXT |

Quoted values are supported (e.g., `from:"John Doe"`). The `in:` prefix matches by IMAP server name or role label (case-insensitive).

**Search scope:** `in:` prefix or `--mailbox` flag restricts to one mailbox; otherwise all configured mailboxes with a server name are searched in sequence using a shared IMAP session.

**Result types:** `SearchHit` (raw result from background thread), `SearchResultEntry` (held in App state, includes `saved_path` once the email is written locally), `SearchTarget` (mailbox descriptor used to drive the search).

**Result list keys:** `j`/`k` navigate, `Enter`/`e` open in `$EDITOR` (saves locally first), `r`/`R` reply/reply-all, `w` forward, `a` archive, `b` open HTML in browser, `Esc` close overlay.

### Attachment Picker Overlay

When an email has multiple attachments and `o` is pressed, an `AttachmentPicker` overlay appears listing all files in the `_attachments/` directory. Navigate with `j`/`k`, confirm with `Enter`, dismiss with `Esc`/`q`. For a single attachment, the file is opened immediately without the overlay. Uses `parse::list_attachments()` and `parse::open_file_with_system()` (macOS `open`).

`o` is available in List focus, Headers focus, and Preview focus.

### Save Attachment Flow

Press `O` on an email with attachments to save them to disk. The flow is:

1. **Attachment picker (save mode)**: Multi-select with `Space` (checkbox icons), `Ctrl+a` to toggle all, `Enter` to confirm. For a single attachment, skips directly to the dir picker.
2. **Directory picker**: Two modes toggled with `Tab`:
   - **Zoxide mode** (default if `zoxide` is installed): Type to search, results from `zoxide query --list`, `Down`/`Up` to navigate, `Enter` to confirm.
   - **Browser mode**: Navigate filesystem directories. `j`/`k` to move, `Enter` on `[ Save here ]` to confirm, `Enter` on a directory to descend, `h`/`Backspace` to go up, `~` for home.
3. Files are copied via `parse::save_attachment()` which handles filename conflicts by appending `_1`, `_2`, etc.

Last-used directory is remembered for the session (pre-filled next time). Default: `~/Downloads/`.

`O` is available in List focus, Headers focus, and Preview focus.

### Open HTML in Browser

Press `b` on a selected email (List, Preview, or Search overlay) to open its `.html` companion file in the system default browser via `open_file_with_system()`. Reports "No HTML version available" if the `.html` file does not exist.

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

1. **Test the app** -- Run the binary yourself to verify the change works as expected.
2. **Update the binary** -- If it works, install the updated binary via **`./scripts/install.sh`** (NOT `cargo install --path .` directly). The wrapper runs `cargo install` and then re-signs the binary with a stable dev cert so the Keychain stops re-prompting for every `smtp-password-*` / `imap-password-*` entry at TUI startup. On Linux/Windows the codesign step is a no-op. See `CONTRIBUTING.md` for the one-time Keychain setup. Extra args are forwarded to cargo (e.g. `./scripts/install.sh --locked`).
3. **Update `roadmap.md`** -- After every code change, update the roadmap: move completed items from the backlog to the resolved section, strike through done items, and remove entries that are no longer relevant. Keep the backlog accurate and current.
