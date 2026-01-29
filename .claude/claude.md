# Email CLI - Project Instructions

## Overview

CLI tool for managing email drafts written in Markdown with YAML frontmatter. Supports sending via SMTP, fetching via IMAP, and an auditable draft workflow (`draft → approved → sent`).

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

## Configuration

- **`.env`** — SMTP/IMAP credentials, directory paths (`DRAFTS_DIR`, `SENT_DIR`, `INBOX_DIR`). Gitignored.
- **`DRAFTS_DIR` resolution** — `list`, `send-approved`, and `reply` auto-resolve the drafts directory via `resolve_drafts_dir`: explicit CLI arg → `DRAFTS_DIR` env var → `"."` fallback.
- **`config.toml`** — Email formatting (font family/size), signature definitions. Searched up to 3 parent directories.

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

Status workflow: `draft` → `approved` (via `mark-approved`) → `sent` (via `send`).

### Reply Drafts & Signature Placement

Reply drafts (created via `email reply`) include a `{{SIGNATURE}}` placeholder between the reply area and the quoted conversation. When sent, `markdown_to_html` replaces this placeholder with the signature HTML, ensuring the signature appears between the reply and the quoted text. If the placeholder is removed, the signature falls back to being appended at the end (same as regular drafts).

## Output Conventions

- `colored` crate for terminal styling
- `✓` green for success, `✗` red for errors, `⚠` yellow for warnings, `ℹ` blue for info
- Header keys in `fetch` display use distinct colors: From (green), To/Cc (blue), Subject (yellow), Date (magenta)

## After Each Change

1. **Test the app** — Run the binary yourself to verify the change works as expected.
2. **Update the binary** — If it works, install the updated binary via `cargo install --path .` so it's available on the machine.
