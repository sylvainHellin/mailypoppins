---
id: 0012
title: Apple Developer ID signing for macOS releases
type: chore
priority: later
status: open
created: 2026-05-01
---

$99/yr Apple Developer Program. Store cert + key in GitHub Actions secrets; codesign the macOS release binary in CI with `--options runtime`; optionally notarize via `notarytool`.

## Downstream

Once shipped, consider flipping the default `secrets_backend` to `keyring` for new installs from signed releases (the Keychain ACL persists as long as the Designated Requirement matches a stable signing identity). Existing installs with `secrets.enc` keep working; **no auto-migration** (per project policy).

## Depends on

- [#0011 ci-release-pipeline](0011-ci-release-pipeline.md)
