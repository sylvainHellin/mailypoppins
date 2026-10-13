# mailypoppins

Rust CLI + TUI for managing emails as Markdown files with YAML frontmatter. Cargo workspace: the root crate (library + binary, which owns the engine and the daemon) plus `crates/mp-core` (the engine-free modules a client and the engine both read, re-exported from the root crate under their old paths), `crates/mp-protocol`, `crates/mp-client` and `clients/tui` (the terminal client, re-exported as `mailypoppins::tui`); every client crate lives under `clients/`. The CLI and the TUI are clients of the daemon over a Unix socket and spawn no subprocess; no crate under `crates/` or `clients/` depends on the root crate, which is what keeps the engine out of a client.

## Repo layout

- Rust crate at the root (`src/`, `Cargo.toml`, `tests/`), workspace members under `crates/`. All build / test commands run from the root.
- `spikes/` and `clients/desktop/` are excluded from the workspace (the spike carries its own `Cargo.lock`).
- Marketing + docs site under [website/](website/) (Astro + Svelte, pnpm). Deployed to <https://mailypoppins.dev> by [scripts/deploy-website.sh](scripts/deploy-website.sh) (rsync to OVH). Colocated so CLI changes and the docs that describe them ship in one commit.
- `.gitignore` files are kept local-only by convention (the root `.gitignore` self-ignores). Edit them as needed but do not `git add -f` them.

## Build and test

```sh
cargo install --path .                     # install / reinstall, run after every code change
cargo test --workspace                     # offline, the whole tree including the daemon
cargo insta review                         # approve markdown_to_html snapshot diffs
```

Skipping `cargo install --path .` after a code change is the single most common footgun.

Test scratch goes to `/var/tmp`, set once in [.cargo/config.toml](.cargo/config.toml) so no run needs a flag: `/tmp` is a small RAM-backed tmpfs and ~2200 tests each building a tempdir sqlite store exhaust it, which surfaces as `Disk quota exceeded` / `SQLITE_IOERR` rather than as a real failure.
Agent scratch belongs there too, as `/var/tmp/mp-<purpose>-<id>` for worktrees; `/var/tmp` is aged out after 30 days while `~/.cache` accumulates forever.
Parallel agents in the same checkout share one `CARGO_TARGET_DIR=/var/tmp/mp-shared-target` (and `/var/tmp/mp-shared-desktop-target` for `clients/desktop/src-tauri`) instead of one per agent: a build tree is 5 to 17 GB, twenty of them filled the Mac's disk once, and cargo's build lock makes sharing safe.
An agent in its own git worktree must not use the shared dir (cargo hashes workspace crates relative to the workspace root, so two worktrees overwrite each other's artifacts and run each other's test binaries): it clones it with `cp -cR /var/tmp/mp-shared-target /var/tmp/mp-<purpose>-target` and runs `cargo clean -p` on the workspace crates there.
An orchestrator deletes the shared dirs when the batch is done.

Website: `cd website && pnpm install && pnpm dev` (preview) or `pnpm build` (production bundle in `website/dist/`).

The desktop crate (`clients/desktop/src-tauri`) builds only on the Mac; pure Rust in it can still be checked here by copying it into a scratch crate under `/var/tmp/mp-<purpose>-check` with `CARGO_TARGET_DIR` beside it, and `cargo fmt --check` runs without a build. A change made on the home server ends with the checks Sylvain has to run there: list them as a short Plane comment or task addressed to him (a line per check), never as a document section; a handoff for an agent on the other machine goes to `.agents/handoff/`, which Syncthing mirrors.

## Further reading

See [docs/](docs/) for architecture, project invariants, lessons-learned, auth, secrets, exchange setup, design plans, and ticket workflow. Open work is indexed in [BACKLOG.md](BACKLOG.md); shipped features in [CHANGELOG.md](CHANGELOG.md).

When you discover a non-obvious behaviour or hard-won fix, append it to [docs/lessons-learned.md](docs/lessons-learned.md) in the same turn. For commands and key bindings, the source of truth is `mp --help` and the in-TUI help overlay (`?`); the website pages under `website/src/pages/` are hand-derived from those and must be updated alongside CLI changes.
