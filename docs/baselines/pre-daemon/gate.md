# Phase 0 exit gate

The four gate lines of Phase 0 of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the artifact that answers it and the command that proves it, with the result recorded rather than asserted.

Ticket [#0118](../../tickets/0118-pre-daemon-baselines-and-inventories.md).
Evidence taken on 2026-09-09 at `7297b81` on branch `daemon`, six commits past the `pre-daemon` freeze `f8af44b`, all six of them Phase 0's own.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, host Ubuntu 26.04 LTS.

Three lines pass as written.
One cannot pass in Phase 0 and is carried forward; it is the second one below and the reason is stated there rather than papered over.

## Every current action and command is classified into one of the five classes

Artifact: `docs/parity-matrix.md`.

```sh
grep -cE '^### [A-Z]{2,3}-[0-9]{2} ' docs/parity-matrix.md      # 131 entries
grep -c '^- Classification:' docs/parity-matrix.md              # 132 = 131 + the one prose line at :14
```

131 identifiers from `ACC-01` to `MIG-04`, each carrying exactly one classification from the vocabulary the matrix defines: GUI parity, CLI automation, diagnostics and maintenance, daemon administration, migration-only.
97 are GUI parity.
The 132nd `- Classification:` line is the field description in "How to read an entry", not an entry.

Every source anchor in the matrix was resolved against the tree at `f8af44b`; the four that had moved are corrected under "Anchor corrections", and `RD-05` has no anchor by design because #0109 deleted the capability it names.
The matrix's own cross-read of `cli-help.txt` and `tui-keys.json` found no unnamed CLI surface and three implicitly covered key surfaces, which it lists.

Passes.

## Every GUI-parity feature has a target interaction, or a settled deferral recorded in BACKLOG.md

Artifacts: `docs/parity-matrix.md` and the daemon-migration blockquote in `BACKLOG.md`.

```sh
grep -c '^- Classification: GUI parity' docs/parity-matrix.md   # 97
grep -c '^- GUI location: TBD (Phase 9)' docs/parity-matrix.md  # 130, every entry but retired RD-05
```

Partially satisfied, and it cannot be more than that in Phase 0.

The plan puts the GUI design in Phase 9 and instructs P0-U3 in the same document to leave the GUI column at `TBD (Phase 9)`.
So no GUI-parity entry carries a target interaction, and the gate line's first half is unsatisfiable by the phase that is supposed to satisfy it.
The one exception is `RD-05`, inline image rendering, whose GUI location reads `none` because #0109 retired the capability and the identifier stays reserved rather than carrying an obligation.
The second half is met for what is actually settled today: the deferral list is in `BACKLOG.md` under the daemon-migration blockquote in `## Now`, and it holds the decisions taken rather than a placeholder per row.

Of the 97 GUI-parity entries, one is retired (`RD-05`), one is deferred with the deferral recorded (`OBS-07`), and 95 carry neither a target interaction nor a deferral because the interaction does not exist to record yet.

What the deferral list covers: the dark-only first GUI release with the light theme deferred (`OBS-07`, the one matrix entry whose status is `deferred`), the `mp fetch` deprecation decision (`LST-11`, `ANO-3`), the `schemars` and `notify` dependencies, the `SyncResult.bodies_truncated` widening, and the retired capabilities whose identifiers stay reserved.

Action: the "target interaction" half of this line moves to the Phase 9 checklist, where the GUI is designed, and Phase 0 does not claim it.
A reviewer reading only the gate table would otherwise read 97 unanswered entries as an omission.

## Baseline tests and snapshots pass unchanged

```sh
timeout 600 cargo test
git diff --stat -- tests/snapshots src/tui/ui/snapshots src/snapshots
```

1335 tests pass, 0 failed, 3 ignored, at `7297b81`.
The snapshot diff is empty: no `.snap` file in `tests/snapshots/` (the CLI help surface), `src/tui/ui/snapshots/` (18 golden frames) or `src/snapshots/` (6 `markdown_to_html` snapshots) moved in Phase 0, and no `.snap.new` was written.

The count is 1335 rather than the freeze's 1331 because Phase 0 added four tests of its own, which is the only way this line can move without a behaviour change:

```sh
git worktree add /tmp/predaemon-oracle f8af44b && (cd /tmp/predaemon-oracle && cargo test)   # 1331
```

Three of the four are `tests/architecture_boundaries.rs` (P0-U4) and the fourth is `the_preview_query_span_is_entered_once_per_body_build` in `src/tui/app/types.rs` (P0-U5).
No pre-existing test changed, and `git diff --stat f8af44b..HEAD -- tests/ src/` shows only those additions plus the timing span itself.

Passes.

## The binary built from pre-daemon reproduces the committed baselines

```sh
cargo install --path . --locked
diff <(mp dump-keys --json) docs/baselines/pre-daemon/tui-keys.json
diff <(scripts/capture-cli-help.sh) docs/baselines/pre-daemon/cli-help.txt
diff docs/baselines/pre-daemon/tui-keys.json website/src/data/tui-keys.json
```

All three diffs are empty.
The third is the ANO-1 guard: the published key reference on the website is still the binary's own `KEYMAP`.

`--locked` is load-bearing and the plain form of the command is not equivalent, for the reason recorded in `README.md` and in `docs/lessons-learned.md`: plain `cargo install --path .` re-resolves past `Cargo.lock`, and a clap newer than the pinned 4.5.54 renders an empty-string default as `[default: ""]` where 4.5.54 renders `[default: ]`, one line of `mp search --help`.
An oracle built without `--locked` therefore disagrees with the committed baseline and with the insta snapshot the test suite pins, and the disagreement looks like a regression in the daemon.

The binary under test is built at `7297b81`, not at `f8af44b`.
The only `src/` change between them is P0-U5's timing span, which adds no command, no flag and no key, so the CLI and key surfaces it prints are the freeze's.

Passes.

## Owner action: the measurements this host cannot take

Five rows of [measurements.md](measurements.md) are `NOT TAKEN`, and the plan is explicit that no synthetic number may be substituted for any of them.
They are listed here as well as there because a gate document that only records what passed is how an escalation gets lost.

- W1, cursor-move preview p50 and p95 under the twenty rows #0108 pinned. Blocks the Phase 5 parity gate, which has no oracle without it.
- W5, cold first paint. Needed before the daemon puts a daemon start on the launch path.
- W8, mutation write plus propagation. Needed before Phase 5 claims the daemon improved it.
- W2, the TUI half: frames over 50 ms during a 5000-row sync burst. Input to the list-transfer decision in P1a-U3; the offline CLI figure (50 ms for 5000 envelopes in one process) is a lower bound and not a substitute.
- W6, the cold-cache half. Needs root (`sysctl -w vm.drop_caches=3`) and a data directory on a real filesystem.

The first four need a terminal, a configured account and a warm store, none of which this host has.
The fifth additionally cannot be taken here at all, because the fixture lives under `/tmp` and `/tmp` is tmpfs on this machine, so every figure in `measurements.md` is a lower bound against a disk-backed data directory and none of them says anything about cold-cache behaviour.

Owner action for Sylvain, on the macOS machine with a real account: run the W1, W2, W5, W6 and W8 commands as `workloads.md` writes them, and commit the result as a dated block appended to `measurements.md`.
The plan requires this to happen before Phase 1a dispatches.
