# Authentication methods

Each account in `~/.config/mailypoppins/config.toml` declares an `auth_method`.
Two are supported.

| `auth_method` | Transport | Use case |
|--------------|-----------|----------|
| `password` (default) | IMAP + SMTP with username/password | Every IMAP server: Proton Bridge, Fastmail, Gmail, on-premises Exchange |
| `graph` | Microsoft Graph REST API | Every Microsoft 365 (Exchange Online) account |

Auth method is set per account; one config can mix both.

## The retired `oauth2` mode

`auth_method = "oauth2"` signed in to Microsoft and then spoke IMAP and SMTP with XOAUTH2; it is retired (decision log, 2026-10-23).
`AuthMethod::RetiredOAuth2` still deserializes, so a config that names it loads and only that account fails: `ImapConfig::load` and `SmtpConfig::load` refuse it, the daemon's `start_account` records it as a failed start (`blocked`, with the reason), `sync.*` refuses it with `-32006` and `state: "blocked"`, `config.oauth2_login` and `config.probe` refuse it, and `config_warnings` names it.
Every refusal is `config::retired_oauth2_message`, which says how to switch: `auth_method = "graph"` for Microsoft 365, a password for on-premises Exchange.
No write takes a block naming it: the daemon's `account_block` refuses one.

## Password

- Passwords stored in the secrets backend under `smtp-password-<account>` and `imap-password-<account>`.
- IMAP falls back to the SMTP password if `imap-password-<account>` is missing.
- Default backend: machine-bound encrypted file at `~/.config/mailypoppins/secrets.enc`. Opt into the OS keyring with `secrets_backend = "keyring"` in `config.toml`. See [secrets.md](secrets.md).
- Set or rotate via `mp config set-password {smtp|imap} --account <name>`.

## Graph (Microsoft Graph REST)

- Used for every Microsoft 365 account.
  Signs in through the shared mailypoppins app registration unless the account names its own (see [exchange-setup.md](exchange-setup.md)).
- Config block:

  ```toml
  [[accounts]]
  name = "tenant-only"
  auth_method = "graph"
  default_from = "you@tenant.onmicrosoft.com"

  [accounts.oauth2]
  client_id = "..."
  tenant_id = "..."
  ```

- Token cache: `<mailypoppins_data_dir>/tokens/<account>.enc` (encrypted with the same `encrypt_blob` / `decrypt_blob` as `secrets.enc`), refreshed automatically; when the refresh token expires after roughly 90 days of inactivity, re-run `mp config oauth2-login --account <name>`.
- Scopes: `GRAPH_SCOPES` constant in `oauth2.rs` (`Mail.Read`, `Mail.ReadWrite`, `Mail.Send`, `offline_access`).
- `SmtpConfig::load()` and `ImapConfig::load()` return errors for Graph accounts. Use `GraphConfig::load()` instead.
- All mail operations route through `src/graph.rs`: `list_folders`, `fetch_new_messages`, `sync_mailboxes_graph`, `send_mail`, `move_message_graph`, `delete_message_graph`, `mark_read_graph`, `search_messages_threaded`.
- TUI actions branch on `app.is_graph()` in `tui/actions.rs::handle_action()` and dispatch to Graph helpers (`graph::archive_email_graph`, etc.) instead of IMAP/SMTP.
- No IMAP IDLE -- the watcher is a 60-second timer that polls inbox message count and emits `WatchEvent::Changed` on delta.
- `mp config oauth2-login --account <name>` tests Graph accounts via the `/me` endpoint. `mp config show` displays "graph (Microsoft Graph API)" and hides SMTP/IMAP details.

## Setup wizard

`mp config init` (and `mp config add-account`) presents three provider options:

1. Standard IMAP/SMTP with a password (also on-premises Exchange)
2. Proton Mail (Bridge, password, self-signed cert)
3. Microsoft 365 / Exchange Online (Graph API, no IMAP/SMTP)

For Graph, the wizard runs the device-code flow, tests the credential, discovers folders, and lets you assign mailbox roles.
See [exchange-setup.md](exchange-setup.md) for the Entra ID app-registration walkthrough.

## Recovery

- Forgotten password: `mp config set-password {smtp|imap} --account <name>`.
- Expired Graph refresh token: `mp config oauth2-login --account <name>`.
- Lost / corrupted secrets file (e.g. Time Machine restore on a new Mac): `mp config reset-secrets`. This wipes `secrets.enc` and `tokens/`, then walks each account prompting for re-entry.
