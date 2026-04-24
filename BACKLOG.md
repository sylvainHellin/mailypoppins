# Backlog

## Now

Items to tackle in the current work cycle.

- **Diagnose and fix quick-sync latency (P0)** -- Quick sync still takes ~16s despite the in-memory index and state-caching work (commits `08b8b94`..`5cdd643`). The optimization added zero-disk-scan paths and reconciliation skipping but did not measurably reduce wall-clock time. Two problems:
  1. **Root cause unknown.** Previous attempts optimized the wrong bottleneck (local disk I/O) without profiling. The actual bottleneck is likely network round-trips (TLS handshake, LOGIN, N x SELECT+SEARCH+FETCH) or server-side query latency. Need to instrument with wall-clock timings at each phase boundary to identify where the 16s is spent.
  2. **Cold start regression.** `AccountState::new()` now calls `scan_mailbox_message_ids()` for every mailbox directory synchronously on the main thread. With large mailboxes this blocks the UI for seconds before the first frame renders. The index build must move to a background task with a loading indicator.
  - Approach: (a) Add `log::info!` timestamps or `std::time::Instant` spans around each phase in `sync_mailboxes` and `fetch_new_emails_on_session` (TLS connect, LOGIN, each SELECT, each SEARCH, each FETCH pass, reconciliation, file saves). Run a quick sync and read the log. (b) Move index build in `AccountState::new()` to a spawned thread; show "Indexing..." in the status bar until `BgResult::IndexReady` arrives. (c) Based on profiling, fix the actual bottleneck -- likely parallel IMAP connections or connection reuse (see "Parallel IMAP fetch" in Next).
- **Fix read/unread sync behaviour** -- read status resets after each fetch. Bidirectional sync with IMAP `\Seen` flag is not working correctly. [handoff](.claude/handoff/2026-04-05_23-34-46_read-unread-sync-debug.md)
- **macOS codesign for local dev** -- re-sign `~/.cargo/bin/email` with a stable self-signed cert after every `cargo install --path .` so the Keychain ACL persists across rebuilds and stops re-prompting three times per install. Secrets stay in Keychain; no plaintext fallback. [plan](.pi/workflow/macos-codesign-dev/plan.md)

## Next

Queued up, will pick from here when Now is clear.
- **Parallel IMAP fetch per mailbox** -- `sync_mailboxes` uses one IMAP session and SELECTs mailboxes sequentially (IMAP requires one selected mailbox per connection). For accounts with 3+ mailboxes on a remote server, each SELECT+SEARCH+FETCH cycle adds ~200-300ms of network latency serially. Opening N parallel IMAP connections (one per mailbox) would turn `N * latency` into `1 * latency`. Trade-off: N TLS handshakes + N logins vs sequential latency. Measure post-index-optimization to confirm this is still needed.
- **Flagging / starring** -- flag important emails on the server, display flag icon in the list, filter flagged.
- **Threading / conversation view** -- group emails by `In-Reply-To` / `References` headers. Show conversation as an expandable tree or inline thread. [plan](docs/plans/threading.md)
- **Desktop notifications** -- IDLE watcher detects new mail but gives no notification outside the TUI. Integrate `notify-rust` or macOS `terminal-notifier`.

## Later

Ideas and longer-term improvements. Not committed to yet.
- **Contact autocomplete** -- address book from sent/received history, autocomplete when editing draft frontmatter.
- **Inline image rendering** -- render inline images in terminal preview (sixel, iTerm2 protocol, or Kitty graphics).
- **Encrypted secrets file backend** -- optional `age`-encrypted `~/.config/email/secrets.toml` decrypted at startup (passphrase prompt, or identity file). Target audience: headless / Docker / CI / NixOS users who have no OS keychain. Keyring remains the default; this is an opt-in alternative. No plaintext involved.

### Distribution / cross-platform (adoption track)
- **CI release pipeline** -- GitHub Actions for build, test, and binary release on tag push. Build matrix: macOS (arm64 + x86_64), Linux (gnu + musl), Windows (msvc). Publish artifacts to GitHub Releases. Prerequisite for Homebrew tap and signed macOS builds.
- **Apple Developer ID signing for macOS releases** -- $99/yr Apple Developer Program; store cert + key in GitHub Actions secrets; codesign the macOS release binary in CI with `--options runtime`; optionally notarize via `notarytool`. Solves the keychain re-prompt-on-upgrade problem for end users installing via Homebrew or release tarballs (macOS Keychain ACLs persist as long as the Designated Requirement matches a stable signing identity). Depends on the CI release pipeline above.
- **Homebrew tap** -- publish a `homebrew-email` tap with a formula that downloads the signed macOS release artifact (rather than building from source, which would lose the signature). Linux users can install the same way via `brew install`. Depends on CI release pipeline.
- **Linux packaging** -- ship `.deb`, `.rpm`, AUR package, and a static musl binary in releases. No signing needed for `keyring` to work on Linux (Secret Service / gnome-keyring / KWallet). Can be partially automated with `cargo-deb` / `cargo-generate-rpm`.
- **Windows packaging** -- ship `.exe` via GitHub Releases plus Scoop / winget manifests. Credential Manager works without code signing. EV cert (~$200-400/yr) only needed later to silence SmartScreen warnings.
- **Cross-platform smoke tests** -- end-to-end test of the `keyring` path on Linux (gnome-keyring, KWallet) and Windows (Credential Manager) before promoting the tool. Verify that `SmtpConfig::load` / `ImapConfig::load` succeed on a fresh install with passwords stored via `email config init`. Surface backend-specific error messages well in the `Password '{}' not found` context.
