# Phase 6 soak

Two of the Phase 6 exit gate lines (`.agents/workflow/native-gui-daemon/plan.md`, section 4) are statements about a daemon that keeps running rather than about one that answers correctly:

> The daemon can run unattended across client churn.
> Memory remains bounded under slow clients and repeated syncs.

`tests/daemon_soak.rs` (unit P6-U9, ticket [#0125](../tickets/0125-daemon-hardening.md)) is what proves them, and this file is the run that landed it: the method, the host, the measured numbers, the ceilings they justify, and the one leak the measuring found.

Taken on 2026-09-21 on branch `daemon`, at the commit that adds the file.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 16 threads, 28 GiB RAM, `/var/tmp` on the root ext4 filesystem.
Debug build, which is what `cargo test` runs: every latency below is therefore a pessimistic figure and is compared with a ceiling, never with a release baseline.

## Method

Each row starts its own `mp daemon run` against its own copy of an `examples/mkfixture --rows 5000` root (two accounts, six mailboxes, 5501 messages, one 10 MiB body), plus an `[accounts.imap]` host for `alpha` so `sync.quick` runs a real pass instead of being refused as local-only, an `[email] send_hold_secs`, and one approved draft for the hold row.
The template is built once per machine into `$TMPDIR/mp-soak-fixture-v1` and copied per row, because two daemons cannot share a data directory.

Every row measures the daemon from the outside, at two moments:

- descriptors: the number of entries in `/proc/<pid>/fd`;
- memory: `VmRSS` from `/proc/<pid>/status`;
- what the daemon says about itself: `diagnostic.health`, for `clients`, `operations.active`, `holds` and `store.size_bytes`.

**Both readings are taken against a warm daemon.**
A daemon that has just bound its socket is still starting - the account runtimes come up off the startup path, the read pool opens its connections on the first read that needs them, and SQLite keeps a further handle per concurrent reader - so a cold "before" reading reports the daemon finishing its own startup as a leak.
Measured here: a fresh daemon holds 22 descriptors, the first few dozen concurrent clients take it to 37, and four thousand more leave it at 37.
Each row therefore runs a short burst of concurrent clients first, waits for the descriptor count to stop moving, and only then takes the numbers it will compare.
The "after" reading waits for stability too, because a client's `close` returns before the daemon's read sees the end-of-file.

## Running it

```sh
TMPDIR=/var/tmp cargo test --offline --test daemon_soak -- --nocapture     # ~15 s, the ordinary suite
MAILYPOPPINS_SOAK_SECS=60 TMPDIR=/var/tmp cargo test --offline --test daemon_soak -- --nocapture
```

`MAILYPOPPINS_SOAK_SECS=<n>` is the owner's knob: row (g) runs for `n` seconds and every other row multiplies its counts by `n / 10`.
`--nocapture` is what makes a passing run leave evidence; without it libtest swallows the numbers below.

## The rows, and what each one measured

The short run (default counts, all seven rows in parallel, **15.5 s wall**):

| row | work | descriptors | memory | other |
|---|---|---|---|---|
| a | 200 connect/bootstrap/disconnect cycles from 8 threads, 0.2 s | 48 -> 51 (+3) | +0.3 MiB | `clients` 1 -> 1 |
| b | 50 syncs, 4 readers, 1 stalled client, 4000-event burst per bootstrap, 1.3 s | - | +6.6 MiB | 1 resync for the stalled client, 1 for each reader, every one of the 50 `sync.completed` delivered to all four |
| c | 200 `sync.quick` passes, 1.2 s | - | +0.7 MiB | `operations.active` peak 0, back to 0; `store.size_bytes` unchanged to the byte |
| d | 300 `message.list`/`message.search` from 4 clients, 2.0 s | - | +22.0 MiB | p50 15.3 ms, p95 62.3 ms, max 82.6 ms, no error frame |
| e | 100 drafts written then deleted, 0.3 s | 48 -> 48 (+0) | +0.5 MiB | 100 `draft.changed`, 100 `state.remove` |
| f | 50 holds armed and cancelled from alternating clients, 0.1 s | - | +0.3 MiB | `holds` 0, outbox 0 rows, draft still `approved` |
| g | 10 s mixed run | 47 -> 47 (+0) | +76.7 MiB | 2567 churn cycles, 1853 syncs, 587 reads, 474 draft write/delete pairs, 6696 holds, 23790 events drained, `daemon.stop` `clean: true` |

The long run (`MAILYPOPPINS_SOAK_SECS=60`, **65.6 s wall**):

| row | work | descriptors | memory | other |
|---|---|---|---|---|
| a | 1200 cycles, 1.3 s | 46 -> 50 (+4) | +0.1 MiB | `clients` 1 -> 1 |
| b | 300 syncs, 4.7 s | - | +5.7 MiB | 1 resync each, all 300 outcomes delivered to all four readers |
| c | 1200 passes, 8.2 s | - | +0.9 MiB | `store.size_bytes` unchanged |
| d | 1800 reads, 11.5 s | - | +43.5 MiB | p50 14.2 ms, p95 61.3 ms, max 106.1 ms |
| e | 600 drafts, 2.0 s | 48 -> 48 (+0) | +0.9 MiB | 600 and 600 |
| f | 300 holds, 0.6 s | - | +1.1 MiB | `holds` 0 |
| g | 60 s mixed run | 47 -> 47 (+0) | +60.8 MiB | 14667 churn cycles, 10596 syncs, 3445 reads, 2840 draft pairs, 37018 holds, 132242 events, stop clean |

Three consecutive short runs took 14.1 s, 14.2 s and 13.8 s wall.

## The ceilings, and why they are where they are

| ceiling | value | justification |
|---|---|---|
| `FD_MARGIN` | 12 descriptors | measured drift against a warm baseline is +3 at 200 cycles and +4 at 1200; a leak of one descriptor per cycle would be two hundred |
| `RSS_CEILING_BYTES` | 192 MiB | worst measured growth is 76.7 MiB (row g, short run); the ceiling is twice that |
| `P95_CEILING` | 250 ms | measured p95 is 62 ms in a debug build with all seven rows competing for the machine; a lock convoy costs hundreds of milliseconds, not tens |
| `STORE_MARGIN_BYTES` | 4 MiB | measured movement across 1200 passes that ingest nothing is 0 bytes |
| `BURST` | 4000 events per bootstrap | above the 512-event queue plus the kernel socket buffer by a wide margin, which is what makes the overflow deterministic |
| `DRAFT_BATCH` | 100 drafts | under the 512-event queue: a batch larger than it overflows the subscriber and the daemon is entitled to answer with a resync instead of the events |

A ceiling alone is a poor leak detector - every one of these is far above the measurement it guards - so **the rows print their numbers and the numbers are the instrument**.
The leak below was found by reading row (c)'s growth at two scales, not by any assertion failing.

## What the soak found: the operation registry never forgot anything

`OperationRegistry` (`src/daemon/operations.rs`) kept every operation the daemon process had ever started, live or settled, so `operation.status` could still answer for it.
Measured before the fix:

| row (c) passes | RSS growth |
|---:|---|
| 200 | +0.6 MiB |
| 1200 | +0.7 MiB |
| 3600 | +2.7 MiB |

About 0.8 KiB per settled operation, never returned.
A daemon that quick-syncs every five minutes for a year holds a hundred thousand of them; a TUI that starts an operation per keystroke gets there in an afternoon.
The mixed row showed the same shape from the other side: +61.9 MiB at 10 s, +77.9 MiB at 60 s, +86.1 MiB at 180 s.

The fix is a bounded history: a `start` that finds more than `operations::HISTORY` (256) settled entries forgets the oldest of them first, and live operations are never forgotten.
An id past the window answers as an id this daemon never issued, which is what a client that outlived a daemon restart already has to handle.

After the fix the mixed row's growth stops tracking the duration - +61.1 MiB at 10 s, +62.8 MiB at 60 s, +83.7 MiB at 180 s while doing **4.4x** the work of the pre-fix 180 s run - and what is left is working set: allocator arenas under thread churn, SQLite page caches for two stores, and the per-connection buffers of a daemon that has just churned fifty thousand connections.

The same change made the daemon markedly faster under sustained load, because `live()` and every disconnect scan walked the table: the 60 s mixed run went from 113 holds a second to 767.

## What the rows deliberately do not claim

- **They do not separate a fast client from a slow one under a burst.** The `MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST` hook commits its changes in a tight loop, faster than any client drains them, so at 4000 events per bootstrap every subscriber overflows and every subscriber is told to resync exactly once. That a stalled reader does not delay another client's round trip is `tests/daemon_events.rs`'s row, taken at a bootstrap latency, and it is not repeated here.
- **They do not measure a release build.** Every number is from `cargo test`'s debug binary. The latency figures are ceilings-with-headroom, not a benchmark; P6-U10 owns the release-build rerun of the Phase 1a workloads.
- **They are not a test of a real IMAP sync.** `alpha` configures a host that does not resolve and has no stored secret, so each `sync.quick` runs the real pass, fails in about ten milliseconds and commits a real `sync.completed` outcome with `severity: error`. That is what makes 200 passes cost a second; it is also why row (c) can assert that the store did not move.
