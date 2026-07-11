---
id: 0013
title: Homebrew tap
type: chore
priority: later
status: blocked
created: 2026-05-01
---

> **Blocked on one-time manual setup** (cannot be done from this repo):
> create the `sylvainHellin/homebrew-mailypoppins` tap repo, generate an
> ed25519 deploy keypair, add the public half as a **write-enabled**
> deploy key on the tap repo, and add the private half as the
> `TAP_DEPLOY_KEY` Actions secret on this repo. Exact steps in
> [docs/release-process.md](../release-process.md#homebrew-tap). All
> repo-side work is done; until the key exists the `homebrew-tap` release
> job skips itself with a notice.

Publish a `homebrew-mailypoppins` tap with a formula that downloads the signed macOS release artifact (rather than building from source, which would lose the signature). Linux users can install the same way via `brew install`. Install: `brew tap sylvainHellin/mailypoppins && brew install mailypoppins` (binary `mp`).

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
  every `v*` tag. Auth is via an SSH **deploy key** (secret
  `TAP_DEPLOY_KEY`, loaded into ssh-agent by `webfactory/ssh-agent`),
  cloning/pushing over `git@github.com:sylvainHellin/homebrew-mailypoppins.git`
  -- not an HTTPS PAT.
- The release binary and formula `bin.install` are now `mp` (renamed from
  `email`, ticket #0022); archive names stay `mailypoppins-<target>.tar.gz`
  so formula URLs are unaffected. The formula test asserts against the
  real `mp --version` output (`mailypoppins X.Y.Z`).
- Until [#0012](0012-apple-developer-id-signing.md) ships, the macOS
  artifacts are unsigned; `brew install` still works (no quarantine on
  brew-fetched binaries), and signing slots in without formula changes.

## Depends on

- [#0011 ci-release-pipeline](0011-ci-release-pipeline.md) -- done
- [#0012 apple-developer-id-signing](0012-apple-developer-id-signing.md) (for signed macOS artifacts; not a hard blocker for the tap itself)
