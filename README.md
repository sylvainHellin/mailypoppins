# email

A CLI tool for sending emails from Markdown drafts with YAML frontmatter. Supports IMAP fetch/sync/archive and an auditable draft workflow (`draft -> approved -> sent`).

## Installation

```bash
# Build and install to ~/.cargo/bin/
cargo install --path .
```

## Setup

Run the interactive setup wizard:

```bash
email config init
```

This will:
1. Prompt for SMTP and IMAP credentials
2. Test the connections
3. Discover server mailboxes and assign roles (inbox, archive, sent)
4. Write the config to `~/.config/email/config.toml`
5. Store passwords securely in the OS keyring (macOS Keychain, Windows Credential Manager, or Linux Secret Service)

### Configuration file

All configuration lives in `~/.config/email/config.toml`:

```toml
[email]
font_family = "Helvetica, Arial, sans-serif"
font_size = "12pt"
include_signature = true

[smtp]
host = "postout.lrz.de"
port = 587
username = "your-id"
default_from = "your.name@example.com"

[imap]
host = "imap.example.com"    # Falls back to smtp.host if omitted
port = 993
username = ""                # Falls back to smtp.username if omitted

[directories]
root = "~/notes/email"       # Base path for all local mailbox directories
drafts = "drafts"            # Relative to root, or absolute

[mailboxes.inbox]
server = "INBOX"
local = "inbox"              # Relative to root

[mailboxes.archive]
server = "Archive"
local = "archive"

[mailboxes.sent]
server = "Sent Items"
local = "sent"

[[mailboxes.extra]]          # Additional mailboxes to sync
server = "Projects"
local = "projects"

[signatures]
default = "work"

[signatures.work]
name = "Work Signature"
path = "~/notes/email/signatures/work.html"

[signatures.personal]
name = "Personal"
path = "~/notes/email/signatures/personal.html"
```

Passwords are **never** stored in the config file. They live in the OS keyring under keys `smtp-password` and `imap-password`. IMAP password falls back to SMTP password if not set separately.

### Config commands

```bash
email config init           # Interactive setup wizard
email config show           # Display config (passwords masked)
email config set-password   # Store SMTP or IMAP password in keyring
email config path           # Print config file path
```

## Email Draft Format

Files use Markdown with YAML frontmatter:

```markdown
---
to: recipient@example.com
cc: optional@example.com       # Optional
bcc: hidden@example.com        # Optional
subject: "Your subject line"
status: draft                  # draft | approved | sent
from: alternate@example.com   # Optional, overrides default_from
reply_to: reply@example.com   # Optional
attachments:                   # Optional
  - /path/to/file.pdf
---

Email body in **Markdown** format.
```

### Status workflow

```
draft  --[mark-approved]-->  approved  --[send]-->  sent
```

Only approved emails can be sent. The `send` command rejects drafts that haven't been approved.

Inbox emails follow a separate flow: `inbox -> archived` (via the `archive` command).

### After sending

The file is updated in-place with `status: sent`, `sent_at` timestamp, `sent_via` version, and `message_id`. If a sent directory is configured, the file is moved there.

## Commands

### Global options

```bash
-s, --signature <name>   Use a specific signature
    --no-signature       Skip signature entirely
```

### Drafts

```bash
email <file>                    # Preview a draft (dry-run)
email new <name>                # Create a new draft from template
email list [dir]                # List drafts grouped by status
email validate <file|dir>       # Validate frontmatter
email mark-approved <file>      # Mark draft as approved
email send <file> [-y]          # Send a single approved email
email send-approved [dir] [-y]  # Send all approved emails in directory
email reply [file] [--all]      # Create a reply draft from a received email
```

### IMAP

```bash
email fetch [filters]           # Fetch emails from server
email sync [options]            # Sync local folders with server
email watch [options]           # Watch mailbox for changes (IMAP IDLE)
email list-mailboxes            # List available server mailboxes
email archive <file>            # Archive an inbox email (server + local)
email delete <file>             # Delete an inbox email (server + local)
```

### Fetch options

| Option | Description |
|--------|-------------|
| `--from <addr>` | Filter by sender |
| `--to <addr>` | Filter by recipient |
| `--cc <addr>` | Filter by CC |
| `--subject <text>` | Subject contains |
| `--body <text>` | Body contains |
| `--since <YYYY-MM-DD>` | Emails since date |
| `--before <YYYY-MM-DD>` | Emails before date |
| `-n, --limit <N>` | Max results (default: 10) |
| `--full` | Show full body instead of preview |
| `--mailbox <name>` | Mailbox name (default: INBOX) |

### Sync options

| Option | Description |
|--------|-------------|
| `-n, --limit <N>` | Max messages per mailbox (default: 50) |
| `--mailbox <name>...` | Mailboxes to sync (default: all configured) |
| `--reconcile` | Also detect server-side moves and deletes |

Sync is additive by default (new emails only, deduped by Message-ID). With `--reconcile`, it also detects emails moved between INBOX and Archive on the server, and removes locally deleted emails.

### Watch options

| Option | Description |
|--------|-------------|
| `--mailbox <name>` | Mailbox to watch (default: INBOX) |
| `--timeout <seconds>` | Timeout in seconds |

Exit codes: `0` = mailbox changed, `1` = error, `2` = timed out.

## Signatures

Signature files are HTML snippets appended after the email body:

```html
<div style="font-family: Helvetica, Arial, sans-serif; font-size: 12pt;">
<p style="margin: 0;">
<b>Your Name</b><br>
Title | Organization
</p>
</div>
```

Reply drafts include a `{{SIGNATURE}}` placeholder between the reply area and quoted text. When sent, the placeholder is replaced with the signature HTML.

## Example workflow

```bash
# 1. Set up (once)
email config init

# 2. Sync inbox
email sync

# 3. Create and review drafts
email new meeting-followup
email list ~/notes/email/drafts/
email ~/notes/email/drafts/2026-03-01_meeting-followup.md   # preview

# 4. Approve and send
email mark-approved ~/notes/email/drafts/2026-03-01_meeting-followup.md
email send ~/notes/email/drafts/2026-03-01_meeting-followup.md

# 5. Or batch send all approved
email send-approved ~/notes/email/drafts/
```

## Troubleshooting

### "Config file not found"
Run `email config init` to create the config file.

### "Password not found in keyring"
Run `email config set-password` to store your password.

### "Email not approved for sending"
Run `email mark-approved <file>` first.

### "SMTP authentication failed"
Check your credentials with `email config show` and re-run `email config set-password smtp` if needed.

### "Signature file not found"
Check the path in your config. Paths support `~` expansion.

## Building

```bash
cargo build --release           # Build
cargo install --path .          # Install to ~/.cargo/bin/
```
