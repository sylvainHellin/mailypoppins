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

Single-file project: all code lives in `src/main.rs`.

## Commands

| Command | Description |
|---------|-------------|
| `email <file>` | Preview a draft (dry-run, default) |
| `email send <file>` | Send an approved email |
| `email send-approved [dir]` | Batch send all approved drafts |
| `email list [dir]` | List drafts grouped by status |
| `email validate [file\|dir]` | Validate frontmatter and fields |
| `email mark-approved <file>` | Change status from draft to approved |
| `email new <name>` | Create a new draft from template |
| `email reply [file] [--all]` | Create a reply draft from a received email |
| `email list-mailboxes` | List available IMAP mailboxes |
| `email fetch [filters]` | Fetch emails via IMAP |
| `email sync [--limit N] [--mailbox ...]` | Sync local mailbox folders with IMAP server |
| `email browse [--view]` | Interactive fuzzy-finder email browser |

## Configuration

- **`.env`** — SMTP/IMAP credentials, directory paths (`DRAFTS_DIR`, `SENT_DIR`, `INBOX_DIR`, `ARCHIVE_DIR`). Gitignored.
- **`DRAFTS_DIR` resolution** — `list`, `send-approved`, and `reply` auto-resolve the drafts directory via `resolve_drafts_dir`: explicit CLI arg → `DRAFTS_DIR` env var → `"."` fallback.
- **`config.toml`** — Email formatting (font family/size), signature definitions. Searched up to 3 parent directories.

## Logging

Logs are written to `~/.email-cli/logs/email-cli-YYYY-MM-DD.log` (append mode, one file per day). Logging is initialized at startup via `init_logging()` and is non-fatal — if the log directory or file can't be created, a warning is printed and execution continues.

**Log levels used:**
- `INFO`: Command invoked, email sending started, per-recipient success, IMAP operations, archive operations
- `WARN`: Partial send failures, missing config fallbacks
- `ERROR`: SMTP failures (per recipient), IMAP connection failures, invalid addresses
- `DEBUG`: Config paths, recipient list details

## Per-Recipient Sending

`send_email` sends to each recipient individually via `send_raw` with single-recipient envelopes, while preserving visible To/Cc headers for all recipients. This provides per-recipient success/failure tracking. Recipients are deduplicated by address. Return type is `SendResult` containing `Vec<RecipientResult>` (address, role, success, error).

**Caller behavior:**
- **All succeeded** → mark as sent
- **Partial success** → mark as sent + warn (re-sending would duplicate to successful recipients; log file provides audit trail)
- **All failed** → return error, do not mark as sent

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

Status workflow: `draft` → `approved` (via `mark-approved`) → `sent` (via `send`). Inbox emails: `inbox` → `archived` (via archive action in browse).

### Reply Drafts & Signature Placement

Reply drafts (created via `email reply`) include a `{{SIGNATURE}}` placeholder between the reply area and the quoted conversation. When sent, `markdown_to_html` replaces this placeholder with the signature HTML, ensuring the signature appears between the reply and the quoted text. If the placeholder is removed, the signature falls back to being appended at the end (same as regular drafts).

## Output Conventions

- `colored` crate for terminal styling
- `✓` green for success, `✗` red for errors, `⚠` yellow for warnings, `ℹ` blue for info
- Header keys in `fetch` display use distinct colors: From (green), To/Cc (blue), Subject (yellow), Date (magenta)

## Browse Keybindings

The `browse` command uses skim (fuzzy finder) with context-specific keybindings:

- **Global:** `Tab` (next mailbox: Inbox → Drafts → Sent → Archive), `Ctrl+r` (refresh), `Ctrl+d` (delete local file), `Esc` (quit)
- **Inbox:** `Enter` (open), `Ctrl+y` (reply), `Ctrl+o` (reply-all), `Ctrl+s` (archive)
- **Drafts:** `Enter` (open), `Ctrl+g` (approve), `Ctrl+x` (send)
- **Sent:** `Enter` (open)
- **Archive:** `Enter` (open)

Note: `Ctrl+a/e` are reserved by skim (emacs line editing). Avoid binding them.

## After Each Change

1. **Test the app** — Run the binary yourself to verify the change works as expected.
2. **Update the binary** — If it works, install the updated binary via `cargo install --path .` so it's available on the machine.
