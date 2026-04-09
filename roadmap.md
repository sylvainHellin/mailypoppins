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

---

## Backlog

### Bug to fix

- ~~german char are not properly rendered in the browser when opening an email with `b`. Images are also not rendered.~~ Fixed: `ensure_utf8_charset` now replaces stale charset declarations (e.g. `iso-8859-1`) instead of keeping them; inline images with `Content-ID` headers are extracted and `cid:` references in HTML are rewritten to local `file://` paths.
- something is wrong with the install script and self signed certificate - I am still asked for my password after each update (despite installing with the script)
- in draft, when I select multiple emails (with the space bar), I cannot bulk mark approve (only the highlighted one will change status), whereas I can bulk archive them and bulk delete them

### TUI enhancements

*Ported from the beautifulmail roadmap after the v0.5.0 merge.*

**Navigation & feedback**
- **Unread/new indicator** -- Mark emails that arrived since last viewed (bold, dot, or color).
- **Jump-to-date** -- Quickly jump to a specific date range in large mailboxes.

**Actions**
- ~~**Batch operations**~~ -- Done (v0.5.2).
- **Quick-move between mailboxes** -- e.g., move an archived email back to inbox without leaving the view.

**Preview & reading**
- **Thread/conversation grouping** -- Group related emails by `In-Reply-To`/`References` headers.
**Quality of life**
- ~~**Help overlay scroll/filter**~~ -- Done (v0.5.3).
- **Configurable keybindings** -- Allow users to remap keys via config file.
- **Status pane** -- A dedicated pane to display the status of local/server actions (sent/sync/archive/etc.). Position and design TBD.

### CLI

- **Delete draft command**: `email delete` currently only works for inbox emails (requires `message_id` for server-side deletion). Drafts are local-only, so deleting them should just remove the local `.md` and companion `.html` files without any IMAP operation.

---

- add a function in the TUI to open the html of the email in the default browser
- add support for exchange accounts and proton
- add a way to have multiple accounts (another pane to navigate between accounts?)
- add a search function
