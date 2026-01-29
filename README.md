# email

A CLI tool for sending emails from Markdown drafts with YAML frontmatter. Designed for controlled, auditable bulk email workflows.

## Installation

```bash
# Build from source
cargo build --release

# Install to PATH
cp target/release/email ~/.local/bin/
```

## Configuration

### SMTP Configuration (`.env`)

Create a `.env` file in your working directory or vault:

```bash
SMTP_HOST=postout.lrz.de
SMTP_PORT=587
SMTP_USERNAME=your-tum-id
SMTP_PASSWORD=your-password
DEFAULT_FROM=your.name@tum.de
DRAFTS_DIR="~/path/to/email/drafts"  # Optional
SENT_DIR="~/path/to/email/sent"      # Optional: where to move sent emails
INBOX_DIR="~/path/to/email/inbox"    # Optional: where to save fetched emails
```

### IMAP Configuration (`.env`)

For the `fetch` command, add IMAP settings:

```bash
IMAP_HOST=xmail.mwn.de          # Required
IMAP_PORT=993                    # Default: 993 (SSL/TLS)
IMAP_USERNAME=your-tum-id        # Falls back to SMTP_USERNAME
IMAP_PASSWORD=your-password      # Falls back to SMTP_PASSWORD
```

For TUM, the IMAP server is `xmail.mwn.de:993` with SSL/TLS, using TUM-Kennung + password (same as SMTP).

**Important**: If paths contain spaces, wrap them in quotes:
```bash
SENT_DIR="~/Library/Mobile Documents/iCloud~md~obsidian/Documents/SecondBrain/email/sent"
```

**Note**: The CLI searches for `.env` files in the current directory and up to 3 parent directories from the file being processed.

### Email Formatting (`config.toml`)

Create a `config.toml` file for email formatting and signatures:

```toml
[email]
font_family = "Helvetica, Arial, sans-serif"
font_size = "12pt"
include_signature = true  # default, can override with --no-signature

[signatures]
default = "work"  # signature to use by default

[signatures.work]
name = "Work Signature"
path = "signatures/work.html"

[signatures.personal]
name = "Personal"
path = "signatures/personal.html"
```

The CLI searches for `config.toml` in:
- The directory containing the email file
- Parent directories (up to 3 levels)
- `email/config.toml` relative to parent directories

Signature paths can be relative (to the config.toml location) or absolute.

### Signature Files

Signature files are HTML snippets that get appended after the email body:

```html
<div style="font-family: Helvetica, Arial, sans-serif; font-size: 12pt; color: #000000;">
<p style="margin: 0;">
<b>Your Name</b><br>
Your Title | Your Organization
</p>
<p style="margin: 16px 0 0 0;">
Mail: <a href="mailto:you@example.com">you@example.com</a><br>
Web: <a href="https://example.com">example.com</a>
</p>
</div>
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
from: alternate@tum.de         # Optional, overrides DEFAULT_FROM
reply_to: reply@example.com    # Optional
attachments:                   # Optional
  - /path/to/file.pdf
---

# Email Body

Your email content in **Markdown** format.

- Lists work
- Links work: [Example](https://example.com)
- Basic formatting works
```

## Commands

### Global Options

These options work with all commands:

```bash
-s, --signature <name>   Use a specific signature from config.toml
    --no-signature       Skip signature entirely (overrides config)
```

### Preview (Dry Run)

```bash
email path/to/draft.md
email --signature work path/to/draft.md
email --no-signature path/to/draft.md
```

Shows what would be sent without actually sending. Displays:
- From, To, Cc, Subject
- Body preview (first 500 chars)
- Font and signature settings
- Validation status

### List Drafts

```bash
email list path/to/drafts/
```

Lists all `.md` files with email frontmatter, showing:
- Status (draft/approved/sent)
- Filename
- Recipient

### Validate

```bash
email validate path/to/draft.md
email validate path/to/drafts/
```

Checks YAML syntax and required fields. Returns exit code 1 if any invalid.

### Mark as Approved

```bash
email mark-approved path/to/draft.md
```

Changes `status: draft` to `status: approved` in the file.

**How it works under the hood:**
1. Reads the file and parses YAML frontmatter
2. Validates current status is `draft` (errors if already `sent`)
3. Creates a new frontmatter with `status: approved`
4. Serializes the updated frontmatter back to YAML
5. Reconstructs the file: `---\n{yaml}\n---\n\n{body}`
6. Writes the updated content back to the same file

The file is modified in-place. The body content is preserved exactly.

### Send Single Email

```bash
email send path/to/draft.md
```

Sends a single email. Requirements:
- Status must be `approved`
- Shows preview and asks for confirmation
- After sending: updates status to `sent`, adds `sent_at` timestamp
- If `SENT_DIR` is set, moves file there

### Send All Approved

```bash
email send-approved path/to/drafts/
```

Sends all emails with `status: approved` in the directory:
1. Lists all approved emails
2. Asks for confirmation
3. Sends each one sequentially
4. Reports success/failure count

### Fetch Emails (IMAP)

```bash
email fetch                                          # Latest 10 from INBOX
email fetch --from boss@company.com --since 2025-01-01
email fetch --subject "quarterly" --limit 5 --full
email fetch --mailbox "Sent"
```

Connects to an IMAP server and searches emails by criteria. Options:

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
| `--full` | Show full body (default: 300-char preview) |
| `--mailbox <name>` | Mailbox name (default: INBOX) |

Requires IMAP configuration in `.env` (see above).

**Inbox saving:** If `INBOX_DIR` is set in `.env`, fetched emails are automatically saved as `.md` files with YAML frontmatter (matching the draft/sent format). Files are named `YYYY-MM-DD_<subject-slug>.md`. Duplicate emails (by `Message-ID`) are skipped on subsequent fetches.

### Create New Draft

```bash
email new draft-name
```

Creates a new `.md` file with empty YAML frontmatter template.

## Status Workflow

```
┌─────────┐    mark-approved    ┌──────────┐      send       ┌────────┐
│  draft  │ ─────────────────▶ │ approved │ ──────────────▶ │  sent  │
└─────────┘                     └──────────┘                  └────────┘
     │                                                             │
     │              Only approved emails can be sent               │
     └─────────────────────────────────────────────────────────────┘
```

**Safety features:**
- `send` commands only work on `approved` files
- Dry-run by default (use `send` subcommand to actually send)
- Confirmation prompt before sending
- Bulk send shows list and asks for confirmation

## Fish Shell Helpers

If you have the fish functions installed (`~/.config/fish/functions/email.fish`):

```fish
email-list           # List all drafts
email-validate       # Validate all drafts
email-preview <file> # Preview single draft
email-approve <file> # Mark as approved
email-approve-all    # Approve all drafts
email-send-approved  # Validate then send all approved
email-stats          # Show counts
email-help           # Show all commands
```

## Examples

### Complete Workflow

```bash
# 1. Create drafts (manually or via Claude Code)
# 2. Review and validate
email list email/drafts/
email validate email/drafts/

# 3. Preview a specific draft
email email/drafts/2026-01-15_test.md

# 4. Approve drafts you want to send
email mark-approved email/drafts/2026-01-15_test.md

# 5. Send approved emails
email send-approved email/drafts/
```

### Test Your Setup

```bash
# Create a test email to yourself
# Mark it as approved
email mark-approved email/drafts/2026-01-15_test-email.md

# Send it
email send email/drafts/2026-01-15_test-email.md
```

## File Changes After Sending

Before:
```yaml
---
to: recipient@example.com
subject: "Hello"
status: approved
---
```

After:
```yaml
---
to: recipient@example.com
subject: "Hello"
status: sent
sent_at: "2026-01-15T14:30:00Z"
sent_via: "email v0.1.0"
---
```

If `SENT_DIR` is configured, the file is also moved to that directory.

## Troubleshooting

### "Could not load SMTP config"
Create a `.env` file with your SMTP credentials.

### "Email not approved for sending"
Run `email mark-approved <file>` first.

### "SMTP authentication failed"
Check your username/password in `.env`. For TUM, use your TUM-ID.

### "Invalid email address"
Check the `to:` field has a valid email format.

### "Signature file not found"
Check that the path in `config.toml` is correct. Paths are relative to the config.toml location.

### "IMAP_HOST not set in environment"
Add `IMAP_HOST` to your `.env` file. For TUM, use `xmail.mwn.de`.

### "IMAP login failed"
Check your IMAP credentials. If `IMAP_USERNAME`/`IMAP_PASSWORD` are not set, they fall back to `SMTP_USERNAME`/`SMTP_PASSWORD`.

### Emails don't have signature
- Check `include_signature = true` in `config.toml`
- Check `[signatures] default = "name"` is set
- Verify signature file exists at the specified path
- Use `--signature <name>` to explicitly specify one

## Building & Releasing

### Building from Source

```bash
# Build for your current architecture
cargo build --release

# Binary will be at target/release/email
```

### Cross-compiling for macOS

To build for both Apple Silicon and Intel Macs:

```bash
# Add Intel target (if on Apple Silicon)
rustup target add x86_64-apple-darwin

# Or add Apple Silicon target (if on Intel)
rustup target add aarch64-apple-darwin

# Build for both architectures
cargo build --release                              # Native
cargo build --release --target x86_64-apple-darwin # Intel
cargo build --release --target aarch64-apple-darwin # Apple Silicon
```

### Creating a Release

1. **Prepare release artifacts:**

```bash
mkdir -p release

# Copy binaries (adjust paths based on your architecture)
cp target/release/email release/email-darwin-arm64
cp target/x86_64-apple-darwin/release/email release/email-darwin-x86_64

# Create compressed archives
cd release
tar -czvf email-v0.1.0-darwin-arm64.tar.gz email-darwin-arm64
tar -czvf email-v0.1.0-darwin-x86_64.tar.gz email-darwin-x86_64

# Generate checksums
shasum -a 256 *.tar.gz > SHA256SUMS.txt
```

2. **Commit your changes** (but not the release artifacts):

```bash
# Make sure release/ and target/ are in .gitignore
git add .
git commit -m "Prepare for v0.1.0 release"
git push
```

3. **Create GitHub release:**

```bash
# Install GitHub CLI if needed
brew install gh
gh auth login

# Create release with assets
gh release create v0.1.0 \
  release/email-v0.1.0-darwin-arm64.tar.gz \
  release/email-v0.1.0-darwin-x86_64.tar.gz \
  release/SHA256SUMS.txt \
  --title "v0.1.0" \
  --notes "Initial release"
```

The `gh release create` command will automatically create and push the git tag.
