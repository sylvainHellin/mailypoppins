---
id: 0013
title: Homebrew tap
type: chore
priority: later
status: open
created: 2026-05-01
---

Publish a `homebrew-email` tap with a formula that downloads the signed macOS release artifact (rather than building from source, which would lose the signature). Linux users can install the same way via `brew install`.

## Depends on

- [#0011 ci-release-pipeline](0011-ci-release-pipeline.md)
- [#0012 apple-developer-id-signing](0012-apple-developer-id-signing.md) (for macOS)
