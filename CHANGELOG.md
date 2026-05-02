# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Performance
- **TUI cold start is now ~instant instead of ~1.4 s black screen.**
  `AccountState::new` no longer walks every mailbox directory to build
  the in-memory `message_id_index` synchronously. The scan is extracted
  into a free function `build_message_id_index` and dispatched on a
  background thread per account from `run_loop`; results arrive via the
  new `BgResult::IndexReady` variant, which sets `acct.message_id_index`
  and clears `acct.indexing`. The status bar shows "Indexing..." with
  the existing spinner; sync/fetch actions queue via the existing
  `bg_count > 0` gate and fire automatically when the last account
  finishes. Local benchmark: per-account `AccountState::new` dropped
  from 1183 ms / 234 ms (TUM / Proton) to 13 ms / 4 ms.
  Closes [#0003](docs/tickets/0003-cold-start-async-indexing.md).
- **First quick sync after launch is now <2 s instead of ~14 s.** Persist
  per-role IMAP `MailboxState` (`uid_validity`, `uid_next`, `exists`) to
  `<account_dir>/mailbox-states.json` after every successful Fetch /
  Sync, and reload it in `AccountState::new`. This lets the cold-start
  reconcile decision in `sync_mailboxes` take the state-based skip
  branch instead of falling through to a full Message-ID scan of INBOX +
  Archive. `uid_validity` mismatch on the next SELECT still falls back
  to a full reconcile, so other-client moves / mailbox renumbering
  remain safe. Closes [#0002](docs/tickets/0002-persist-mailbox-states.md).

### Changed
- **All app data moved to a single OS-conventional data directory.**
  Mail tree, drafts, contacts cache, OAuth2 tokens, and logs now live
  under `mailypoppins_data_dir()`:
  - macOS: `~/Library/Application Support/mailypoppins/`
  - Linux (incl. WSL): `$XDG_DATA_HOME/mailypoppins/` (default `~/.local/share/mailypoppins/`)

  Layout: `accounts/<name>/{inbox,archive,sent,drafts,<extra>}/`,
  `accounts/<name>/contacts-cache.json`, `tokens/<name>.enc`, `logs/`.
  `~/.mailypoppins/` is retired entirely. Override the root with the
  `MAILYPOPPINS_DATA_DIR` env var (test seam + portable-install escape
  hatch).
- New `src/config.rs` helpers: `mailypoppins_data_dir`, `account_dir`,
  `mailbox_dir`, `drafts_dir`, `contacts_cache_path`, `tokens_dir`,
  `logs_dir`. Replace the removed `resolve_root_dir`,
  `resolve_mailbox_local_path`, `resolve_drafts_dir_from_config`,
  `resolve_dir`, `expand_path`, `log_base_dir`.
- Wizard prints the derived data-dir paths instead of asking the user to
  pick one. The Obsidian-vault workflow is preserved via a symlink hint:
  `ln -s ~/Library/Application\ Support/mailypoppins/accounts/<name> ~/notes/email/<name>`.

### Removed
- Per-account `[accounts.directories]` block (`root`, `drafts` keys).
- Per-mailbox `local = "..."` field on `[accounts.mailboxes.{inbox,archive,sent}]`
  and `[[accounts.mailboxes.extra]]`. `MailboxMapping` now carries only `server`.
- `email config migrate` subcommand (legacy single-account -> multi-account
  migrator). Per the v1.0 "no migrations" policy, configs that still use the
  old keys are rejected at parse time with a clear message instructing the
  user to re-run `email config init`.

### Breaking
- Existing configs containing `[accounts.directories]` or per-mailbox
  `local = "..."` fail to load. Run `email config init` to regenerate.
- Existing local mail trees at user-chosen roots (e.g. `~/notes/email/`)
  are no longer read or written. Re-run `email config init`, then
  `email sync` to repopulate the new data dir from the server. (Or move
  the existing tree manually into
  `<data_dir>/accounts/<name>/{inbox,archive,sent,...}/`.)
- OAuth2 token caches at `~/.mailypoppins/tokens/<account>.enc` are
  ignored. Re-run `email config oauth2-login --account <name>`.

### Added
- `dirs = "5"` dependency for cross-platform data-dir resolution.

### Fixed
- `email config init` and `email config add-account` no longer panic with
  "Cannot start a runtime from within a runtime" when the wizard reaches
  the IMAP test, OAuth2 device-code flow, or Graph folder discovery.
  `main` is `#[tokio::main]`, so the wizard's sync code paths now drive
  nested async work via a new `config_cmd::helpers::run_async_blocking`
  helper that detects the existing runtime and spawns a fresh one in a
  dedicated OS thread (mirrors `oauth2::load_or_refresh_token_blocking`).

## [0.8.0] -- previous

### Changed
- **Default password store switched from OS keyring to a machine-bound
  encrypted file** at `~/.config/email/secrets.enc`. Built on
  ChaCha20-Poly1305 + HKDF-SHA256 with the key derived from
  `machine-uid + getuid + app salt`. Pure Rust, zero prompts at runtime,
  identical UX on macOS, Linux, and WSL. The file is undecryptable on any
  machine other than the one that wrote it (defends against Time Machine /
  iCloud / Dropbox / accidental git commit leakage).
- OAuth2 token caches now stored encrypted at
  `~/.mailypoppins/tokens/<account>.enc` using the same crypto.
- New `email config reset-secrets` command -- wipes the secrets file and
  token caches, then walks each account prompting for re-entry. Use after
  a Time Machine restore to a new machine.
- `keyring` retained as an **opt-in backend** via
  `secrets_backend = "keyring"` in `~/.config/email/config.toml`.
- New `src/secrets.rs` module exposing `SecretsBackend` trait,
  `EncryptedFileBackend`, `KeyringBackend`, and `encrypt_blob` /
  `decrypt_blob` helpers reused by `oauth2.rs`.
- See [docs/secrets.md](docs/secrets.md) for the threat model, key
  derivation, file layout, and recovery procedure.

### Removed
- `scripts/codesign-macos.sh` and the `scripts/install.sh` wrapper. No
  longer needed -- secrets do not depend on a stable code-signing
  identity, so `cargo install --path .` is the canonical install command
  again.
- `keyring`-to-encrypted-file migration command. Per project policy ("no
  migration paths until v1.0"), users on the old keyring path re-enter
  passwords once via `email config init` or `email config set-password`.
- `windows-native` feature on the `keyring` crate. Native Windows is not
  a supported target (WSL only).

### Breaking
- After upgrading, run `email config init` (fresh setup) or
  `email config set-password <smtp|imap> --account <name>` for each
  configured account to populate `~/.config/email/secrets.enc`. Existing
  Keychain entries are ignored unless you opt into
  `secrets_backend = "keyring"` in `config.toml`.
- OAuth2 token cache file extension changed from `.json` to `.enc`. Run
  `email config oauth2-login --account <name>` to re-acquire and cache
  tokens for each OAuth2 / Graph account.

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
