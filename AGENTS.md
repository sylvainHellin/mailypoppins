# mailypoppins

Rust CLI + TUI for managing emails as Markdown files with YAML frontmatter. Cargo workspace: the root crate (library + binary, which owns the engine and the daemon) plus `crates/mp-core` (the engine-free modules a client and the engine both read, re-exported from the root crate under their old paths), `crates/mp-protocol`, `crates/mp-client` and `clients/tui` (the terminal client, re-exported as `mailypoppins::tui`); every client crate lives under `clients/`. The CLI and the TUI are clients of the daemon over a Unix socket and spawn no subprocess; no crate under `crates/` or `clients/` depends on the root crate, which is what keeps the engine out of a client.
A bug report that names no client may be about the desktop app as much as the TUI: check and fix the behaviour in both, or ask which one before fixing.

## Principles

- Keep the codebase clean to the highest standard: something sub-optimal noticed in passing is fixed, never parked or left as a leftover.
- Every mailbox operation goes through the daemon, and the CLI, the TUI and the desktop app make the same daemon calls and offer the same capabilities ([docs/architecture.md](docs/architecture.md), "Project invariants").

## Repo layout

- Rust crate at the root (`src/`, `Cargo.toml`, `tests/`), workspace members under `crates/`. All build / test commands run from the root.
- `clients/desktop/` is excluded from the workspace and carries its own `Cargo.lock`.
- Marketing + docs site under [website/](website/) (Astro + Svelte, pnpm). Published at <https://mailypoppins.com> (OVH free hosting, SFTP only, no SSH) by [scripts/deploy-website.sh](scripts/deploy-website.sh); its privacy page is the one the Entra app registration links to. Colocated so CLI changes and the docs that describe them ship in one commit.
- `.gitignore` files are kept local-only by convention (the root `.gitignore` self-ignores). Edit them as needed but do not `git add -f` them.

## Build and test

```sh
cargo install --path .                     # install / reinstall, run after every code change
cargo test --workspace                     # offline, the whole tree including the daemon
cargo insta review                         # approve markdown_to_html snapshot diffs
```

Skipping `cargo install --path .` after a code change is the single most common footgun.
On the Mac, `scripts/install-local.sh` is the install step for agents and Sylvain alike: the desktop app relaunches its own bundled `mp` as the daemon, so after `cargo install` alone a restarted daemon still runs the old code.
The script pulls, installs `mp`, bundles and swaps in the desktop app, restarts the daemon once and relaunches the app; it refuses a dirty tree, so commit first.

Test scratch goes to `/var/tmp`, set once in [.cargo/config.toml](.cargo/config.toml) so no run needs a flag: `/tmp` is a small RAM-backed tmpfs and ~2200 tests each building a tempdir sqlite store exhaust it, which surfaces as `Disk quota exceeded` / `SQLITE_IOERR` rather than as a real failure.
Agent scratch belongs there too; `/var/tmp` is aged out after 30 days while `~/.cache` accumulates forever.

Parallel work (a long task beside quick fixes, or several writing agents) runs in Worktrunk worktrees (`code` skill): `wt switch --create <branch>` makes `../mailypoppins.<branch>`, and its setup hook (`[projects]` in `~/.config/worktrunk/config.toml`) copies the local-only `.gitignore` files, `node_modules` and the `.agents`/`.sync` links into it.
Each worktree builds into its own in-tree `target/`, never a shared `CARGO_TARGET_DIR`: cargo hashes workspace crates relative to the workspace root, so two worktrees on one dir overwrite each other's artifacts and run each other's test binaries.
On the Mac the hook clones `target/` by APFS reflink, so dependencies stay warm; on the home server (ext4) a worktree builds cold, about 5 to 17 GB, and `wt remove` or `wt merge` deletes it with the worktree.
A worktree runs `cargo test`, `cargo clippy` and `pnpm` builds, never `cargo install` or `scripts/install-local.sh`: there is one installed `mp` and one daemon, so a live test runs from the main checkout after `wt merge main`.
Before that merge, `git pull --rebase --autostash` in the main checkout; `wt merge` then rebases the branch onto local main, which no other agent's fetch can move.

Website: `cd website && pnpm install && pnpm dev` (preview) or `pnpm build` (production bundle in `website/dist/`).

The desktop crate (`clients/desktop/src-tauri`) builds only on the Mac; pure Rust in it can still be checked here by copying it into a scratch crate under `/var/tmp/mp-<purpose>-check` with `CARGO_TARGET_DIR` beside it, and `cargo fmt --check` runs without a build. A change made on the home server ends with the checks Sylvain has to run there: list them as a short Plane comment or task addressed to him (a line per check), never as a document section; a handoff for an agent on the other machine goes to `.agents/handoff/`, which Syncthing mirrors.

## Further reading

See [docs/](docs/) for architecture, project invariants, lessons-learned, auth, secrets and exchange setup; design plans live in `.agents/plans/`, mirrored between the machines and kept out of git. Open work lives in Plane, project PERSO, module Mailypoppins (`plane issue list PERSO --module Mailypoppins`), with the label `bug` or `feature`; commits and docs cite an issue by its identifier (`PERSO-139`), and the four-digit numbers (`#0143`) are the retired repo tickets, kept in git history and in the titles of the issues they became. Shipped features are in [CHANGELOG.md](CHANGELOG.md).

When you discover a non-obvious behaviour or hard-won fix, append it to [docs/lessons-learned.md](docs/lessons-learned.md) in the same turn; a subagent instead reports its suggested lessons-learned and CHANGELOG lines, and the session that merges its branch writes them after the review. For commands and key bindings, the source of truth is `mp --help` and the in-TUI help overlay (`?`); the website pages under `website/src/pages/` are hand-derived from those and must be updated alongside CLI changes.
