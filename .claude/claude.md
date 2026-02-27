# Email CLI - Project Instructions

## Overview

CLI tool for managing email drafts written in Markdown with YAML frontmatter. Supports sending via SMTP, fetching via IMAP, archiving via IMAP, and an auditable draft workflow (`draft → approved → sent`). Inbox emails can be archived (`inbox → archived`).

## Tech Stack

- **Language:** Rust (Edition 2021)
- **CLI parsing:** `clap` (derive macros)
- **SMTP:** `lettre` (async, TLS)
- **IMAP:** `imap` + `native-tls`
- **Email parsing:** `mailparse`
- **HTML → plaintext:** `html2text`
- **Frontmatter:** `gray_matter` + `serde_yaml`
- **Markdown → HTML:** `pulldown-cmark`
- **Terminal output:** `colored`
- **Async runtime:** `tokio`
- **Error handling:** `anyhow`
- **Logging:** `log` + `simplelog` (file-based, daily rotation)

## Architecture

Multi-module project:

| File | Lines | Responsibility |
|------|-------|---------------|
| `src/main.rs` | ~890 | CLI definition (`Cli`, `Commands`), `main()`, command dispatch |
| `src/types.rs` | ~70 | Shared types: `EmailStatus`, `EmailFrontmatter`, `EmailDraft`, `InboxFrontmatter` |
| `src/config.rs` | ~300 | Config loading, env resolution, logging, `SmtpConfig`, `ImapConfig` |
| `src/parse.rs` | ~370 | Email body parsing, slugifying, `FetchedEmail`, save/display |
| `src/imap_client.rs` | ~530 | All IMAP operations: fetch, list, append, watch, archive, delete |
| `src/draft.rs` | ~480 | Draft parsing, validation, preview, reply creation, status updates |
| `src/send.rs` | ~400 | `RecipientRole`, `SendResult`, `markdown_to_html`, `send_email` |
| `src/sync.rs` | ~180 | Local file scanning, reconciliation, mailbox dir resolution |

Dependency graph (no cycles):
```
types      --> (none)
config     --> (none)
parse      --> types
imap_client--> config, parse, types
draft      --> config, parse, types
send       --> config, types
sync       --> (none, only external crates)
main       --> all modules
```

## Commands

| Command | Description |
|---------|-------------|
| `email <file>` | Preview a draft (dry-run, default) |
| `email send <file> [--yes]` | Send an approved email (`-y` skips confirmation) |
| `email send-approved [dir] [--yes]` | Batch send all approved drafts (`-y` skips confirmation) |
| `email list [dir]` | List drafts grouped by status |
| `email validate [file\|dir]` | Validate frontmatter and fields |
| `email mark-approved <file>` | Change status from draft to approved |
| `email new <name>` | Create a new draft from template |
| `email reply [file] [--all]` | Create a reply draft from a received email |
| `email list-mailboxes` | List available IMAP mailboxes |
| `email fetch [filters]` | Fetch emails via IMAP |
| `email sync [--limit N] [--mailbox ...] [--reconcile]` | Additive sync (Message-ID dedup); `--reconcile` detects server moves/deletes |
| `email watch [--mailbox] [--timeout]` | Watch mailbox for changes via IMAP IDLE |
| `email archive <file>` | Archive an inbox email (server + local) |
| `email delete <file>` | Delete an inbox email (server + local) |

## Configuration

- **`.env`** — SMTP/IMAP credentials, directory paths (`DRAFTS_DIR`, `SENT_DIR`, `INBOX_DIR`, `ARCHIVE_DIR`), and `SENT_MAILBOX` (IMAP Sent folder name, default: `"Sent"`). Gitignored.
- **`DRAFTS_DIR` resolution** — `list`, `send-approved`, and `reply` auto-resolve the drafts directory via `resolve_drafts_dir`: explicit CLI arg → `DRAFTS_DIR` env var → `"."` fallback.
- **`SENT_MAILBOX`** — Configures the IMAP Sent folder name for IMAP APPEND and sync. Defaults to `"Sent"`. Some providers use `"Sent Items"`, `"INBOX.Sent"`, etc.
- **`config.toml`** — Email formatting (font family/size), signature definitions. Searched up to 3 parent directories.

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

**IMAP APPEND:** After a successful send, the raw message is appended to the server-side Sent folder (configured via `SENT_MAILBOX`) with `\Seen` flag. This is best-effort -- APPEND failure is logged as a warning but does not fail the send. The `message_id` is stored in the local sent file's frontmatter for sync dedup.

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

## Output Conventions

- `colored` crate for terminal styling
- `✓` green for success, `✗` red for errors, `⚠` yellow for warnings, `ℹ` blue for info
- Header keys in `fetch` display use distinct colors: From (green), To/Cc (blue), Subject (yellow), Date (magenta)

## Sync Reconciliation

`email sync` is additive by default. Pass `--reconcile` to also detect server-side moves and deletes:

1. **Additive pass** (always): Fetch last N emails per mailbox (default 50), save new ones locally with Message-ID dedup.
2. **Reconciliation pass** (`--reconcile` only): Lightweight fetch of just Message-ID headers from INBOX and Archive (no body download). Compares local files against server sets. Emails moved between INBOX and Archive on the server are moved locally (with status update). Emails deleted on the server are removed locally. Companion `.html` files are moved/deleted alongside `.md` files.

**Important constraints:**
- The Sent directory is never reconciled -- locally-authored files are the source of truth.
- Only INBOX and Archive participate in reconciliation.

## After Each Change

1. **Test the app** — Run the binary yourself to verify the change works as expected.
2. **Update the binary** — If it works, install the updated binary via `cargo install --path .` so it's available at `~/.cargo/bin/email`.
