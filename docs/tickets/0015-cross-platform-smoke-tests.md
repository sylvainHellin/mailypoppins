---
id: 0015
title: Cross-platform smoke tests
type: chore
priority: later
status: open
created: 2026-05-01
---

End-to-end test of `email config init` -> `set-password` -> `SmtpConfig::load` round-trip on macOS and Linux. Surface backend-specific error messages well (e.g. `Cannot decrypt secrets store on this machine` vs `Secret '{}' not found`).

## Depends on

- [#0011 ci-release-pipeline](0011-ci-release-pipeline.md)
