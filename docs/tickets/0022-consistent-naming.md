---
id: 0022
title: consistent naming
type: refactor
priority: now
status: open
created: 2026-05-02
---

Ensure the naming is consistent throughout: `mailypoppins`
at the moment, the config is still under `email` and the cli is also called like this. for the CLI, check if `mp` is available

## Progress

- **CLI binary renamed `email` -> `mp`** (`[[bin]]` in `Cargo.toml`;
  `cargo install --path .` now installs `mp`). All user-facing command
  references (`--help`, README, `docs/*.md`, website) updated. The
  displayed name/version string stays `mailypoppins X.Y.Z` (clap
  `#[command(name = "mailypoppins")]`).
- **Still under `email`:** the Cargo package + library name and the
  config directory `~/.config/email/` are unchanged (data-location /
  internal-identity, invisible in the binary name). Renaming the config
  dir would need a migration path (against the "no migrations until v1.0"
  policy) and is deferred. The library rename is a large `use email::`
  churn with no user-visible payoff; left as-is for now.
