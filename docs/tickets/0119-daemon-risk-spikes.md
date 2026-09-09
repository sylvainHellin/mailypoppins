---
id: 0119
title: Phase 1a of the daemon migration, the risk spikes and the decisions they settle
type: chore
priority: now
status: done
created: 2026-09-09
---

Second ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md`), after #0118.

Phase 1a writes no product code either.
It answers, with measurements rather than preferences, the four questions the later phases would otherwise have to guess at (transport, list transfer, large payloads, read-pool size), proves the bootstrap ordering algorithm before any domain method exists, and records the dependency due diligence the plan requires before a new crate can be pre-installed.

## Why spikes before Phase 2

Phase 2 fixes the protocol, and a protocol is expensive to change once fixtures pin it and clients read it.
Three of its shapes are decisions with a number behind them: whether a JSON-RPC round trip is affordable on the preview path at all, whether a 5000-row mailbox travels whole or paged, and how a payload larger than the frame cap travels.
The plan's own precedent for not doing this is `docs/plans/preview-latency.md`, which waived its measurement and shipped four tickets against a remembered cost.

So Phase 1a is a throwaway harness that reads the real store through the real read path, and the artifacts it produces are the inputs Phase 2 and Phase 5 are written against.

## What landed

Seven units in five commits.

- P1a-U1 (`b678ed4`): `spikes/ipc-bench`, a standalone package with its own `Cargo.lock` and an empty `[workspace]` table, measuring a direct library call against the same work over NDJSON JSON-RPC on a Unix socket, with the round trip split into serialize, frame-write, socket, dispatch and deserialize stages.
- P1a-U2 (`ca3c61c`): `docs/baselines/decisions/transport.md`. p95 direct 0.029 ms, p95 over the socket 0.054 ms, delta 0.024 ms, which is 0.5% of the 5 ms budget the plan set. Verdict: keep JSON-RPC. The whole round trip, dispatch included, is 1% of that budget, so the cache-and-prefetch contingency in `docs/plans/preview-latency.md` is not required work.
- P1a-U3 (`ca3c61c`): `docs/baselines/decisions/list-transfer.md`. Whole-list transfer with row-level delta events, 1.23 MB compact and 13.8 ms at 5000 rows against 0.6 ms for a page plus its count. Per sync event both options are sub-millisecond; the difference is a one-off at mailbox open, and paging spends its saving back on the three interactions (jump-to-date, metadata filter, select-all) it would have to move to the server, which is a behaviour change inside a migration whose gate is behaviour parity.
- P1a-U4 (`95abf08`): `docs/baselines/decisions/large-payloads.md`. A temp-file handle above a 1 MiB threshold, which is also the request frame cap; one frame below it; chunked frames not adopted. The handle takes the 10 MiB body by 32% (18.4 ms against 27.2 ms) because the body travels unescaped, and loses the whole-account dump by 8%, a loss accepted for the per-record ordering contract.
- P1a-U5 (`95abf08`): `docs/baselines/decisions/read-pool.md`. Two read connections per account. Preview p95 under contention is 219.7 us at size 1, 84.2 us at 2 and 79.7 us at 4, and the size-1 maximum is 11.4 ms, one whole-list read of head-of-line blocking. `rusqlite::Connection` is `Send` but not `Sync`, so the pool is N threads each owning one connection.
- P1a-U6 (`460da63` then `d8294db`): the forced-race tests first, against a contract written down in `spikes/ipc-bench/tests/BOOTSTRAP_CONTRACT.md`, then the prototype that passes them. Eight tests: the five bootstrap boundaries, a revision gap, a duplicate revision, and two clients sharing one order.
- P1a-U7 (this ticket): the dependency due diligence, the bootstrap write-up, the spike's fate, and the exit sweep.

## The bootstrap result, carried forward

`docs/plans/daemon-bootstrap.md` is the part Phase 3a implements: the two ordering rules (register the subscriber before capturing the snapshot; initialise the client watermark to R and silently drop revisions at or below it), the per-boundary table, and the reentrant-gate constraint.

No boundary needed an idempotent replacement to reach "exactly once", so Phase 3a does not have to carry that shape.
`BeforeRegister` and `AfterRegister` are snapshot-only, and the three post-capture boundaries each produce exactly one delivered event.

## Dependencies

`docs/plans/daemon-dependencies.md` records, per candidate in section 2 of the plan, the version, licence, maintenance signal, transitive additions, platform support and verdict, taken from the crates.io API on 2026-09-09 rather than from memory.

Nothing is adopted.
`thiserror` is promoted to a direct dependency when Phase 2 creates `crates/mp-protocol`, which downloads nothing since both trains are already in the lock and in the local cache; the file recommends the 2 train where the plan wrote 1.0.69, and flags the choice for the Phase 2 dispatcher.
`schemars`, `notify`, `tokio-util`, `criterion`, `nix`, `jsonrpsee` and `assert_cmd` are all declined, `schemars` reopening at Phase 7 and nothing else reopening on a date.
The plan's claim that peer credentials need no new crate is verified in the vendored tokio source: `UnixStream::peer_cred` resolves to `impl_linux` on Linux and `impl_macos` on macOS, where the pid comes from `getsockopt(SOL_LOCAL, LOCAL_PEEREPID)` and uid and gid from `getpeereid`.

## The spike is kept

The plan has P1a-U7 delete `spikes/`.
It is kept, unchanged and still outside the workspace, because Phase 6 (P6-U10) has to re-run these workloads against the complete dispatcher, and a benchmark rewritten later from its own report is not the same benchmark: the four decision artifacts are comparable to a Phase 6 number only while the harness that produced them exists.
The reason is recorded in `spikes/ipc-bench/README.md` as well, so the directory explains its own survival.

What that gate line protects is the product tree, and that half holds: `cargo metadata --no-deps` at the repo root lists exactly one workspace member, nothing under `src/`, `tests/` or `Cargo.toml` refers to the spike, and `cargo test` is 1335 tests as before.

## Gate evidence

| Gate line | Artifact | Result |
| --- | --- | --- |
| The transport decision has measured evidence. | `docs/baselines/decisions/transport.md` | Met. p95 delta 0.024 ms, verdict `retain JSON-RPC`, host and commit recorded. |
| The list-transfer, large-payload, and read-pool decisions each have a recorded number and a chosen option. | `docs/baselines/decisions/{list-transfer,large-payloads,read-pool}.md` | Met. Whole list with row deltas; handle above 1 MiB; two read connections. |
| The bootstrap algorithm passes forced-race tests. | `spikes/ipc-bench/tests/bootstrap_race.rs` | Met. `cd spikes/ipc-bench && cargo test` green, 8 tests, at `--test-threads=1` and default. |
| No spike code is carried into the product tree. | `cargo metadata --no-deps`, `cargo test` | Half met. No spike code reaches the product tree and the test count is unchanged, but `spikes/` is deliberately not deleted, for the Phase 6 re-run above. The deletion moves to P6-U10. |

## The measurements this host cannot take

Two escalations, both owner action on a machine with a terminal and a configured account.

- The interactive held-key A/B. The transport delta was measured on the read path, which is the half a socket can change, but the number #0108 was supposed to be judged against (p50/p95 from keypress to painted preview with `j` held over the twenty rows it pinned) still needs a real TUI run. It is also the W1 row that `docs/baselines/pre-daemon/measurements.md` carries as `NOT TAKEN`.
- Cold first paint (W5). The Phase 5 gate compares it against the Phase 0 baseline, and neither figure exists yet.

Both were already escalated out of Phase 0 and neither is closed by Phase 1a.
The spike figures are also lower bounds in one respect: the fixture is on tmpfs on this host, so no number here says anything about cold-cache behaviour.

## Acceptance criteria

- Each of the four questions is answered by an artifact naming a number and an option. Met, four files under `docs/baselines/decisions/`.
- The bootstrap ordering is proved against forced races before Phase 3a is written. Met, 8 tests and `docs/plans/daemon-bootstrap.md`.
- The dependency due diligence is written down per candidate. Met, `docs/plans/daemon-dependencies.md`, facts dated and sourced.
- No product code changes. Met, nothing under `src/`, `cargo test` 1335.
- `spikes/` deleted. Not met, deliberately, with the reason recorded here and in the spike's README, and the deletion carried to P6-U10.

## Files

- `docs/plans/daemon-dependencies.md`, `docs/plans/daemon-bootstrap.md`
- `docs/baselines/decisions/{transport,list-transfer,large-payloads,read-pool}.md`
- `spikes/ipc-bench/**` (harness, prototype, contract, race tests, README)
- `CHANGELOG.md`, `BACKLOG.md`, `docs/lessons-learned.md`
