# mailypoppins

I live in the terminal and write everything in Neovim, except my email.
No mainstream client let me read and edit a message in Neovim, and the system-wide Vim layers on the Mac never worked well.
I also wanted my AI agents to help with email: tell me which messages still wait for an answer, and check my drafts for typos, missing points and language before I send them.
With Copilot and the AI extensions of existing clients, that meant pasting my text into a chat, pasting the corrections back, and never seeing what the agent had changed.
So I wrote my own client.
In mailypoppins every draft is a Markdown file that I write in Neovim and my agent edits in place.
Because it is a plain file, I see the agent's changes as a diff, and nothing is sent before I approve it.

![The mailypoppins desktop app](docs/screenshots/desktop-app.png)

## How it works

mailypoppins is a daemon with three clients.
The daemon owns the accounts, syncs mail into a local store and runs the send queue.
The clients talk to it over a Unix socket and offer the same operations:

- the `mp` command line, for scripts and AI agents;
- the terminal UI, which `mp` opens when run without arguments;
- the desktop app for macOS.

A change made in one client shows up in the others at once.

## Features

- IMAP and SMTP accounts, and Microsoft 365 through OAuth2 or the Graph API.
- Drafts as Markdown files with YAML frontmatter, opened in your `$EDITOR`; the desktop app runs Neovim inside its window.
- An approval step before sending, an undo-send window and a durable outbox.
- Conversations that gather a thread from Inbox, Sent, Archive and Drafts.
- One search syntax for the server and the local store.
- Calendar invitations: send them, answer them and see them in an agenda.
- A contacts index built from your mail, signatures as Markdown files, and hooks that run a command on matching incoming mail.
- Vim keys, a which-key popup and a command palette in the terminal UI and the desktop app.

## Getting started

Install the command line and the terminal UI with Homebrew, on macOS or Linux:

```sh
brew tap sylvainHellin/mailypoppins
brew install mailypoppins
```

The desktop app is a DMG for Apple silicon on the [releases page](https://github.com/sylvainHellin/mailypoppins/releases); it carries its own `mp`.
The app is not signed yet, so macOS blocks its first launch: the [getting started guide](https://mailypoppins.com/getting-started/#mac-app) shows how to allow it and how to use the app's `mp` from a terminal.
To build the command line and the terminal UI from source, run `cargo install --path .` in a clone; [clients/desktop/README.md](clients/desktop/README.md) covers the app.

Then add an account and open the terminal UI:

```sh
mp config init
<<<<<<< Updated upstream
```

This will:
1. Prompt for SMTP and IMAP credentials
2. Test the connections
3. Discover server mailboxes and assign roles (inbox, archive, sent)
4. Write the config to `~/.config/mailypoppins/config.toml`
5. Store passwords securely in the OS keyring (macOS Keychain, Windows Credential Manager, or Linux Secret Service)

### Configuration file

All configuration lives in `~/.config/mailypoppins/config.toml`:

```toml
[email]
font_family = "Helvetica, Arial, sans-serif"
font_size = "16px"
include_signature = true
send_hold_secs = 20          # Undo-send window in the TUI (0 = hand off immediately)

[smtp]
host = "postout.lrz.de"
port = 587
username = "your-id"
default_from = "your.name@example.com"

[imap]
host = "imap.example.com"    # Falls back to smtp.host if omitted
port = 993
username = ""                # Falls back to smtp.username if omitted

[mailboxes.inbox]
server = "INBOX"             # IMAP folder name; the local directory is
                             # always <data_dir>/accounts/<name>/inbox/

[mailboxes.archive]
server = "Archive"

[mailboxes.sent]
server = "Sent Items"

[[mailboxes.extra]]          # Additional mailboxes to sync
server = "Projects"
```

The file has no signature keys: each signature is a Markdown file in `~/.config/mailypoppins/signatures/`, and the account's default is recorded in the app's own state file. See [Signatures](#signatures).

Passwords are **never** stored in the config file. They live in the OS keyring under keys `smtp-password` and `imap-password`. IMAP password falls back to SMTP password if not set separately.

### Where mailypoppins stores data

All mail, drafts, contacts cache, OAuth2 tokens, and logs live under a single OS-conventional app data directory:

| OS                  | Path                                                  |
|---------------------|-------------------------------------------------------|
| macOS               | `~/Library/Application Support/mailypoppins/`         |
| Linux (incl. WSL)   | `$XDG_DATA_HOME/mailypoppins/` (def. `~/.local/share/mailypoppins/`) |

Layout:

```
mailypoppins/
  accounts/<name>/{inbox,archive,sent,drafts,<extra>}/    # mail tree
  accounts/<name>/contacts-cache.json                     # contacts index
  tokens/<name>.enc                                       # OAuth2 tokens
  state.json                                              # app state (default signature per account)
  logs/mailypoppins-YYYY-MM-DD.log
```

The location is not user-configurable per account. To make the mail tree visible inside an Obsidian vault (or any other directory), symlink `accounts/<name>/` into the desired location. Override the root for tests / portable installs with `MAILYPOPPINS_DATA_DIR=/some/path`.

### OAuth2 / Exchange Online

For Microsoft 365 accounts using OAuth2 (IMAP/SMTP) or Graph API:

1. Register an app in Azure Entra ID with delegated permissions (`Mail.Read`, `Mail.ReadWrite`, `Mail.Send`).
2. Add the account to `config.toml` with `auth_method = "oauth2"` (IMAP/SMTP) or `auth_method = "graph"` (Graph API, for tenants that block IMAP/SMTP).
3. Run the device code flow:

```bash
mp config oauth2-login --account <name>
```

Graph accounts require an `[accounts.oauth2]` section with `client_id` and `tenant_id`, and use Graph well-known folder names (`inbox`, `archive`, `sentitems`) instead of IMAP folder names.

### Config commands

```bash
mp config init           # Interactive setup wizard
mp config show           # Display config (passwords masked)
mp config set-password   # Store SMTP or IMAP password in keyring
mp config oauth2-login   # Run OAuth2 device code flow
mp config path           # Print config file path
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

The file is updated in-place with `status: sent`, `sent_at` timestamp, `sent_via` version, and `message_id`.
A send that reached every recipient and was recorded in the outbox then deletes the draft file: the copy that matters lives on the server, where the outbox appends it to Sent and sync reads it back into the local store.
A partial send, or one the outbox store could not record, keeps the file with `status: sent`, because it is the only local copy left.
Sending such a draft again means editing `status:` back to `approved` by hand, since `send` refuses anything that is not approved and `mark-approved` refuses anything already sent; trim the recipient lines first, because a re-send delivers to everyone the file still lists.

## Commands

### TUI

```bash
mp                              # Launch the interactive TUI
```

### Global options

```bash
-A, --account <name>     Account to use (default: first in config; every account for
                         calendar rebuild, contacts rebuild and cutover)
-s, --signature <name>   Use a specific signature
    --no-signature       Skip signature entirely
```

### Drafts

```bash
mp <selector>                   # Preview a draft (dry-run)
mp new <name>                   # Create a new draft from template, print its selector
mp list [--status S] [--json]   # List the account's drafts (draft|approved|sent)
mp validate [<selector>]        # Validate one draft, or every draft of the account
mp mark-approved <selector>     # Mark draft as approved
mp send <selector> [-y]         # Send a single approved email
mp send-approved [--all-accounts] [-y]  # Send every approved draft of the account
mp reply <selector> [--all]     # Create a reply draft from a received email
mp forward <selector>           # Create a forward draft from a received email
mp delete <drafts-selector> [--force]   # Delete a draft (--force for an approved one)
mp delete --sent                # Clear every sent draft of the account
```

A draft selector is `mp://<account>/drafts/<id>`, `drafts/<id>` or `<id>`; a received one is `mp://<account>/<mailbox>/<message-id>`, and `--mailbox <name>` picks the mailbox a short one resolves in.
On a terminal `mp new`, `edit`, `reply`, `forward` and `mark-approved` print `✓ <selector>`; into a pipe they print the bare selector (and `mp save` the bare paths), so `sel=$(mp new note) && mp path "$sel"` works.

### IMAP

```bash
mp fetch [filters]              # Fetch emails from server
mp sync [options]               # Sync local folders with server
mp watch [options]              # Watch mailbox for changes (IMAP IDLE)
mp list-mailboxes               # List available server mailboxes
mp archive <selector>           # Archive a received email (server + local)
mp delete <selector>            # Delete a received email (server + local)
mp search <query> [--json]      # Search emails on the server (--local: the store)
mp open <selector>              # Open an attachment in the default app
mp save <selector> [--output]   # Save attachment(s) to a directory
mp show <selector> [--json]     # Print one received message from the local store (fetched first if missing)
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

A signature is one Markdown file in `~/.config/mailypoppins/signatures/`.
The file name without its extension is the signature's name, so `work.md` is the signature `work`, and creating a signature is creating a file.
The per-account default is recorded in the app's own state file, `<data_dir>/state.json`.

The signature is spliced into the draft body when the draft is created: after the body for a new message, above the quoted content for a reply or forward.
So it is visible and editable while you write, and the same text feeds both the plain-text and the HTML part of the sent mail.

Manage them from the TUI with `cs`, which lists the signatures with the default starred: `Enter` sets or clears the default, `e` opens the selected file in `$EDITOR`, `n` creates one, `r` renames, `d` deletes after a confirmation.

HTML in a signature file is converted to Markdown as it is read, keeping links and line breaks, so a signature exported from another mail client can be pasted in as-is.
An account with no default signature adds nothing.
Use `-s <name>` to pick a signature for one draft, or `--no-signature` to skip it.

Signatures used to live in `config.toml` under `[accounts.signatures.*]`.
The first run after the upgrade copies those entries out to files, keeps the old default, and prints a notice; the tables are dead after that and can be deleted by hand, since mailypoppins never rewrites `config.toml`.

Reply and forward drafts keep a `<!-- mp:quote -->` marker between the reply area and the quoted text.
It carries no text; it marks where the send path splits the body to place the quoted original, and it is stripped from every sent copy.
Drafts written by older versions carry `{{SIGNATURE}}` in that place, which is still read as the same boundary.

## TUI

Running `mp` with no arguments opens a full-screen terminal interface (built on `ratatui`).

Layout: sidebar (mailbox list) | email list | headers + body preview. Adapts to terminal width.

Every mailbox but Drafts lists one row per conversation, with its messages from Inbox, Sent, Archive and Drafts one keypress away, newest first; `tc` switches an account to one row per message, and the TUI remembers the choice in `~/.config/mailypoppins/tui.json`.

Keys are nvim-style mnemonic families: a leader letter opens a which-key popup listing its continuations, and the second key runs the action.
`c` is compose, `t` is thread and attachments, `f` is find and filter, `s` is sync and settings, `g` is go-to, `Space` switches view.
Forget a chord and `:` or `Ctrl+p` opens a command palette over every runnable action by name.

| Key | Action |
|-----|--------|
| `j`/`k`, arrows | Navigate |
| `Enter` | Read the message in the body pane (expands a folded conversation first) |
| `l`/`h` | Expand / fold the conversation |
| `e` | Open in `$EDITOR` (received mail is read-only) |
| `cn` | New draft |
| `cs` | Manage signatures |
| `cr`/`ca`/`cf` | Reply / Reply all / Forward |
| `cA`/`cD` | Approve draft / back to draft (in Drafts or a conversation) |
| `x`/`cX` | Approve and send / Send all approved |
| `a`/`d` | Archive / Delete (with confirmation) |
| `ta`/`to`/`ts` | Attach file to draft / Open attachment / Save attachment |
| `tt`/`tb` | Show conversation / Open HTML in browser |
| `tc` | Conversation view on / off for this account |
| `ff`/`fm` | Search all mail / Filter the current list |
| `ss`/`sS` | Quick sync / Full sync |
| `ga`/`gm` | Account picker / Go to the sidebar |
| `1`-`9` | Jump to mailbox |
| `:` or `Ctrl+p` | Command palette |
| `?` | Help |
| `q` | Quit |

The help overlay (`?`) and `mp dump-keys` print the full catalogue, both generated from the same table the TUI dispatches on.

A send from the TUI is held for `email.send_hold_secs` (default 20) before it reaches the transport: the status line counts down and `u` cancels it, leaving the approved draft in place.
Set it to `0` to hand off immediately. `mp send` and the batch run are never held.

The TUI is a client of the daemon over its Unix socket and spawns no subprocess; the daemon syncs new mail and pushes the changes to every open client.

## Example workflow

```bash
# 1. Set up (once)
mp config init

# 2. Launch the TUI for interactive management
||||||| Stash base
```

This will:
1. Prompt for SMTP and IMAP credentials
2. Test the connections
3. Discover server mailboxes and assign roles (inbox, archive, sent)
4. Write the config to `~/.config/mailypoppins/config.toml`
5. Store passwords securely in the OS keyring (macOS Keychain, Windows Credential Manager, or Linux Secret Service)

### Configuration file

All configuration lives in `~/.config/mailypoppins/config.toml`:

```toml
[email]
font_family = "Helvetica, Arial, sans-serif"
font_size = "16px"
include_signature = true
send_hold_secs = 20          # Undo-send window in the TUI (0 = hand off immediately)

[smtp]
host = "postout.lrz.de"
port = 587
username = "your-id"
default_from = "your.name@example.com"

[imap]
host = "imap.example.com"    # Falls back to smtp.host if omitted
port = 993
username = ""                # Falls back to smtp.username if omitted

[mailboxes.inbox]
server = "INBOX"             # IMAP folder name; the local directory is
                             # always <data_dir>/accounts/<name>/inbox/

[mailboxes.archive]
server = "Archive"

[mailboxes.sent]
server = "Sent Items"

[[mailboxes.extra]]          # Additional mailboxes to sync
server = "Projects"
```

The file has no signature keys: each signature is a Markdown file in `~/.config/mailypoppins/signatures/`, and the account's default is recorded in the app's own state file. See [Signatures](#signatures).

Passwords are **never** stored in the config file. They live in the OS keyring under keys `smtp-password` and `imap-password`. IMAP password falls back to SMTP password if not set separately.

### Where mailypoppins stores data

All mail, drafts, contacts cache, OAuth2 tokens, and logs live under a single OS-conventional app data directory:

| OS                  | Path                                                  |
|---------------------|-------------------------------------------------------|
| macOS               | `~/Library/Application Support/mailypoppins/`         |
| Linux (incl. WSL)   | `$XDG_DATA_HOME/mailypoppins/` (def. `~/.local/share/mailypoppins/`) |

Layout:

```
mailypoppins/
  accounts/<name>/{inbox,archive,sent,drafts,<extra>}/    # mail tree
  accounts/<name>/contacts-cache.json                     # contacts index
  tokens/<name>.enc                                       # OAuth2 tokens
  state.json                                              # app state (default signature per account)
  logs/mailypoppins-YYYY-MM-DD.log
```

The location is not user-configurable per account. To make the mail tree visible inside an Obsidian vault (or any other directory), symlink `accounts/<name>/` into the desired location. Override the root for tests / portable installs with `MAILYPOPPINS_DATA_DIR=/some/path`.

### OAuth2 / Exchange Online

For Microsoft 365 accounts using OAuth2 (IMAP/SMTP) or Graph API:

1. Register an app in Azure Entra ID with delegated permissions (`Mail.Read`, `Mail.ReadWrite`, `Mail.Send`).
2. Add the account to `config.toml` with `auth_method = "oauth2"` (IMAP/SMTP) or `auth_method = "graph"` (Graph API, for tenants that block IMAP/SMTP).
3. Run the device code flow:

```bash
mp config oauth2-login --account <name>
```

Graph accounts require an `[accounts.oauth2]` section with `client_id` and `tenant_id`, and use Graph well-known folder names (`inbox`, `archive`, `sentitems`) instead of IMAP folder names.

### Config commands

```bash
mp config init           # Interactive setup wizard
mp config show           # Display config (passwords masked)
mp config set-password   # Store SMTP or IMAP password in keyring
mp config oauth2-login   # Run OAuth2 device code flow
mp config path           # Print config file path
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

The file is updated in-place with `status: sent`, `sent_at` timestamp, `sent_via` version, and `message_id`.
A send that reached every recipient and was recorded in the outbox then deletes the draft file: the copy that matters lives on the server, where the outbox appends it to Sent and sync reads it back into the local store.
A partial send, or one the outbox store could not record, keeps the file with `status: sent`, because it is the only local copy left.
Sending such a draft again means editing `status:` back to `approved` by hand, since `send` refuses anything that is not approved and `mark-approved` refuses anything already sent; trim the recipient lines first, because a re-send delivers to everyone the file still lists.

## Commands

### TUI

```bash
mp                              # Launch the interactive TUI
```

### Global options

```bash
-A, --account <name>     Account to use (default: first in config; every account for
                         calendar rebuild, contacts rebuild and cutover)
-s, --signature <name>   Use a specific signature
    --no-signature       Skip signature entirely
```

### Drafts

```bash
mp <selector>                   # Preview a draft (dry-run)
mp new <name>                   # Create a new draft from template, print its selector
mp list [--status S] [--json]   # List the account's drafts (draft|approved|sent)
mp validate [<selector>]        # Validate one draft, or every draft of the account
mp mark-approved <selector>     # Mark draft as approved
mp send <selector> [-y]         # Send a single approved email
mp send-approved [--all-accounts] [-y]  # Send every approved draft of the account
mp reply <selector> [--all]     # Create a reply draft from a received email
mp forward <selector>           # Create a forward draft from a received email
mp delete <drafts-selector> [--force]   # Delete a draft (--force for an approved one)
mp delete --sent                # Clear every sent draft of the account
```

A draft selector is `mp://<account>/drafts/<id>`, `drafts/<id>` or `<id>`; a received one is `mp://<account>/<mailbox>/<message-id>`, and `--mailbox <name>` picks the mailbox a short one resolves in.
On a terminal `mp new`, `edit`, `reply`, `forward` and `mark-approved` print `✓ <selector>`; into a pipe they print the bare selector (and `mp save` the bare paths), so `sel=$(mp new note) && mp path "$sel"` works.

### IMAP

```bash
mp fetch [filters]              # Fetch emails from server
mp sync [options]               # Sync local folders with server
mp watch [options]              # Watch mailbox for changes (IMAP IDLE)
mp list-mailboxes               # List available server mailboxes
mp archive <selector>           # Archive a received email (server + local)
mp delete <selector>            # Delete a received email (server + local)
mp search <query> [--json]      # Search emails on the server (--local: the store)
mp open <selector>              # Open an attachment in the default app
mp save <selector> [--output]   # Save attachment(s) to a directory
mp show <selector> [--json]     # Print one received message from the local store (fetched first if missing)
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

A signature is one Markdown file in `~/.config/mailypoppins/signatures/`.
The file name without its extension is the signature's name, so `work.md` is the signature `work`, and creating a signature is creating a file.
The per-account default is recorded in the app's own state file, `<data_dir>/state.json`.

The signature is spliced into the draft body when the draft is created: after the body for a new message, above the quoted content for a reply or forward.
So it is visible and editable while you write, and the same text feeds both the plain-text and the HTML part of the sent mail.

Manage them from the TUI with `cs`, which lists the signatures with the default starred: `Enter` sets or clears the default, `e` opens the selected file in `$EDITOR`, `n` creates one, `r` renames, `d` deletes after a confirmation.

HTML in a signature file is converted to Markdown as it is read, keeping links and line breaks, so a signature exported from another mail client can be pasted in as-is.
An account with no default signature adds nothing.
Use `-s <name>` to pick a signature for one draft, or `--no-signature` to skip it.

Signatures used to live in `config.toml` under `[accounts.signatures.*]`.
The first run after the upgrade copies those entries out to files, keeps the old default, and prints a notice; the tables are dead after that and can be deleted by hand, since mailypoppins never rewrites `config.toml`.

Reply and forward drafts keep a `<!-- mp:quote -->` marker between the reply area and the quoted text.
It carries no text; it marks where the send path splits the body to place the quoted original, and it is stripped from every sent copy.
Drafts written by older versions carry `{{SIGNATURE}}` in that place, which is still read as the same boundary.

## TUI

Running `mp` with no arguments opens a full-screen terminal interface (built on `ratatui`).

Layout: sidebar (mailbox list) | email list | headers + body preview. Adapts to terminal width.

Every mailbox but Drafts lists one row per conversation, with its messages from Inbox, Sent, Archive and Drafts one keypress away, newest first; `tc` switches an account to one row per message, and the TUI remembers the choice in `~/.config/mailypoppins/tui.json`.

Keys are nvim-style mnemonic families: a leader letter opens a which-key popup listing its continuations, and the second key runs the action.
`c` is compose, `t` is thread and attachments, `f` is find and filter, `s` is sync and settings, `g` is go-to, `Space` switches view.
Forget a chord and `:` or `Ctrl+p` opens a command palette over every runnable action by name.

| Key | Action |
|-----|--------|
| `j`/`k`, arrows | Navigate |
| `Enter` | Read the message in the body pane (expands a folded conversation first) |
| `l`/`h` | Expand / fold the conversation |
| `e` | Open in `$EDITOR` (received mail is read-only) |
| `cn` | New draft |
| `cs` | Manage signatures |
| `cr`/`ca`/`cf` | Reply / Reply all / Forward |
| `cA`/`cD` | Approve draft / back to draft (in Drafts or a conversation) |
| `x`/`cX` | Approve and send / Send all approved |
| `a`/`d` | Archive / Delete (with confirmation) |
| `ta`/`to`/`ts` | Attach file to draft / Open attachment / Save attachment |
| `tt`/`tb` | Show conversation / Open HTML in browser |
| `tc` | Conversation view on / off for this account |
| `ff`/`fm` | Search all mail / Filter the current list |
| `ss`/`sS` | Quick sync / Full sync |
| `ga`/`gm` | Switch account / Go to the sidebar |
| `1`-`9` | Jump to mailbox |
| `:` or `Ctrl+p` | Command palette |
| `?` | Help |
| `q` | Quit |

The help overlay (`?`) and `mp dump-keys` print the full catalogue, both generated from the same table the TUI dispatches on.

A send from the TUI is held for `email.send_hold_secs` (default 20) before it reaches the transport: the status line counts down and `u` cancels it, leaving the approved draft in place.
Set it to `0` to hand off immediately. `mp send` and the batch run are never held.

The TUI is a client of the daemon over its Unix socket and spawns no subprocess; the daemon syncs new mail and pushes the changes to every open client.

## Example workflow

```bash
# 1. Set up (once)
mp config init

# 2. Launch the TUI for interactive management
=======
>>>>>>> Stashed changes
mp
```

## Documentation

- [mailypoppins.com](https://mailypoppins.com) covers [getting started](https://mailypoppins.com/getting-started/), the [commands](https://mailypoppins.com/commands/), the [configuration](https://mailypoppins.com/config/), the [draft format](https://mailypoppins.com/draft-format/) and an [FAQ](https://mailypoppins.com/faq/).
- `mp --help` lists every command, and `?` in the terminal UI every key.
- [docs/](docs/) holds the architecture, the daemon protocol, the secrets model and the release process.
- [clients/desktop/README.md](clients/desktop/README.md) covers the desktop app's build.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE)
