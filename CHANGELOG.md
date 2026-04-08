# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

## [0.8.0] - 2026-04-08

### Added
- **Contact autocomplete**: mine `from:`/`to:`/`cc:` headers from each
  account's local mail archive, filter noreply/bulk-domain noise, rank
  contacts with a tiered comparator (`sent_to` > `sent_cc` > `received`)
  and a 180-day-half-life frecency tiebreaker. Cached per account at
  `<root>/.contacts-cache.json`.
- New `email contacts` CLI subcommand tree:
  - `rebuild` (re)builds the index from local mail for one or all
    accounts.
  - `stats` shows totals and the top 10 ranked contacts for an account.
  - `search <query>` returns ranked fuzzy matches. `--parsable` emits
    tab-delimited `email\tname` lines compatible with mutt
    `query_command`, aerc `address-book-cmd`, himalaya-vim, and
    vim `completefunc` completion sources.
- **TUI compose wizard overlay** triggered by `n` (new) and `w`
  (forward), with a four-field form (`To`/`Cc`/`Bcc`/`Subject`), live
  fuzzy-matched suggestions under the focused address field, Tab
  cycling, Ctrl+g force-submit, Ctrl+u clear-field, and Esc cancel.
  Reply keys `r`/`R` stay direct-to-editor as before. Aerc-style split
  on the last comma so multi-recipient fields work naturally; accepted
  suggestions render as `"Display Name" <addr>, ` so the user can keep
  typing. Once submitted, the draft file is written with a populated
  frontmatter block and then handed off to `$EDITOR`.
- Incremental contacts-index hooks: every successful send (CLI or TUI)
  bumps the recipients' `sent_to`/`sent_cc` counters, and every sync
  merges freshly-fetched email headers into the active account's index,
  preserving historical `first_seen` dates. Both hooks are best-effort
  and no-op when no `.contacts-cache.json` exists yet.
- `scripts/install.sh` and `scripts/codesign-macos.sh` for stable macOS
  code signing during local development. After one-time setup of a
  self-signed cert (see `CONTRIBUTING.md`), the Keychain no longer
  re-prompts on every `cargo install --path .` rebuild.
- `CONTRIBUTING.md` with build instructions, the macOS keychain setup
  walkthrough, and troubleshooting notes.
- `src/contacts/` module (`types`, `filter`, `rank`, `extractor`,
  `matcher`, `cache`, `hooks`) built on `nucleo-matcher` for fuzzy
  matching and `mailparse::addrparse` for robust multi-recipient
  header parsing.
- 47 new unit tests across filter, rank, extractor, matcher, hooks,
  and the wizard recipient-field normalizer (total now 236).

### Changed
- `AGENTS.md` and project `CLAUDE.md` both now point at
  `./scripts/install.sh` as the canonical install command.
- TUI `n` key now opens the compose wizard instead of writing a
  skeleton directly. `Action::NewDraft` is kept as a dead-code
  fallback path.
- TUI `w` key now opens the wizard with the `Fwd:` subject
  pre-populated and attachments preserved through
  `create_forward_draft` before the frontmatter is patched with the
  wizard's edits.
- `cargo clippy -- -D warnings` is clean again: collapsed a nested
  `if` in `src/imap_client/sync.rs` that was already failing the lint.

### Fixed
- Creating a new email via the TUI `n` key now writes a fully populated
  frontmatter block (`to`/`cc`/`bcc`/`subject`/`from`/`date`) instead
  of an empty skeleton. Closes the "data is not added to the
  frontmatter" bug noted in `roadmap.md`.
- Wizard now strips trailing commas and whitespace from the
  `to`/`cc`/`bcc` fields before writing the draft, so contacts with
  display names that contain a comma (for example
  `"Doe, Jane" <addr>`) no longer break
  `mailparse::addrparse` and fail the subsequent send.

### Dependencies
- New: `nucleo-matcher = "0.3"`, `regex = "1"`, `serde_json = "1"` (the
  last one was already a transitive dep).
- Avoided `once_cell` as a direct dep by using `std::sync::LazyLock`
  (Rust 1.80+).

## [0.7.4] - 2026-04-05

### Added
- Mark as read / unread: persistent local tracking via `read` field in frontmatter, synced with IMAP `\Seen` flag
- Auto-mark-as-read when previewing emails (cursor moves to a new email in Inbox/Archive/Extra mailboxes)
- Manual toggle with `m` key (single email or batch selection)
- Unread indicator in email list: blue `\u{f444}` dot for unread, bold text for unread, dimmed text for read
- Unread count shown in status bar (e.g. "3 unread")
- IMAP fetch now captures FLAGS alongside body (`BODY.PEEK[] FLAGS`), preserving server-side read status
- Bidirectional read status sync: server `\Seen` flag synced to local `read:` frontmatter on each fetch
- `mark_read_on_server()` / `mark_unread_on_server()` IMAP operations
- `update_read_status_locally()` for frontmatter file updates
- `sync_local_read_flags()` updates existing local emails' read status from server during fetch/sync
- 16 new tests for read/unread functionality (types, ops, sync, frontmatter)

### Changed
- Existing emails without `read:` field default to unread (backward compatible)
- Optimistic updates: local state updates immediately, server follows async with rollback on failure
- Pass 1 of two-pass fetch now also retrieves FLAGS, enabling read status sync for existing emails

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
