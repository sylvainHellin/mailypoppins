---
title: FAQ
description: Troubleshooting and common questions about mailypoppins.
---

## Setup

### The config file is not found

Run `mp config init`.
The wizard writes `~/.config/mailypoppins/config.toml`, tests the connection, finds your server's mailboxes and lets you assign their roles.

### A password is missing

Run `mp config set-password smtp` or `mp config set-password imap`, with `-A <account>` for an account other than the first.
Passwords live in an encrypted secrets file, never in `config.toml`.

### My passwords stopped working after moving to a new Mac

The secrets file is bound to the machine that wrote it, so a restored copy does not decrypt.
Run `mp config reset-secrets`: it wipes the secrets and the OAuth2 tokens, then asks for each password again.

### SMTP authentication failed

Check the account with `mp config show`, then set the password again with `mp config set-password smtp`.
For an OAuth2 account, sign in again with `mp config oauth2-login -A <account>`.

### Can I use several accounts?

Yes.
Add an `[[accounts]]` table per account, or run `mp config add-account`.
Each account has its own servers, local store and mailboxes.
Switch between them with <kbd>g</kbd><kbd>a</kbd> in the terminal UI, or pass `-A <name>` to a command.

## Drafts and sending

### "Email not approved for sending"

Only an approved draft can be sent.
Run `mp mark-approved <selector>`, or press <kbd>x</kbd> in the terminal UI, which approves and sends in one step after a confirmation.
`mp mark-draft <selector>` takes an approval back.

### Can I take a send back?

Yes, from the terminal UI and the app.
A send is held for 20 seconds before it goes out, and <kbd>u</kbd> cancels it.
The approved draft stays in place.
Change the window with `send_hold_secs` under `[email]`; `mp send` and `mp send-approved` are never held.

### My draft came out without a signature

The account needs a default signature.
`mp config show` prints the signatures directory, the current default and every file in it.
Press <kbd>c</kbd><kbd>s</kbd> to set the default, or to create a signature if there is none.

### mp warns that my [accounts.signatures] tables are dead

Signatures moved out of `config.toml` into Markdown files in `~/.config/mailypoppins/signatures/`.
The first run after the upgrade copied them there and kept each default.
mailypoppins does not edit `config.toml` for you, so delete the old tables yourself and the notice stops.
Read the notice first: an entry it could not copy still exists only in those tables.

### Does it send HTML mail?

Yes.
You write Markdown, and it is sent as HTML with a plain-text part.
Incoming HTML mail is shown as its plain text in the terminal UI; <kbd>t</kbd><kbd>b</kbd> opens the original in your browser.
The desktop app renders the HTML in a sandboxed reader.

### How are attachments handled?

Received attachments are stored beside the message, not as files you browse.
<kbd>t</kbd><kbd>o</kbd> opens one and <kbd>t</kbd><kbd>s</kbd> saves them; `mp save` does it from the CLI.
For a draft, list paths in `attachments:`, or name a folder to attach every file in it.

## Sync

### How does sync work?

`mp sync` downloads only new messages, matched by Message-ID.
It reads the headers first and fetches a full message only when it is not stored yet.
Every sync also enumerates the whole mailbox, so a message deleted on the server is removed locally too.

### Why were some removals held back?

A sync that could not bring in everything that arrived since the last one does not remove anything, because a message missing locally proves nothing.
The status line says `N removal(s) held back`, and the next complete sync applies them.
A first sync always works this way, including the first one after an upgrade rebuilds the local store.

### An archived Gmail message is still in my inbox

On Gmail, archiving removes the Inbox label and the copy in All Mail keeps its old identifier.
The inbox row goes on the next sync, and the archived copy is filed by the next full sync, <kbd>s</kbd><kbd>S</kbd> in the terminal UI.

### Do I have to keep the terminal UI open to get mail?

No.
The daemon watches the inbox and syncs on its own, with no window open.
`mp daemon install-service` starts it at login; see [The daemon](/guides/daemon/).

## Microsoft 365

### IMAP is disabled for my organisation

Ask IT to enable it, or use `auth_method = "graph"`, which uses the Microsoft Graph API instead of IMAP and SMTP.
See [Microsoft 365 and Exchange](/guides/microsoft-365/).

### "Needs admin consent"

`IMAP.AccessAsUser.All` and `SMTP.Send` often need an administrator's consent.
Ask IT to grant it in the Azure portal, under Entra ID, App registrations, API permissions.

### The OAuth2 token expired

Run `mp config oauth2-login -A <account>`.
Access tokens refresh on their own, but the refresh token expires after about 90 days without use.

### "AUTHENTICATE failed" with OAuth2

IMAP may be disabled for the account, the token may have the wrong permissions, or admin consent may be missing.
Check the app's permissions in Azure, or switch to `auth_method = "graph"` if your tenant blocks IMAP.

## Keys

### I forgot a key

Press <kbd>:</kbd> or <kbd>Ctrl+p</kbd> to open the command palette, type part of an action's name and press <kbd>Enter</kbd>.
Press a family key alone (<kbd>c</kbd>, <kbd>t</kbd>, <kbd>f</kbd>, <kbd>s</kbd>, <kbd>g</kbd>) to see what can follow it.
<kbd>?</kbd> shows every key, and [Key bindings](/reference/keys/) lists them all.

## The desktop app

### macOS says the app cannot be opened

The app is not signed yet.
Allow it once under System Settings, Privacy & Security, with "Open Anyway", or run `xattr -dr com.apple.quarantine /Applications/mailypoppins.app`.

### The app says the daemon runs another version

Another `mp` started the daemon, for example a Homebrew install or a build from source that was not followed by `mp daemon restart`.
Choose Restart in the app, which replaces the daemon with the app's own version.
To avoid it, use one `mp`: link the app's copy onto your `PATH` instead of installing a second one.
