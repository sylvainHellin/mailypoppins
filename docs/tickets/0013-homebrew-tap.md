---
id: 0013
title: Homebrew tap
type: chore
priority: later
status: done
created: 2026-05-01
---

> **Done** (2026-07-11). The one-time manual setup is complete: the
> `sylvainHellin/homebrew-mailypoppins` tap repo was created and seeded,
> an ed25519 deploy keypair was generated, the public half was added as a
> **write-enabled** deploy key on the tap repo, and the private half was
> stored as the `TAP_DEPLOY_KEY` Actions secret on this repo. Steps kept
> in [docs/release-process.md](../release-process.md#homebrew-tap) for
> reference (e.g. key rotation). First-release verification is pending:
> the `homebrew-tap` job goes fully live on the next `v*` tag push.

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
