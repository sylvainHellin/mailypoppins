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

### Scroll Position & Attachment Indicator (v0.5.1)

- Status bar now shows current position in the email list (e.g., `Inbox 3/47`, or `Inbox 3/15 (47)` when filtered)
- Paperclip icon shown in email list for emails with attachments

### Batch Operations (v0.5.2)

- Toggle selection with `Space` (advances cursor), `Ctrl+a` to select all visible, `Esc` to clear
- `a`/`d` operate on all selected when selection is active; single-item behavior preserved when nothing selected
- Selection stored as `HashSet<PathBuf>`, stable across list mutations
- Nerd Font checkbox icons in a dedicated column (only shown when selection is active)
- Selected rows get `SURFACE0` background highlight; selection count shown in status bar
- Selection cleared on mailbox switch, search, reload, and after batch confirm
- Batch archive/delete spawn one background thread per item, reusing existing `BgResult` handling

### Help Overlay Scroll & Filter (v0.5.3)

- Help overlay (`?`) now supports `j`/`k` scroll, `d`/`u` half-page, `gg`/`G` top/bottom
- Press `/` inside help to filter entries by key or description (case-insensitive)
- Scroll position indicator in bottom border; discoverability hint when content fits
- Filter bar with cursor, `Esc` to clear filter, `Esc` again to close help

### Backlog items (resolved)

- **Fetch saves to correct mailbox**: `email fetch --mailbox Archive` now saves to the archive directory with `status: archived` (fixed in v0.3.1).
- **Case-insensitive mailbox matching**: Mailbox fetch and sync now match IMAP folder names case-insensitively (fixed in v0.3.1).

### Workstream 3: async-imap Swap (v0.6.0)

- Migrated from `imap` v2.4 (unmaintained) to `async-imap` v0.11 (Delta Chat-maintained)
- Replaced `native-tls` direct dependency with `async-native-tls` + `async-std` for TLS/TCP
- All IMAP functions in `imap_client.rs` are now `async`
- Removed `tokio::task::spawn_blocking` wrappers in `main.rs` (direct `.await` instead)
- TUI background threads create per-thread `tokio::runtime::Runtime` for async IMAP calls
- IDLE watcher uses `IdleResponse::NewData` / `IdleResponse::Timeout` instead of `WaitOutcome`
- Fetch/list results consumed via `futures::TryStreamExt::try_collect()` (Stream → Vec)
- `config_cmd.rs` test connection uses `async-imap` with `block_on`
- Eliminated `imap-proto v0.10.2` Rust compatibility warning

### OAuth2 / Exchange Online Support (v0.8.0)

- Added `auth_method = "oauth2"` and `[accounts.oauth2]` config section for Microsoft 365 / Exchange Online accounts
- OAuth2 Device Code Flow via `email config oauth2-login` command (acquires and caches tokens)
- Token cache at `~/.email-cli/tokens/<account>.json` with automatic refresh on expiry
- IMAP XOAUTH2 authentication branch in `open_imap_session()` (via `async_imap::Authenticator` trait)
- SMTP XOAUTH2 authentication branch in `send_email()` (via `lettre::Mechanism::Xoauth2`)
- Config init wizard updated with "Microsoft 365 / Exchange Online" provider option
- `email config show` displays OAuth2 config and token cache status
- Exchange setup guide at `docs/exchange-setup.md`
- New module: `src/oauth2.rs` (device code flow, token cache, refresh, XOAUTH2 string builder)
- New module: `src/config_cmd/oauth2.rs` (oauth2-login command)
- New dependencies: `base64`, `reqwest` (for OAuth2 HTTP requests)

### Graph API Integration Phase 1 (in progress)

- Added `AuthMethod::Graph` variant (`auth_method = "graph"` in config)
- `SmtpConfig::load()` and `ImapConfig::load()` return early errors for Graph accounts (they use Graph API, not SMTP/IMAP)
- New `GraphConfig` struct with `::load()` to extract client_id, tenant_id, username, account_name from account config
- OAuth2 scopes parameterized: `IMAP_SMTP_SCOPES` and `GRAPH_SCOPES` constants replace the old single `SCOPES`; all oauth2 functions accept a `scopes` parameter
- `scopes_for_auth_method()` helper maps auth method to the correct scopes
- `email config oauth2-login` supports Graph accounts: acquires Graph-scoped token and tests via Graph `/me` endpoint (skips IMAP/SMTP tests)
- `email config show` displays "graph (Microsoft Graph API)" for Graph accounts and hides SMTP/IMAP details

---

## Backlog

### Backlog items (resolved, cont.)

- **Inline images paperclip icon**: Images with `Content-ID` now detected as attachments for the indicator.
- ~~**Install script / keychain re-prompting**: Fixed codesign/keychain issue after updates.~~ Regression: binary is still prompting 3x for keychain passwords at TUI startup despite codesign + ACL reset.
- **Delete draft command**: `email delete` now works for local-only drafts (removes `.md` + `.html` files without IMAP).
- **IMAP watcher retry spam**: Exponential sleep between retries; silent errors after first one (v0.6.0+).
- ~~review the hotkeys for the different commands~~ Done: `f`=IMAP search, `s`/`S`=quick/full sync, `1-4`=pane focus. Removed `h`/`l` from global Tab/BackTab. Sidebar focus via `1` instead of `s`.
- ~~bug: duplicated emails in inbox/archive~~ Fixed: case-insensitive Message-ID header parsing, propagate known IDs across sync targets, post-sync dedup pass, frontmatter scanner handles both quote styles. 37 existing duplicates cleaned up.
- ~~open/save attachments from server search overlay~~ Done: `o`/`O` in search overlay list saves result locally then opens the same attachment picker / dir picker flow as the regular list.
- enable opening the attachments also for draft with `o` (useful to check what is being send).
- ~~bug: quoted display names with commas (e.g. `"Hellin, Sylvain" <addr>`) broke send/validate because naive `.split(',')` split inside the quoted name~~ Fixed: `split_addresses()` helper respects quotes.
- ~~bug: frontmatter does not validate when to: is empty (or null)~~ Fixed: `to` is now `Option<String>` with `#[serde(default)]`. Validation requires at least one of to/cc/bcc to be non-empty. BCC-only emails are fully supported.
- ~~bug: cannot open in-email img as attachments (e.g. message_id: <202604170909.63H99GFW3924666@easychair.org>) - also not possible to open in browser (no html attached)~~ Fixed: `is_attachment_part()` now treats inline non-text MIME parts with a filename as attachments (catches PDFs, images, etc. sent with `Content-Disposition: inline`). Previously only explicit `attachment` disposition or inline `image/*` were detected.

### TUI enhancements

*Ported from the beautifulmail roadmap after the v0.5.0 merge.*

**Navigation & feedback**
- **Jump-to-date** -- Quickly jump to a specific date range in large mailboxes.

**Actions**
- ~~**Batch operations**~~ -- Done (v0.5.2).
- **Quick-move between mailboxes** -- e.g., move an archived email back to inbox without leaving the view.

**Quality of life**
- ~~**Help overlay scroll/filter**~~ -- Done (v0.5.3).
- **Configurable keybindings** -- Allow users to remap keys via config file.
- **Status pane** -- A dedicated pane to display the status of local/server actions (sent/sync/archive/etc.). Position and design TBD.

### CLI

- accept invitations. I regularly receive calendar invitation per mail, and in other clients I can accept/tentative/reject - would be nice to have something in the same vein here.
- ~~add an option to save the attachments (with a path picker). right now, only possible to open them, but does not work well for things like .zip files~~ Done: `O` in TUI opens a save-attachment flow with multi-select + directory picker (zoxide/browser). CLI: `email save <file> [--output <dir>]`.

---

- **Thread/conversation grouping** -- Group related emails by `In-Reply-To`/`References` headers. (moved from TUI enhancements -- uncertain if needed)
- ~~add a way to have multiple accounts~~ -- Done: multi-account support via `[[accounts]]` in config.toml + sidebar account switching in TUI.
