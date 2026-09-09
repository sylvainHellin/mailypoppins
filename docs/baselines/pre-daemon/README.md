# Pre-daemon baselines

Frozen record of the CLI and TUI surface as it stood immediately before the native-GUI/daemon
migration (`.agents/workflow/native-gui-daemon/plan.md`, phase 0, ticket #0118). Every later phase
compares against these files rather than against remembered numbers, so nothing in this directory
is regenerated in place: a divergence is a decision, recorded in the phase that causes it.

## Provenance

| Fact | Value |
|---|---|
| Commit | `f8af44b623aba5fdc607fccb21749b7b09ebad95` |
| Tag | `pre-daemon` (same commit; `main` and `daemon` also point here) |
| Captured | 2026-09-09 |
| Binary | `mailypoppins 0.9.0`, `cargo install --path . --locked` |
| Toolchain | `rustc 1.96.0 (ac68faa20 2026-05-25)`, `cargo 1.96.0 (30a34c682 2026-05-25)` |
| Host | Ubuntu 26.04 LTS, Linux 7.0.0-22-generic x86_64 |

`--locked` is not optional. Plain `cargo install --path .` re-resolves the dependency graph past
`Cargo.lock`, and a clap newer than the pinned 4.5.54 renders an empty string default as
`[default: ""]` where 4.5.54 renders `[default: ]` (visible once, in `mp search --help`). The
committed `cli-help.txt` is the lockfile-pinned rendering, which is what `cargo test` sees and what
`tests/snapshots/cli_help_snapshot__cli_help_surface_snapshot.snap` holds.

## Artifacts

### `cli-help.txt`

The whole `mp --help` surface: 50 screens, top level plus every subcommand reached recursively.
Identical to the body of `tests/snapshots/cli_help_snapshot__cli_help_surface_snapshot.snap` (up to
the one trailing blank line insta trims when it stores a snapshot), because `scripts/capture-cli-help.sh` reimplements the walk in `tests/cli_help_snapshot.rs` exactly:
run the real binary, emit a `$ mp … --help` header before each screen, and recurse into every name
listed under `Commands:` in the order clap prints it, skipping clap's auto-generated `help`.

```sh
cargo install --path . --locked
scripts/capture-cli-help.sh > docs/baselines/pre-daemon/cli-help.txt
```

To check the artifact against the snapshot the test suite pins:

```sh
diff <(awk 'f {print} /^---$/ {c++; if (c == 2) f = 1}' \
        tests/snapshots/cli_help_snapshot__cli_help_surface_snapshot.snap) \
     <(head -n -1 docs/baselines/pre-daemon/cli-help.txt)
```

### `tui-keys.json`

`mp dump-keys --json`: 92 bindings across 10 sections, derived from the single `KEYMAP` in
`src/tui/app/keymap.rs`. This is the machine-readable half of the TUI surface; the keys the TUI
hand-dispatches inside overlays are invisible to it and are inventoried separately in
`manual-keys.md` (P0-U2).

```sh
mp dump-keys --json > docs/baselines/pre-daemon/tui-keys.json
diff docs/baselines/pre-daemon/tui-keys.json website/src/data/tui-keys.json
```

The second command is the ANO-1 guard: `website/src/data/tui-keys.json` is generated from the same
`KEYMAP` by `scripts/regen-website-keys.sh`, so a difference means the published key reference has
drifted from the binary. At capture time the two were byte-identical, so the website was not stale.

## What is deliberately absent

No latency or timing numbers. Those arrive with the measurement units later in phase 0 and phase 1a
and are committed beside these files.
