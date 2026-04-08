# Backlog

## Now

Items to tackle in the current work cycle.

- **Fix read/unread sync behaviour** -- read status resets after each fetch. Bidirectional sync with IMAP `\Seen` flag is not working correctly. [handoff](.claude/handoff/2026-04-05_23-34-46_read-unread-sync-debug.md)
- **macOS codesign for local dev** -- re-sign `~/.cargo/bin/email` with a stable self-signed cert after every `cargo install --path .` so the Keychain ACL persists across rebuilds and stops re-prompting three times per install. Secrets stay in Keychain; no plaintext fallback. [plan](.pi/workflow/macos-codesign-dev/plan.md)

## Next

Queued up, will pick from here when Now is clear.
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
