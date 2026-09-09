# Pre-daemon measurements

One entry per workload defined in [workloads.md](workloads.md), filled where this host could fill it
and marked `NOT TAKEN` where it could not.
A `NOT TAKEN` entry is not a gap to be quietly closed later with a plausible number: it is an
escalation, and the plan requires the orchestrator to hand it to the owner before Phase 1a.

Nothing here is regenerated in place.
A rerun in a later phase is appended as its own dated block, so the pre-daemon column stays readable
next to the daemon-era one.

## Provenance

- Commit: `42db7ac4ecaed7f01beb7c5d1838f3b7c14697e8` (branch `daemon`, one commit past the
  `pre-daemon` tag; that commit added the `tui_preview_query` span and no product behaviour).
- Taken: 2026-09-09.
- Binary: `mailypoppins 0.9.0`, release, installed with `cargo install --path . --locked`.
- Toolchain: `rustc 1.96.0 (ac68faa20 2026-05-25)`, `cargo 1.96.0 (30a34c682 2026-05-25)`.
- Host: Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM.
- Fixture: `cargo run --example mkfixture -- --out <tmp> --rows 5000`, default `--big-mb 10`.
- Fixture cost: 10.0 s to build (debug binary, three runs within 0.1 s of each other), 5501
  messages, 25.1 MiB of store and blobs
  (`store.sqlite3` 11.1 MiB for `alpha`, 5645 blob files, 13.7 MiB of blob bytes).
- Filesystem: **tmpfs**, because the fixture lives under `/tmp` on this host.
  Every figure below is therefore a lower bound on the same work against a disk-backed data
  directory, and no figure below says anything about cold-cache behaviour.
- Method: median of eleven runs after one discarded warm-up, min and max recorded, using the `bench`
  helper in [workloads.md](workloads.md).
- No account is configured on this host, so `mp` printed its SMTP-secret warning on stderr for every
  run; stdout was discarded and stderr does not enter the timing.

## Reference floor

`mp --version` touches no store and opens no database, so it is the fixed cost every other CLI
figure carries.

- `mp --version`: median 7 ms, min 6, max 8.

Subtracting it from a CLI figure gives the work; comparing it with the daemon-era figure gives what
the socket round trip cost.

## W1: cursor-move preview

`NOT TAKEN (no account / headless host) - owner action.`

The span exists in every build (`[TIMING] tui_preview_query`, see [README.md](README.md)) and its
contract is pinned by a unit test, but the number needs a real terminal, a real account and the
twenty named rows from ticket #0108.
Without it the Phase 5 parity gate has no oracle, which is risk 1 of the plan.

## W2: list refetch at 5000 rows per sync event

`NOT TAKEN (no account / headless host) - owner action` for the TUI figure.

The offline lower bound was taken, on `alpha/Bulk` with all 5000 rows requested:

- `mp list-messages --mailbox Bulk -n 5000`: median 50 ms, min 49, max 52 (five runs).

That is 43 ms of work over the 7 ms floor for reading, sorting and formatting 5000 envelopes in one
process, which bounds from below what the same 5000 rows will cost over the socket.
It says nothing about frame pacing, which is the half of W2 that matters and the half that needs the
TUI.

## W3: whole-account `dump-mailbox`

Taken.

- `mp dump-mailbox --json -A alpha` (5351 rows): median 85 ms, min 83, max 87.
- `mp dump-mailbox --json` (both accounts, 5501 rows): median 86 ms, min 85, max 87.
- Output size: 5501 NDJSON records, sha256
  `ed07745b8a292fc54f0773fa117b93f7e5fd5735c2fe2e9e4e67f0ee3188f9d0` for the two-account dump of a
  `--rows 5000` fixture.

The second account adds 150 rows and 1 ms, so the figure is dominated by `alpha` and by the
serialisation rather than by opening a second store.
The hash is the determinism check the fixture contract requires and is reproducible with a second
`mkfixture` run into a second directory.

## W4: 10 MiB body fetch

Taken.

- `mp show 'inbox/<big-body@fixture.invalid>'` (10 MiB body): median 74 ms, min 72, max 75.
- `mp show 'inbox/<alpha-inbox-42@fixture.invalid>'` (ordinary body): median 42 ms, min 41, max 44.
- Body cost alone: 32 ms, the difference between the two.
- `--json` variant of the large one: median 72 ms, min 70, max 75.

Reading, verifying and printing 10 MiB of body costs about 32 ms on tmpfs, which is the number the
daemon's chunked or streamed answer has to stay within 1.5x of.
The 42 ms control also shows what a `mp show` pays before touching the body: 35 ms over the floor,
mostly config load and store open.

## W5: cold first paint

`NOT TAKEN (no account / headless host) - owner action.`

Needs a terminal, and the figure is only meaningful against a store with real mail in it.

## W6: cold and warm one-shot CLI

Warm figure taken, cold figure not.

- Floor, `mp --version`: median 7 ms, min 6, max 8.
- Warm, `mp list-messages --mailbox inbox -n 20`: median 42 ms, min 40, max 43.
- Cold, same command with the page cache dropped: `NOT TAKEN (no root, and the fixture is on
  tmpfs) - owner action`.

So a one-shot CLI query against a warm 5501-message store costs 42 ms, of which 7 ms is process
start and roughly 35 ms is config load plus store open plus the query.
That 35 ms is the budget a daemon round trip has to beat, and the reason W6 exists.

Taking the cold figure needs root (`sysctl -w vm.drop_caches=3`) and a data directory on a real
filesystem, so it is owner action on a machine with both.

## W7: search

Taken.

- `mp search --local 'body:zolvertrix' -n 100`: median 41 ms, min 40, max 44, **20 hits**.
- `mp search --local 'body:concrete' -n 100`: median 45 ms, min 44, max 47, hits capped at 100.

Both sit within 4 ms of the 42 ms one-shot floor for this store, so the FTS lookup itself is a few
milliseconds and the cost of `mp search` is the cost of starting `mp`.
The hit count is part of the contract: 20 is `--rows / 250` and a daemon-era rerun that reports a
different number has a correctness problem, not a performance one.

## W8: mutation

`NOT TAKEN (no account / headless host) - owner action.`

Every mutation the CLI exposes offline either needs a server (`mp archive`, `mp delete` on a
received message) or a draft that the fixture does not generate, and the flag toggle is a TUI key
binding.
The propagation half of the metric has no pre-daemon value at all, because there is no second
client to propagate to.

## Summary of NOT TAKEN rows

For escalation, in the order the plan cares about them:

- W1, cursor-move preview p50/p95: the Phase 5 parity oracle, blocking for that gate.
- W5, cold first paint: needed before the daemon adds a start to the launch path.
- W8, mutation write plus propagation: needed before Phase 5 claims the daemon improved it.
- W2, the TUI half (frames over 50 ms during a sync burst): the input to the paging decision
  (P1a-U3); the CLI lower bound above is a partial substitute.
- W6, the cold-cache half: needs root and a disk-backed data directory.

The first four need a terminal, a configured account and a warm store, which this host does not
have; the fifth needs root.
All five are owner action on Sylvain's macOS machine, and the plan is explicit that no synthetic
number may be substituted for them.
