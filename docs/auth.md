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
- Default backend: machine-bound encrypted file at `~/.config/mailypoppins/secrets.enc`.
  Opt into the OS keyring with `secrets_backend = "keyring"` in `config.toml`; both backends use the same keys (see [secrets.md](secrets.md)).
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
- Scopes: `GRAPH_SCOPES` constant in `crates/mp-core/src/oauth2.rs` (`Mail.Read`, `Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite`, `offline_access`).
  The token cache records the scopes each token response granted (`TokenCache::scopes`, empty in a cache written before PERSO-200).
  A refresh asks for all of them, so a token whose user already consented to the calendar picks it up; when Entra refuses the calendar scope, the refresh is repeated without it and the mail keeps working.
  A Graph calendar account whose token lacks `Calendars.ReadWrite` reports it as `missing_scopes` in `account.credentials` and as a `calendar:<name>` health check, which every client shows as a re-login prompt (`mp config oauth2-login -A <name>`).
- `SmtpConfig::load()` and `ImapConfig::load()` return errors for Graph accounts. Use `GraphConfig::load()` instead.
- All mail operations route through `src/graph.rs`: `list_folders`, `fetch_new_messages`, `sync_mailboxes_graph`, `GraphClient::send_mime`, `search_messages_threaded`; the per-message move, delete and read toggle through `src/graph_ops.rs` (`move_message`, `delete_message`, `mark_read`).
- The TUI and the CLI hold no transport: they call the daemon over its socket, and the daemon picks Graph or IMAP/SMTP from the account's `auth_method`.
  Server mutations (move, archive, delete, flags) go through `ops::Backend::resolve` and `ops::run_op` in `src/ops.rs`; sync, send, fetch and search branch in the daemon methods under `src/daemon/methods/`.
- No IMAP IDLE: the daemon's per-account watcher (`src/daemon/runtime/watcher.rs`) enumerates the Graph inbox every 60 seconds (`GRAPH_POLL`) and runs a quick tick when its message-id set changes.
- The scheduler (`src/daemon/runtime/scheduler.rs`) also ticks a Graph account once it has gone `imap.sync_interval_secs` without a sync, and follows up within seconds while a capped folder still owes a backlog.
- An incremental sync pass reads `/messages/delta` from the stored delta token (#0042) and falls back to a full folder enumeration when there is no usable token, on a full sync, or when the delta reports a removal.
- `mp config oauth2-login --account <name>` runs the device-code flow through the daemon (`config.oauth2_login`) and stores the token cache.
  `mp config show` displays "graph (Microsoft Graph API)" and hides SMTP/IMAP details.

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
