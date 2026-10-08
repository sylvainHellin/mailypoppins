# mailypoppins

Rust CLI + TUI for managing emails as Markdown files with YAML frontmatter. Cargo workspace: the root crate (library + binary, which owns the engine and the daemon) plus `crates/mp-core` (the engine-free modules a client and the engine both read, re-exported from the root crate under their old paths), `crates/mp-protocol`, `crates/mp-client` and `clients/tui` (the terminal client, re-exported as `mailypoppins::tui`); every client crate lives under `clients/`. The CLI and the TUI are clients of the daemon over a Unix socket and spawn no subprocess; no crate under `crates/` or `clients/` depends on the root crate, which is what keeps the engine out of a client.

## Repo layout

- Rust crate at the root (`src/`, `Cargo.toml`, `tests/`), workspace members under `crates/`. All build / test commands run from the root.
- `spikes/` and `clients/desktop/` are excluded from the workspace (the spike carries its own `Cargo.lock`).
- Marketing + docs site under [website/](website/) (Astro Starlight, pnpm; pages are Markdown under `website/src/content/docs/`). Deployed to <https://mailypoppins.dev> by [scripts/deploy-website.sh](scripts/deploy-website.sh) (rsync to OVH). Colocated so CLI changes and the docs that describe them ship in one commit.
- `.gitignore` files are kept local-only by convention (the root `.gitignore` self-ignores). Edit them as needed but do not `git add -f` them.

## Build and test

```sh
cargo install --path .                     # install / reinstall, run after every code change
cargo test --workspace                     # offline, the whole tree including the daemon
cargo insta review                         # approve markdown_to_html snapshot diffs
```

Skipping `cargo install --path .` after a code change is the single most common footgun.

Test scratch goes to `/var/tmp`, set once in [.cargo/config.toml](.cargo/config.toml) so no run needs a flag: `/tmp` is a small RAM-backed tmpfs and ~2200 tests each building a tempdir sqlite store exhaust it, which surfaces as `Disk quota exceeded` / `SQLITE_IOERR` rather than as a real failure.
Agent scratch belongs there too, as `/var/tmp/mp-<purpose>-<id>` for worktrees and `CARGO_TARGET_DIR`: a build tree is several GB, and `/var/tmp` is aged out after 30 days while `~/.cache` accumulates forever.

Website: `cd website && pnpm install && pnpm dev` (preview) or `pnpm build` (production bundle in `website/dist/`).

The desktop crate (`clients/desktop/src-tauri`) builds only on the Mac; pure Rust in it can still be checked here by copying it into a scratch crate under `/var/tmp/mp-<purpose>-check` with `CARGO_TARGET_DIR` beside it, and `cargo fmt --check` runs without a build. A change made on the home server ends with the checks Sylvain has to run there: list them as a short Plane comment or task addressed to him (a line per check), never as a document section; a handoff for an agent on the other machine goes to `.agents/handoff/`, which Syncthing mirrors.

## Further reading

See [docs/](docs/) for architecture, project invariants, lessons-learned, auth, secrets, exchange setup, design plans, and ticket workflow. Open work is indexed in [BACKLOG.md](BACKLOG.md); shipped features in [CHANGELOG.md](CHANGELOG.md).

When you discover a non-obvious behaviour or hard-won fix, append it to [docs/lessons-learned.md](docs/lessons-learned.md) in the same turn. For commands and key bindings, the source of truth is `mp --help` and the in-TUI help overlay (`?`). The website's `reference/commands.md` and `reference/keys.md` are generated from them: after a CLI or keymap change run `MP_BIN=<freshly built mp> pnpm gen` in `website/` (or `scripts/regen-website-keys.sh` for the keys) and commit the output; the other doc pages are hand-written and must be updated alongside the change.
