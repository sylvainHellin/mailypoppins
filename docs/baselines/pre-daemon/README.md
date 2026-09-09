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

## Instrumentation added for the baseline

### `[TIMING] tui_preview_query`

The preview cost has to be separable from the rest of the paint before the daemon puts a round trip
in the middle of it, so `App::refresh_preview_body` (`src/tui/app/mod.rs`) opens a
`TimingSpan::with_context("tui_preview_query", "<mailbox>/<entry>")` around the store read that
materialises the body: the `open_store` plus the one blob read for a message, or the one draft-file
read for a draft.

```
[TIMING] tui_preview_query [inbox/message #412] start
[TIMING] store_open [<data_dir>/accounts/alice/store.sqlite3] start
[TIMING] store_open [<data_dir>/accounts/alice/store.sqlite3] done: 0 ms
[TIMING] tui_preview_query [inbox/message #412] done: 3 ms
```

The span sits below the memo check, not around the whole refresh, so it counts store reads rather
than frames: a cursor move produces exactly one start/done pair, and the frames that follow while
the cursor stays put produce none. `[TIMING] tui_draw` (one pair per paint, `src/tui/mod.rs`) and
`[TIMING] store_open` (one pair per store handle, `src/store/mod.rs`) bracket it, so subtracting the
preview query from the draw gives the paint cost without the read.

```sh
rg '\[TIMING\] tui_preview_query' <data_dir>/logs/mailypoppins-YYYY-MM-DD.log
```

No run of the real TUI was possible when this landed: the host has no configured account, so the
contract is pinned by a unit test instead
(`the_preview_query_span_is_entered_once_per_body_build` in `src/tui/app/types.rs`), which ingests
two fixture messages, paints, repaints, moves the cursor, and asserts the span was opened once per
body build and not at all on a memo hit. The owner takes the log measurement on a machine with an
account, against the workloads P0-U6 records in `workloads.md`.

## What is deliberately absent

No latency or timing numbers. Those arrive with the measurement units later in phase 0 and phase 1a
and are committed beside these files.
