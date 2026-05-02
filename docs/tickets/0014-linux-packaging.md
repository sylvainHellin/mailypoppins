---
id: 0014
title: Linux packaging (.deb, .rpm, AUR, musl)
type: chore
priority: later
status: open
created: 2026-05-01
---

Ship `.deb`, `.rpm`, an AUR package, and a static musl binary in releases. The default encrypted-file backend works without any keyring setup. Can be partially automated with `cargo-deb` / `cargo-generate-rpm`.

## Depends on

- [#0011 ci-release-pipeline](0011-ci-release-pipeline.md)
