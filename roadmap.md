# Email CLI Roadmap

Three workstreams to evolve the email CLI from a single-file prototype into a maintainable, portable tool. Priority order: refactor first (makes the other two easier), then config/secrets, then IMAP swap.

```
Workstream 1: Refactor ──┬──> Workstream 2: Global Config & Secrets
                          └──> Workstream 3: async-imap Swap
```

Workstreams 2 and 3 can run in parallel after the refactor -- they touch different modules (`config.rs` vs `imap_client.rs`).

---

## Workstream 1: Split main.rs into Modules

**Priority:** FIRST
**Complexity:** M (~4 hours)
**Goal:** Break the 3,700-line `main.rs` into focused modules with clear boundaries. Purely mechanical -- no behavior changes.

### Proposed Module Layout

| Module | Approx. Lines | Key Functions |
|---|---|---|
| `config.rs` | ~350 | `load_email_config`, `load_signature`, `load_env_from_path`, `init_logging`, font defaults, `EmailConfig`, `SignatureConfig` structs |
| `draft.rs` | ~300 | `parse_email_draft`, `validate_draft`, `preview_draft`, `mark_as_approved`, `find_drafts`, `resolve_drafts_dir`, `update_status_to_sent` |
| `send.rs` | ~500 | `send_email`, `markdown_to_html`, address extraction/dedup, per-recipient sending logic |
| `imap_client.rs` | ~700 | `fetch_emails`, `list_mailboxes`, `append_to_sent_folder`, `watch_mailbox`, `archive_email_on_server`, `delete_email_on_server`, IMAP connection setup |
| `sync.rs` | ~250 | `scan_local_message_ids`, `move_local_email`, `reconcile_local_files`, `fetch_server_message_ids`, `resolve_mailbox_dir` |
| `browse.rs` | ~400 | `browse_emails`, `build_browse_items`, `build_browse_header`, `refresh_inbox`, `ColoredLine`, `BrowseItem` impl, skim integration |
| `parse.rs` | ~300 | `html_to_plain`, `extract_body_parts`, `extract_body_text`, `has_attachments`, `save_fetched_emails`, `display_fetched_emails`, `compress_uid_set` |
| `main.rs` | ~700 | CLI definition (clap derive), `main()`, command dispatch, `prompt_confirmation` |

### Cross-Module Dependencies

```
main.rs ──> config, draft, send, imap_client, sync, browse, parse
send    ──> config (signatures, HTML formatting), parse (markdown_to_html shared types)
browse  ──> imap_client (archive/delete on server), draft (status updates), parse (display)
sync    ──> imap_client (fetch message IDs), parse (save emails)
draft   ──> config (drafts dir resolution)
```

### Circular Dependency Resolution

`imap_client` and `parse` have a potential circular dependency: IMAP fetch calls parsing functions, and parsing needs IMAP types. Resolution: `parse.rs` owns the data types (`EmailParts`, body extraction), `imap_client.rs` depends on `parse`, never the reverse. Any shared types live in a `types` section at the top of `main.rs` or a small `types.rs` if needed.

### Approach

1. Create module files, move functions in dependency order (types/config first, then parse, then the rest)
2. Make struct fields and functions `pub` / `pub(crate)` as needed
3. After each module extraction, run `cargo build` to verify
4. Final pass: run through all commands manually to verify behavior is identical

### Success Criteria

- `cargo build` passes with no warnings
- All commands behave identically (manual smoke test of each)
- No function is duplicated across modules
- `main.rs` is under 800 lines

---

## Workstream 2: Global Config & Secrets Management

**Priority:** SECOND (after refactor)
**Complexity:** M-L (~6-8 hours)
**Goal:** Work from any directory, stop storing passwords in plaintext `.env` files.

### Current State

- `.env` file in the project directory holds SMTP/IMAP credentials in plaintext
- `config.toml` searched up to 3 parent directories for formatting/signature config
- Must `cd` into the project directory (or have `.env` there) to use the CLI

### Target State

- Global config at `~/.config/email-cli/config.toml` (XDG-compliant on Linux, standard on macOS)
- Passwords stored in macOS Keychain via the `keyring` crate
- CLI works from any directory
- Local `.env` still works as override (backward compatible)

### Config Resolution Order

```
CLI flags  >  local .env  >  ~/.config/email-cli/config.toml  >  macOS Keychain (passwords only)
```

### New Subcommands

| Command | Description |
|---|---|
| `email config init` | Interactive setup: prompts for SMTP/IMAP host, user, dirs; stores passwords in Keychain |
| `email config show` | Print resolved config (passwords masked) |
| `email config set-password` | Store or update a password in Keychain |
| `email config path` | Print the config file path |

### Global Config Format

```toml
[smtp]
host = "smtp.example.com"
port = 465
username = "user@example.com"
# password stored in Keychain under service "email-cli", key "smtp"

[imap]
host = "imap.example.com"
port = 993
username = "user@example.com"
# password stored in Keychain under service "email-cli", key "imap"

[directories]
drafts = "~/notes/email/drafts"
sent = "~/notes/email/sent"
inbox = "~/notes/email/inbox"
archive = "~/notes/email/archive"

[imap.mailboxes]
sent = "Sent"  # IMAP sent folder name

[formatting]
font_family = "system-ui, sans-serif"
font_size = "14px"

[signature]
# ... existing signature config
```

### Keychain Integration

- Crate: `keyring` (cross-platform, macOS Keychain native)
- Service name: `email-cli`
- Two entries: `smtp-password`, `imap-password`
- Passwords never written to disk or logged

### Migration Path

1. Ship with `.env` fallback from day one -- existing setups keep working
2. `email config init` creates global config and migrates passwords to Keychain
3. Document the new setup in README
4. Eventually deprecate `.env` with a warning (not in initial release)

### Success Criteria

- `email send` works from any directory without a local `.env`
- Passwords are in Keychain, not on disk
- Existing `.env` setups continue working without changes
- `email config show` displays the full resolved config

---

## Workstream 3: IMAP Crate Swap Feasibility (`imap` to `async-imap`)

**Priority:** THIRD (after refactor)
**Complexity:** M for feasibility (~4-6 hours), M-L for full swap (~6-8 hours)
**Goal:** Assess and plan migration from `imap` v2.4 to `async-imap` v0.11 to mitigate maintainer risk.

### Why

The `imap` crate's primary maintainer (jonhoo) has stepped back. `async-imap` is actively maintained by Delta Chat and is API-compatible. The current sync IMAP code also blocks the tokio runtime during long operations (fetch, IDLE).

### API Compatibility Matrix

All 7 current IMAP operations have direct `async-imap` equivalents:

| Operation | `imap` v2.4 | `async-imap` v0.11 | Notes |
|---|---|---|---|
| Connect + Login | `imap::connect` + `session.login()` | `async_imap::connect` + `.login()` | TLS setup differs (see below) |
| FETCH | `session.fetch(set, query)` | `session.fetch(set, query).await` | Returns `Stream` instead of `Vec` |
| SEARCH | `session.search(query)` | `session.search(query).await` | Same API |
| LIST | `session.list(ref, pattern)` | `session.list(ref, pattern).await` | Same API |
| APPEND | `session.append(mailbox, msg).flags(f).finish()` | `.append(mailbox, msg).flags(f).finish().await` | Flag type may differ |
| STORE (flags) | `session.store(set, query)` | `session.store(set, query).await` | Same API |
| IDLE | `session.idle().wait_keepalive()` | `session.idle().await` + `idle.wait_with_timeout()` | Different keepalive model |

### Key Differences

1. **TLS setup:** `imap` uses `native-tls` directly. `async-imap` uses `async-native-tls` (or `tokio-rustls`). Connection setup changes but is straightforward.
2. **Stream returns:** `fetch()` returns an async `Stream` instead of a collected `Vec`. Need `.try_collect::<Vec<_>>()` or iterate with `while let Some(msg) = stream.try_next().await`.
3. **IDLE API:** `async-imap` uses `idle_handle.wait_with_timeout(duration)` instead of `wait_keepalive()`. The keepalive loop needs restructuring.
4. **Session ownership:** `async-imap` sessions are `!Send` by default. Need to manage session lifetime within a single task or use `spawn_local`.
5. **Append flags:** May need `Flag` enum instead of string flags -- verify at implementation time.

### PoC Strategy

1. **Start with `list_mailboxes()`** -- simplest function, no data processing, quick validation that the connection and TLS setup work.
2. **Then `fetch_emails()`** -- validates Stream handling and `mailparse` integration.
3. **Then `watch_mailbox()`** -- validates the IDLE API differences.
4. If all three work, the remaining functions (`append_to_sent_folder`, `archive_email_on_server`, `delete_email_on_server`, `fetch_server_message_ids`) are mechanical conversions.

### Browse Integration Challenge

The `browse` command uses `skim` which runs a synchronous event loop. Current IMAP calls (archive, delete, refresh) are wrapped in `tokio::runtime::Handle::current().block_on()`. With `async-imap`, this pattern still works but the session must be created inside the `block_on` call (since `!Send`). May need a helper that creates a fresh short-lived session per operation.

### Risk Assessment

| Risk | Impact | Mitigation |
|---|---|---|
| `async-imap` API breaks | Medium | Pin version, review changelogs before upgrading |
| `!Send` session complicates browse | Medium | Short-lived sessions per browse action |
| Performance regression from Stream vs Vec | Low | `try_collect()` gives identical behavior |
| `async-native-tls` compatibility | Low | Well-maintained, same underlying OpenSSL/SecureTransport |

### Success Criteria (Feasibility)

- `list_mailboxes()` works with `async-imap`
- `fetch_emails()` returns identical results to current implementation
- `watch_mailbox()` IDLE loop works with timeout-based keepalive
- No runtime panics or TLS handshake failures

### Success Criteria (Full Swap)

- All IMAP operations work identically
- `browse` archive/delete/refresh work without deadlocks
- `imap` crate fully removed from `Cargo.toml`
- All commands pass manual smoke test

---

## Backlog

- **Delete draft command**: `email delete` currently only works for inbox emails (it requires a `message_id` to delete on the IMAP server). Drafts are local-only files with no server-side counterpart, so deleting them should just remove the local `.md` and companion `.html` files without any IMAP operation. Either extend `delete_email_locally` to detect drafts and skip the server step, or add a separate code path.

- **Fetch saves to inbox regardless of source mailbox**: `email fetch` always saves fetched emails to the inbox directory with `status: inbox`, even when fetching from a different mailbox (e.g. Archive). It should respect the source mailbox and save to the corresponding local directory with the appropriate status (e.g. emails fetched from Archive should go to the archive directory with `status: archived`).

---

## Summary

| Workstream | Priority | Complexity | Depends On |
|---|---|---|---|
| 1. Refactor into modules | FIRST | M (~4h) | Nothing |
| 2. Global config & secrets | SECOND | M-L (~6-8h) | Workstream 1 |
| 3. async-imap swap | THIRD | M (~4-6h feasibility) | Workstream 1 |
