---
id: 0118
title: Phase 0 of the daemon migration, the pre-daemon baselines and inventories
type: chore
priority: now
status: done
created: 2026-09-09
---

First ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md`), one ticket per phase, #0118 through #0125.

Phase 0 writes no daemon code.
It captures what the product does today, while `daemon`, `main` and the `pre-daemon` tag all point at `f8af44b`, because the oracle every later phase diffs against is the binary built from that tree.
The one exception is the `tui_preview_query` timing span (P0-U5), which adds an instrument and no behaviour, and the parity oracle it feeds could not be built without it.

## Why a frozen baseline at all

The migration moves every store read, every network call and every durable operation out of the client process and behind a socket, and its acceptance criterion is that nothing a user can observe changes.
That criterion needs an oracle, and a remembered number is not one: the preview-latency plan (`docs/plans/preview-latency.md`) already waived its own measurement once, and #0108 shipped without the number it was supposed to be judged against.
So Phase 0 commits artifacts rather than quoting them, and records the measurements this host cannot take as an escalation instead of a plausible substitute.

## What landed

Seven units, six of them artifact-only.

- P0-U1 (`79912ac`): `docs/baselines/pre-daemon/` with its provenance, the recursive `mp --help` capture (`cli-help.txt`, 50 screens) and `mp dump-keys --json` (`tui-keys.json`, 92 bindings), plus `scripts/capture-cli-help.sh`, which reimplements the walk in `tests/cli_help_snapshot.rs` so the artifact and the snapshot cannot drift.
  The ANO-1 guard passed at capture time: `website/src/data/tui-keys.json` was byte-identical, so the published key reference was not stale.
- P0-U4 (`417ec78`): `tests/architecture_boundaries.rs`, which walks `src/tui/**/*.rs`, collects every engine import and asserts the set equals `tests/fixtures/tui-engine-imports.txt`.
  The allow-list holds 12 pairs over 7 files, and that 12 is the number Phase 5 has to drive to zero.
  The test is not feature-gated and passes on today's tree; it fails with the file and the module named when a new engine import appears.
- P0-U2 (`ee3607a`): `docs/baselines/pre-daemon/manual-keys.md`, the keys `mp dump-keys` cannot see.
  24 overlay surfaces, 152 hand-dispatched match arms, 192 key spellings, each with its `keys.rs` line at `f8af44b`.
  Only two of those surfaces carry `KeyAction::Manual` rows in `KEYMAP` (20 rows); the other 22 reach neither the help overlay nor the website table, which is exactly the input surface a GUI port drops silently.
- P0-U3 (`8fdc7a0`): `docs/parity-matrix.md`, 131 identifiers from `ACC-01` to `MIG-04`, each with a classification, a source anchor resolved against the tree, the daemon methods it needs, its validation today and its status.
  Four anchors in the plan's inventory had moved and are corrected in the matrix; `RD-05` has no anchor by design, since #0109 deleted the capability.
- P0-U5 (`42db7ac`): `TimingSpan::with_context("tui_preview_query", …)` around the store read that materialises the preview body, so the preview cost is separable from the paint before the daemon puts a round trip in the middle of it.
  The span sits below the memo check, so it counts store reads rather than frames, and its contract is pinned by `the_preview_query_span_is_entered_once_per_body_build` because no real TUI run was possible on this host.
- P0-U6 (`7297b81`): `examples/mkfixture.rs`, a deterministic offline fixture generator, plus `workloads.md` (W1 to W8, each with its command, metric and acceptance rule) and `measurements.md`, filled where this host could fill it.
- P0-U7 (this ticket): the exit sweep, `docs/baselines/pre-daemon/gate.md`, the CHANGELOG entry and the `BACKLOG.md` blockquote.

## Gate evidence

`docs/baselines/pre-daemon/gate.md` maps each of the four Phase 0 exit-gate lines to its artifact and its proving command, and records which one is only partly satisfied.

The short version: three gate lines pass as written, and the fourth, "every GUI-parity feature has a target interaction, or a settled deferral recorded in `BACKLOG.md`", cannot pass in Phase 0.
The plan puts the GUI design in Phase 9 and tells P0-U3 explicitly not to fill the GUI column beyond `TBD (Phase 9)`, so of the 97 GUI-parity entries one is retired (`RD-05`), one is deferred with the deferral recorded (`OBS-07`), and 95 carry neither.
The deferrals that are settled today are recorded in `BACKLOG.md`, and the gate line is carried forward to the Phase 9 checklist rather than declared met.

## The measurements this host cannot take

Five rows of `measurements.md` are `NOT TAKEN`, and they are owner action, not a gap to close later with a plausible number.

- W1, cursor-move preview p50/p95, under the twenty rows #0108 pinned. This is the Phase 5 parity oracle and blocks that gate.
- W5, cold first paint, needed before the daemon adds a daemon start to the launch path.
- W8, mutation write plus propagation, needed before Phase 5 claims the daemon improved it.
- W2, the TUI half (frames over 50 ms during a 5000-row sync burst), the input to the list-transfer decision in P1a-U3. The CLI lower bound, 50 ms for 5000 envelopes in one process, is a partial substitute.
- W6, the cold-cache half, which needs root and a data directory on a real filesystem.

The first four need a terminal, a configured account and a warm store; the fifth needs root and a disk.
The plan requires this escalation to reach Sylvain before Phase 1a dispatches.

## Acceptance criteria

- Every capability the CLI and TUI deliver is classified in `docs/parity-matrix.md`. Met, 131 entries.
- The baselines are committed artifacts, not quoted numbers. Met, `docs/baselines/pre-daemon/`.
- The binary built from the freeze reproduces them. Met, with `--locked`.
- `cargo test` green and the snapshots untouched. Met, 1335 tests, no snapshot diff.
- The GUI-parity target interactions. Not met in Phase 0 by the plan's own scoping; carried to Phase 9.

## Files

- `docs/baselines/pre-daemon/{README.md,cli-help.txt,tui-keys.json,manual-keys.md,workloads.md,measurements.md,gate.md}`
- `docs/parity-matrix.md`
- `tests/architecture_boundaries.rs`, `tests/fixtures/tui-engine-imports.txt`
- `examples/mkfixture.rs`, `scripts/capture-cli-help.sh`
- `src/tui/app/mod.rs`, `src/tui/app/types.rs` (the timing span and its test)
