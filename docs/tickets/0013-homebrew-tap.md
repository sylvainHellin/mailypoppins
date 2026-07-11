---
id: 0013
title: Homebrew tap
type: chore
priority: later
status: blocked
created: 2026-05-01
---

> **Blocked on one-time manual setup** (cannot be done from this repo):
> create the `sylvainHellin/homebrew-email` tap repo and add a
> fine-grained PAT as the `HOMEBREW_TAP_TOKEN` Actions secret. Exact
> steps in [docs/release-process.md](../release-process.md#homebrew-tap).
> All repo-side work is done; until the secret exists the `homebrew-tap`
> release job skips itself with a notice.

Publish a `homebrew-email` tap with a formula that downloads the signed macOS release artifact (rather than building from source, which would lose the signature). Linux users can install the same way via `brew install`.

## Shipped (repo side)

- Formula template:
  [packaging/homebrew/mailypoppins.rb.tmpl](../../packaging/homebrew/mailypoppins.rb.tmpl)
  (binary install from release artifacts; macOS arm64/x86_64 native
  builds, Linux static musl build).
- Renderer:
  [scripts/update-homebrew-formula.sh](../../scripts/update-homebrew-formula.sh)
  fills version + SHA-256 placeholders from the published release
  checksums.
- `homebrew-tap` job in
  [.github/workflows/release.yml](../../.github/workflows/release.yml)
  renders the formula and pushes `Formula/mailypoppins.rb` to the tap on
  every `v*` tag.
- Until [#0012](0012-apple-developer-id-signing.md) ships, the macOS
  artifacts are unsigned; `brew install` still works (no quarantine on
  brew-fetched binaries), and signing slots in without formula changes.

## Depends on

- [#0011 ci-release-pipeline](0011-ci-release-pipeline.md) -- done
- [#0012 apple-developer-id-signing](0012-apple-developer-id-signing.md) (for signed macOS artifacts; not a hard blocker for the tap itself)
