# Email CLI Roadmap

## Completed

### Workstream 1: Split main.rs into Modules (v0.3.0)

Broke the 3,700-line `main.rs` into focused modules: `types.rs`, `config.rs`, `config_cmd.rs`, `parse.rs`, `imap_client.rs`, `draft.rs`, `send.rs`, `sync.rs`. No behavior changes. Browse command was removed during refactor (skim dependency dropped).

### Workstream 2: Global Config & Secrets Management (v0.4.0)

- Global config at `~/.config/email/config.toml` (works from any directory)
- Passwords stored in OS keyring via `keyring` crate (macOS Keychain, Windows Credential Manager, Linux Secret Service)
- `.env` file removed entirely (no backward compatibility layer)
- `email config init` interactive wizard with credential testing and mailbox discovery
- `email config show`, `email config set-password`, `email config path` subcommands
- Mailbox mapping system: role-based (`inbox`, `archive`, `sent`) + extra mailboxes, case-insensitive lookup

### Optimistic Archive & Delete (v0.4.1)

- `archive` and `delete` now perform local file operations first (instant), then IMAP (slow)
- If IMAP fails, local changes are rolled back automatically
- Enables optimistic UI updates in the TUI without UI/disk desync

### Integrated TUI (v0.5.0)

- `email` with no arguments launches a ratatui-based TUI (merged from the standalone beautifulmail project)
- `src/lib.rs` added: crate is now both a library and binary
- New modules: `src/tui/` (mod.rs, app.rs, ui.rs, event.rs, theme.rs)
- New dependencies: `ratatui`, `crossterm`, `arboard`
- TUI calls library functions directly (no subprocess spawning)
- Config types derive `Clone` for thread sharing
- Background IMAP IDLE watcher, optimistic archive/delete, queued sync operations
- Catppuccin Mocha theme, vim-style key bindings, search/filter, confirmation dialogs

### Backlog items (resolved)

- **Fetch saves to correct mailbox**: `email fetch --mailbox Archive` now saves to the archive directory with `status: archived` (fixed in v0.3.1).
- **Case-insensitive mailbox matching**: Mailbox fetch and sync now match IMAP folder names case-insensitively (fixed in v0.3.1).

---

## Up Next

### Workstream 3: async-imap Swap

**Complexity:** M (~4-6h feasibility), M-L (~6-8h full swap)
**Goal:** Migrate from `imap` v2.4 to `async-imap` v0.11 to mitigate maintainer risk.

The `imap` crate's primary maintainer (jonhoo) has stepped back. `async-imap` is actively maintained by Delta Chat and is API-compatible. The current sync IMAP code also blocks the tokio runtime during long operations (fetch, IDLE).

**Note:** `imap-proto v0.10.2` (transitive dependency of `imap`) already generates a future Rust compatibility warning. This adds urgency to the swap.

**PoC strategy:**
1. Start with `list_mailboxes()` -- simplest function, validates connection/TLS setup
2. Then `fetch_emails()` -- validates Stream handling and `mailparse` integration
3. Then `watch_mailbox()` -- validates IDLE API differences
4. Remaining functions are mechanical conversions

**Key differences from current code:**
- TLS: `async-native-tls` instead of `native-tls`
- `fetch()` returns async `Stream` instead of `Vec` (use `.try_collect()`)
- IDLE: `wait_with_timeout(duration)` instead of `wait_keepalive()`
- Sessions are `!Send` -- must manage lifetime within a single task

---

## Backlog

- **Browse command**: Superseded by the integrated TUI (v0.5.0). The TUI provides a full mailbox browser with search, preview, and all operations.

- **Delete draft command**: `email delete` currently only works for inbox emails (requires `message_id` for server-side deletion). Drafts are local-only, so deleting them should just remove the local `.md` and companion `.html` files without any IMAP operation.
