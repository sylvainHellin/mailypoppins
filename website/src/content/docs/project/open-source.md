---
title: Open source
description: mailypoppins is free and open source under the MIT license. How to report a bug, contribute and build it.
---

mailypoppins is free and open source, released under the [MIT license](https://github.com/sylvainHellin/mailypoppins/blob/main/LICENSE).
The code, the issues and the releases are on [GitHub](https://github.com/sylvainHellin/mailypoppins).

## Report a bug or ask for a feature

Open an issue on [GitHub Issues](https://github.com/sylvainHellin/mailypoppins/issues).
For a feature, describe the use case and what you expected to happen.
For a bug, `mp daemon support-bundle` writes a report with every password and token struck out, which helps a lot.

## Contribute

1. Fork the repository.
2. Create a branch: `git checkout -b feat/my-feature`.
3. Make your change and test it: `cargo test --workspace` and `cargo clippy`.
4. Push and open a pull request against `main`.

## Build from source

You need a Rust toolchain, from [rustup.rs](https://rustup.rs).

```sh
git clone https://github.com/sylvainHellin/mailypoppins
cd mailypoppins
cargo build --release
```

`cargo install --path .` installs `mp` into `~/.cargo/bin/`.
Run `mp daemon restart` afterwards, so the daemon runs the new code.

The desktop app lives in `clients/desktop` and builds on macOS with pnpm and Tauri.
Its README has the steps.

## This website

The site lives in `website/` in the same repository, so a change to the CLI and the docs that describe it ship together.
The [command reference](/reference/commands/) and the [key bindings](/reference/keys/) are generated from `mp help` and the keymaps.
