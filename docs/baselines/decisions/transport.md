# Decision: keep JSON-RPC over the Unix socket

Unit P1a-U2 of `.agents/workflow/native-gui-daemon/plan.md`, ticket #0119.
The plan holds the transport to one rule: retain JSON-RPC while the p95 delta between a direct
library call and the same work over the socket stays at or under 5 ms.
This file records the measurement that rule was applied to, and the two things it does not settle.

| field | value |
| --- | --- |
| `p95_direct_ms` | 0.029 |
| `p95_rpc_ms` | 0.054 |
| `delta_ms` | 0.024 |
| `verdict` | retain JSON-RPC |
| `host` | Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM, fixture on tmpfs |
| `commit` | `b678ed42571d27e570d286f6e397699f9a09d07b` (branch `daemon`) |

The delta is 0.5% of the budget the plan fixed.
The whole round trip, dispatch included, is 1% of it.

## What was run

The workload is W1 of [../pre-daemon/workloads.md](../pre-daemon/workloads.md), reduced to the part
a socket can change: one envelope plus its body for `<alpha-inbox-42@fixture.invalid>`, read through
`mailypoppins::store::read` on both sides of the comparison.
The harness is the P1a-U1 spike, `spikes/ipc-bench`, whose README documents the stage split.

Conditions, pinned as ticket [#0108](../../tickets/0108-coalesce-key-events.md) requires:

- release build (`cargo run --release`), never a debug build, which is roughly five times slower on this path;
- the fixture of `docs/baselines/pre-daemon/workloads.md`, built by `mkfixture --rows 5000`, under `/tmp` and therefore on tmpfs;
- named rows rather than positions, so the same message is read on every run;
- warm page cache: 20 untimed warm-up samples per run, and the fixture had already been read by earlier runs;
- `--samples 2000` per run, five runs, the figure below being the median of the five per-run percentiles;
- nearest-rank percentiles, so every figure printed was observed.

## Numbers

Per-run p95, microseconds, in run order: 62.01, 68.13, 53.64, 47.93, 46.65 over the socket, and
32.80, 29.47, 29.34, 31.22, 25.90 direct.

| figure | direct | JSON-RPC | delta |
| --- | ---: | ---: | ---: |
| p50 | 21.73 us | 38.31 us | 16.6 us |
| p95 | 29.47 us | 53.64 us | 24.2 us |
| worst pairing of the five runs, p95 | 25.90 us | 68.13 us | 42.2 us |
| max observed | 153.82 us | 218.73 us | 64.9 us |

Even the worst single sample of the whole set, 0.219 ms, is a twentieth of the budget.
The transport would have to become two hundred times more expensive before the rule fired.

Where the round trip goes, at p50 of the median run:

- request encode 0.15 us;
- frame write plus flush 2.36 us;
- socket round trip 34.0 us, of which the server's own store read is 21.0 us and its payload encode rounds to 0;
- response decode 1.33 us.

Transport alone is therefore about 17 us: 13 us of socket and scheduling inside the round trip, plus
the write and the decode on either side.
The NDJSON delimiter scan is 0.02 us over 1291 bytes, 0.05% of the round trip, so framing is not a
cost worth designing against at this payload size.

## The interactive measurement was not taken

This host has no account and no terminal, so the held-key figure the ticket asks for could not be
taken here, and nothing in this file substitutes for it.
W1 in [../pre-daemon/measurements.md](../pre-daemon/measurements.md) stays `NOT TAKEN (no account /
headless host) - owner action`, and so does its daemon-era counterpart.

What the number above is: the socket cost of one preview-sized read, measured against the same read
in process, which is the quantity the 5 ms rule names.
What it is not: the wall clock a user perceives on `j`, which also carries the TUI's MIME walk, the
body render and the paint, none of which the daemon changes and none of which this harness runs.
A daemon-era regression in the perceived figure would therefore not be a transport regression, and
the honest before-and-after still needs Sylvain's machine, a real account and the twenty named rows.

## The cache-and-prefetch contingency stays closed

[../../plans/preview-latency.md](../../plans/preview-latency.md) holds a cache and a background
prefetch in reserve, closed as unnecessary on 2026-09-08, to be reopened only if a measurement
demands them.
This measurement does not.
The daemon adds 0.024 ms at p95 to the preview path, which is 0.14% of one 60 Hz frame, so it cannot
by itself push a keypress past a frame budget that was already met without the cache.
The contingency also carries a schema bump to `INTEGER PRIMARY KEY AUTOINCREMENT` and an explicit
invalidation hook, both of which are real correctness work, and none of it becomes required work on
the strength of a 24 microsecond delta.

The contingency remains reopenable on the interactive figure above, which is the only measurement
that could demand it, and which is still owed.

## Aside: the server's dispatch runs above the direct call

P1a-U1 reported the server's `dispatch` stage running about 40% above the direct call for w2 and w4,
which is odd because both stages run the same `Fixture::run` in the same process.
One investigation was spent on it, with 200 samples per configuration:

| workload | configuration | dispatch p50 | direct p50 | ratio |
| --- | --- | ---: | ---: | ---: |
| w2 | direct inline on the runtime, after the round trip | 8293 us | 6298 us | 1.32 |
| w2 | direct inline, before the round trip | 8299 us | 6241 us | 1.33 |
| w2 | direct on a dedicated OS thread, after | 7930 us | 6947 us | 1.14 |
| w2 | direct on a dedicated OS thread, before | 8436 us | 6515 us | 1.29 |
| w4 | direct inline, after | 11096 us | 10800 us | 1.03 |
| w4 | direct on a dedicated OS thread, after | 10999 us | 11127 us | 0.99 |

Two candidate explanations are ruled out.
Ordering is not it: taking the direct sample first, so the round trip no longer warms the caches for
it, moves nothing.
The tokio runtime is not it either: moving the direct call onto a plain OS thread outside the
runtime made the direct call slower rather than the server faster, which is the opposite of a
scheduling penalty on the server side.

What survives is that the gap is shaped by the workload rather than by the payload.
It is 1.3x on w2, which converts 5000 rows into 5000 structs with fifteen owned strings each, about
75000 small allocations, and it is 1.0x on w4 and on w1, which allocate one large string and a
handful of small ones.
The server thread also allocates and frees a 1.9 MB serialisation buffer on every iteration, so the
leading remaining explanation is per-thread allocator and cache state on a thread doing both jobs,
not the transport.
Note the drop accounting runs the other way: the direct timing includes freeing its result and the
server's `dispatch` does not, so the true gap in work is if anything slightly larger than the table
shows.

It changes no decision.
Transport cost is measured as the round trip minus dispatch, so a dispatch stage that is generous to
the transport makes the 5 ms verdict conservative rather than optimistic, and a daemon that reads on
a per-account runtime thread will be measured again in Phase 3a against W2 rather than against this
harness.
Worth one more look only if a Phase 5 list refetch misses its budget by a margin of this size.

## Reproducing

```sh
cargo run --release --example mkfixture -- --out /tmp/mp-ipc-fixture --rows 5000
cd spikes/ipc-bench
for i in 1 2 3 4 5; do cargo run --release -- --workload w1 --samples 2000 --json; done
cargo run --release -- --workload w2 --samples 200 --direct-thread --json   # the aside
```

The spike is deleted at the Phase 1a exit gate (P1a-U7), so after that commit this file is the
record and the command above no longer runs.
