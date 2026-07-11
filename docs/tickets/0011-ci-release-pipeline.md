---
id: 0011
title: CI release pipeline
type: chore
priority: later
status: done
created: 2026-05-01
---

GitHub Actions for build, test, and binary release on tag push.

## Scope

- Build matrix: macOS (arm64 + x86_64), Linux (gnu + musl).
- Run `cargo test` on every push.
- On `v*` tags, build release binaries and publish to GitHub Releases.

## Shipped

- [.github/workflows/ci.yml](../../.github/workflows/ci.yml): `cargo test`
  on every push to `main` and every PR.
- [.github/workflows/release.yml](../../.github/workflows/release.yml): on
  `v*` tags, creates a GitHub release with notes from the matching
  CHANGELOG section and attaches `mailypoppins-<target>.tar.gz` +
  `.sha256` for `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, and `x86_64-unknown-linux-musl` (static,
  via the new `vendored-openssl` cargo feature).
- Release procedure documented in
  [docs/release-process.md](../release-process.md). First run happens on
  the next `v*` tag push.

Prerequisite for [#0012 apple-developer-id-signing](0012-apple-developer-id-signing.md), [#0013 homebrew-tap](0013-homebrew-tap.md), and [#0014 linux-packaging](0014-linux-packaging.md).
