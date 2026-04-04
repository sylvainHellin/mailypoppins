# Changelog

All notable changes to this project are documented in this file.

## [0.7.3] - 2026-04-04

### Changed
- Deduplicated message-ID scanning: canonical `scan_mailbox_message_ids` in `parse.rs`, both `scan_existing_message_ids` and `scan_local_message_ids` delegate to it
- Replaced manual `format!()` YAML construction in `save_fetched_emails` with `SaveFrontmatter` struct + `serde_yaml` for correct special-character quoting
- Extracted shared `collapse_hyphens` helper to `types.rs`, used by all three slugify functions
- Email validation in `validate_draft` now uses `lettre::message::Mailbox` (RFC 5321) instead of naive `contains('@')` check
- Added `#[derive(Default)]` to `FetchCriteria` for cleaner construction

### Added
- 28 new tests for `imap_client` submodules (search.rs: 22, fetch.rs: 6), restoring coverage lost during module split
- `SaveFrontmatter` struct in `types.rs` for type-safe frontmatter serialization
- `collapse_hyphens` helper in `types.rs`

### Fixed
- Sync now uses cross-directory dedup: emails already stored in any local mailbox
  directory (e.g. Archive) are skipped when fetching from other mailboxes (e.g. INBOX).
  This prevents re-downloading archived emails if they still exist on the server's INBOX.
- Special characters in email subjects/senders no longer break frontmatter YAML
- Added diagnostic logging to server search: logs which IMAP mailbox each query targets,
  how many results each mailbox returns, and where search results are saved on disk.

## [0.7.2] - 2026-04-04

### Changed
- Refactored large TUI and IMAP modules into focused submodules:
  - `src/tui/app.rs` (1863 lines) -> `src/tui/app/` with `mod.rs` (506), `types.rs` (616), `keys.rs` (824)
  - `src/tui/ui.rs` (1634 lines) -> `src/tui/ui/` with 8 submodules (sidebar, list, headers, preview, status, overlays, search, util)
  - `src/tui/mod.rs` (1304 lines) -> split into `mod.rs` (118), `actions.rs` (678), `bg.rs` (165), `helpers.rs` (367)
  - `src/imap_client.rs` (1530 lines) -> `src/imap_client/` with 6 submodules (fetch, sync, search, watch, ops, batch)
- No file exceeds 824 lines. Zero behavior change.

## [0.7.1] - 2026-04-04

### Added
- Testing infrastructure: 139 tests (110 unit + 29 integration), all offline, <0.5s
- Unit tests for: `parse.rs`, `types.rs`, `send.rs`, `draft.rs`, `config.rs`, `imap_client.rs`, `sync.rs`, `tui/app/types.rs`
- Integration tests: `tests/draft_integration.rs`, `tests/sync_integration.rs`, `tests/save_emails_integration.rs`
- Snapshot tests for `markdown_to_html` via `insta`
- Dev-dependencies: `tempfile`, `insta`

## [0.7.0] - 2026-04-04

### Added
- Proton Mail Bridge support via IMAP STARTTLS and self-signed certificate acceptance
- `accept_invalid_certs` config option for SMTP and IMAP (for Proton Bridge and other self-signed setups)
- IMAP STARTTLS connection mode for non-993 ports (automatic, port-based heuristic)
- Proton Bridge preset in `email config init` wizard (pre-fills localhost:1143/1025 with cert bypass)
- Multi-account support: N email accounts with independent IMAP/SMTP/directories/signatures
- New config format using `[[accounts]]` array in `config.toml`
- Per-account keyring namespacing (`smtp-password-{name}`, `imap-password-{name}`)
- Account switching in TUI: backtick cycles, Ctrl+1-9 for direct jump
- Account selector in status bar with unseen-mail indicators
- Per-account IMAP watchers (all accounts watched simultaneously)
- `--account` / `-A` CLI flag to target a specific account
- `email config migrate` command to convert old single-account config
- `email config add-account` command to add accounts to existing config
- Per-account sidebar titles

### Changed
- Config model: `[smtp]`/`[imap]`/`[directories]`/`[mailboxes]`/`[signatures]` are now nested under `[[accounts]]`
- `default_from` moved from `[smtp]` to `[[accounts]]`
- Status bar simplified: account labels + `? help` replace verbose hotkey hints
- `config set-password` now takes `--account` flag

## [0.6.0] - 2026-03-30

### Added
- Server-side search across all mailboxes
- Background task status panel
- Open HTML version of email in browser
- Open attachments from the TUI (`o`)
- Help overlay with scrolling (`j`/`k`, `d`/`u`, `gg`/`G`) and filtering (`/`)
- Forward emails with attachments
- Batch operations via email selection (delete/archive)
- Paperclip icon for emails with attachments

### Changed
- Performance: shared IMAP connection, skip re-downloading existing emails
- Switched to async-imap
- Streamlined fetch/sync (removed reconcile step)
- Optimistic archive/delete (local state updates immediately)

### Fixed
- Fetching emails no longer marks them as read on the server
- HTML parser charset handling
- Date display in search mode
- UTF-8 character boundary parsing bug

## [0.5.0] - 2026-03-08

### Added
- Full TUI interface (major refactor from CLI-only)

## [0.4.0] - 2026-03-01

### Added
- Case-insensitive mailbox matching (consistent with IMAP)
- Fetching from different mailboxes saves to correct directory

## [0.3.1] - 2026-02-26

### Changed
- Complete refactor into modular components

## [0.3.0] - 2026-02-22

### Added
- Sync sent folder
- Save original `.html` alongside `.md` for received emails, reinject in replies
- Archive and delete commands
- `--yes` flag for non-interactive send
- Watch command (IMAP IDLE)

### Fixed
- UTF-8 string truncation bug

## [0.2.0] - 2026-01-31

### Added
- Copy path of selected file
- Color highlighting in browse view
- Logging and individual send status tracking

## [0.1.0] - 2026-01-16

### Added
- Initial release
- Markdown drafts with YAML frontmatter
- SMTP sending with HTML conversion
- IMAP email fetching
- Mailbox listing
- Signature support
