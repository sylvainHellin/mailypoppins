---
id: 0011
title: CI release pipeline
type: chore
priority: later
status: open
created: 2026-05-01
---

GitHub Actions for build, test, and binary release on tag push.

## Scope

- Build matrix: macOS (arm64 + x86_64), Linux (gnu + musl).
- Run `cargo test` on every push.
- On `v*` tags, build release binaries and publish to GitHub Releases.

Prerequisite for [#0012 apple-developer-id-signing](0012-apple-developer-id-signing.md), [#0013 homebrew-tap](0013-homebrew-tap.md), and [#0014 linux-packaging](0014-linux-packaging.md).
