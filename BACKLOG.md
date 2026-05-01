# Backlog

## Now

Items to tackle in the current work cycle.

- **Auto-fetch on TUI startup** -- The TUI currently spawns IMAP IDLE watchers at launch but does **not** trigger an initial sync. IDLE only catches mail arriving while the watcher is running, so anything that landed between TUI sessions is invisible until the user manually presses `s`. Verified in [.claude/research/2026-04-29_sync-timing-analysis.md](.claude/research/2026-04-29_sync-timing-analysis.md): no `sync_mailboxes` calls between `App::new()` and the first user keypress. Fix: in `tui/mod.rs::run_loop`, after spawning watchers, push `Action::Fetch` for each account with IMAP/Graph config. Best landed *after* "Persist `mailbox_states`" below -- with the cache populated, the startup fetch is a warm-state quick sync (~1.2 s on TUM, ~0.8 s on Proton) instead of the cold 15 s. Without the cache, startup auto-fetch would be a regression. Optional polish: kick the fetch from a background thread so it doesn't block the first frame, and stagger per-account fetches so the TUM 14 s reconcile (if ever forced) doesn't delay Proton's much faster sync.
- **Diagnose and fix quick-sync latency (P0)** -- **Diagnosed.** See [.claude/research/2026-04-29_sync-timing-analysis.md](.claude/research/2026-04-29_sync-timing-analysis.md). The 16 s quick sync is **the first sync after every TUI launch**: `prev_states` (`AccountState.mailbox_states`) starts empty, so the reconcile decision in `sync_mailboxes` (`src/imap_client/sync.rs:319`) falls through to the conservative `true` branch and runs a full Message-ID scan of INBOX + Archive (~14 s on xmail.mwn.de). Subsequent syncs in the same session skip reconcile correctly (1.2 s) -- 13x speed-up confirmed. Two follow-up fixes:
  1. **Persist `mailbox_states` across TUI sessions** (P0). Save to `~/.mailypoppins/cache/<account>/mailbox_states.json` on quit / after each sync; reload in `AccountState::new`. This eliminates the 14 s first-sync penalty entirely. Validate `uid_validity` on next SELECT; fall back to full reconcile if it changed (handles other-client mutations safely).
  2. **Cold start regression** (P1, the original sub-task). `AccountState::new()` blocks ~1.4 s scanning ~17k frontmatter files (TUM 1183 ms / proton 234 ms / hines 1 ms). Move to a spawned thread; show "Indexing..." in the status bar until `BgResult::IndexReady` arrives. Lower priority than fix 1 because it's only ~1.4 s vs ~14 s.
  - Already done: `[TIMING]` instrumentation in `open_imap_session`, `sync_mailboxes`, `fetch_new_emails_on_session`, `lib_do_sync`, `AccountState::new`. Logs at `~/.mailypoppins/logs/mailypoppins-YYYY-MM-DD.log` with millisecond timestamps.
- **Fix read/unread sync behaviour** -- read status resets after each fetch. Bidirectional sync with IMAP `\Seen` flag is not working correctly. [handoff](.claude/handoff/2026-04-05_23-34-46_read-unread-sync-debug.md)
- ~~**macOS codesign for local dev**~~ -- **Obsolete.** Replaced by the machine-bound encrypted secrets file backend (see CHANGELOG). The Keychain ACL rebuild churn is no longer relevant because `keyring` is no longer the default backend; passwords live in `~/.config/email/secrets.enc` and the codesign script has been removed.

## Next

Queued up, will pick from here when Now is clear.
- **Parallel IMAP fetch per mailbox** -- `sync_mailboxes` uses one IMAP session and SELECTs mailboxes sequentially (IMAP requires one selected mailbox per connection). For accounts with 3+ mailboxes on a remote server, each SELECT+SEARCH+FETCH cycle adds ~200-300ms of network latency serially. Opening N parallel IMAP connections (one per mailbox) would turn `N * latency` into `1 * latency`. Trade-off: N TLS handshakes + N logins vs sequential latency. Measure post-index-optimization to confirm this is still needed.
- **Attachment paths break when source email is archived** -- `create_forward_draft` (and reply scaffolding) resolves attachment paths to absolute paths at draft creation time (`src/draft.rs:262-278`). If the source email is subsequently archived (moved from `inbox/` to `archive/`), the hardcoded paths in the draft frontmatter become stale and `send` fails with "Failed to read attachment". Possible approaches:
  1. **Relative paths with lookup** -- store attachment references as relative to the email root (e.g. `tum/inbox/...`) and resolve at send time by searching across `inbox/`, `archive/`, etc. for the matching attachment directory.
  2. **Stable attachment storage** -- copy or hardlink attachments into a dedicated `attachments/` directory (content-addressed or keyed by message-id) at sync time. Drafts reference the stable location, which is unaffected by inbox/archive moves.
  3. **Send-time validation with auto-repair** -- at send time, if an attachment path doesn't exist, search sibling mailbox directories for the same `_attachments/` folder and rewrite the path before sending.
  Option 2 is the most robust (decouples attachment storage from mailbox layout), but option 3 is the cheapest fix.
- **Flagging / starring** -- flag important emails on the server, display flag icon in the list, filter flagged.
- **Threading / conversation view** -- group emails by `In-Reply-To` / `References` headers. Show conversation as an expandable tree or inline thread. [plan](docs/plans/threading.md)
- **Desktop notifications** -- IDLE watcher detects new mail but gives no notification outside the TUI. Integrate `notify-rust` or macOS `terminal-notifier`.
- ~~**Encrypted secrets file backend (age) -- replace macOS Keychain dependency**~~ -- **Done.** Shipped as a machine-bound encrypted file at `~/.config/email/secrets.enc` using ChaCha20-Poly1305 + HKDF-SHA256 over `machine-uid + getuid + app salt`. Pure Rust, zero prompts, identical UX on macOS / Linux / WSL. `keyring` retained as an opt-in via `secrets_backend = "keyring"`. See [docs/secrets.md](docs/secrets.md) and CHANGELOG.

## Later

Ideas and longer-term improvements. Not committed to yet.
- **Contact autocomplete** -- address book from sent/received history, autocomplete when editing draft frontmatter.
- **Inline image rendering** -- render inline images in terminal preview (sixel, iTerm2 protocol, or Kitty graphics).


### Distribution / cross-platform (adoption track)

**Note: Windows is targeted via WSL only. Native Windows (msvc, Credential Manager, Scoop, winget, EV signing) is out of scope.**

- **CI release pipeline** -- GitHub Actions for build, test, and binary release on tag push. Build matrix: macOS (arm64 + x86_64), Linux (gnu + musl). Publish artifacts to GitHub Releases. Prerequisite for Homebrew tap and signed macOS builds.
- **Apple Developer ID signing for macOS releases** -- $99/yr Apple Developer Program; store cert + key in GitHub Actions secrets; codesign the macOS release binary in CI with `--options runtime`; optionally notarize via `notarytool`. Once shipped, consider flipping the default `secrets_backend` to `keyring` for new installs from signed releases (the Keychain ACL persists as long as the Designated Requirement matches a stable signing identity). Existing installs with `secrets.enc` keep working; no auto-migration. Depends on the CI release pipeline above.
- **Homebrew tap** -- publish a `homebrew-email` tap with a formula that downloads the signed macOS release artifact (rather than building from source, which would lose the signature). Linux users can install the same way via `brew install`. Depends on CI release pipeline.
- **Linux packaging** -- ship `.deb`, `.rpm`, AUR package, and a static musl binary in releases. The default encrypted-file backend works without any keyring setup. Can be partially automated with `cargo-deb` / `cargo-generate-rpm`.
- **Cross-platform smoke tests** -- end-to-end test of `email config init` -> `set-password` -> `SmtpConfig::load` round-trip on macOS and Linux. Surface backend-specific error messages well (e.g. `Cannot decrypt secrets store on this machine` vs `Secret '{}' not found`).
