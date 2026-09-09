# mailypoppins

Rust CLI + TUI for managing emails as Markdown files with YAML frontmatter. Cargo workspace: the root crate (library + binary) plus `crates/mp-protocol` and `crates/mp-client`; the TUI calls the library directly, no subprocess spawning.

## Repo layout

- Rust crate at the root (`src/`, `Cargo.toml`, `tests/`), workspace members under `crates/`. All build / test commands run from the root.
- `spikes/` and `desktop/` are excluded from the workspace (the spike carries its own `Cargo.lock`).
- Marketing + docs site under [website/](website/) (Astro + Svelte, pnpm). Deployed to <https://mailypoppins.dev> by [scripts/deploy-website.sh](scripts/deploy-website.sh) (rsync to OVH). Colocated so CLI changes and the docs that describe them ship in one commit.
- `.gitignore` files are kept local-only by convention (the root `.gitignore` self-ignores). Edit them as needed but do not `git add -f` them.

## Build and test

```sh
cargo install --path .                     # install / reinstall, run after every code change
cargo test --workspace                     # offline, ~7s
cargo test --workspace --features daemon   # daemon code and its contract tests
cargo insta review                         # approve markdown_to_html snapshot diffs
```

Skipping `cargo install --path .` after a code change is the single most common footgun.

Website: `cd website && pnpm install && pnpm dev` (preview) or `pnpm build` (production bundle in `website/dist/`).

## Further reading

See [docs/](docs/) for architecture, project invariants, lessons-learned, auth, secrets, exchange setup, design plans, and ticket workflow. Open work is indexed in [BACKLOG.md](BACKLOG.md); shipped features in [CHANGELOG.md](CHANGELOG.md).

When you discover a non-obvious behaviour or hard-won fix, append it to [docs/lessons-learned.md](docs/lessons-learned.md) in the same turn. For commands and key bindings, the source of truth is `mp --help` and the in-TUI help overlay (`?`); the website pages under `website/src/pages/` are hand-derived from those and must be updated alongside CLI changes.
