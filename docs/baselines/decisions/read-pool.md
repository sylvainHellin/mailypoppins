# Decision: two read connections per account

Unit P1a-U5 of `.agents/workflow/native-gui-daemon/plan.md`, ticket #0119.
The question is how many read connections a per-account runtime holds, measured under load rather
than idle: what the preview read costs while a 5000-row listing and a sync-like write are in flight.

The chosen size is two.
One is not enough, because a single connection puts the preview behind whatever long read is already
running, and four buys almost nothing over two.

| field | value |
| --- | --- |
| `pool_size` | 2 read connections per account, plus the writer connection, which is not in the pool |
| `preview_p95_us` | 219.69 at size 1, 84.19 at size 2, 79.70 at size 4, all under contention |
| `constraint` | `rusqlite::Connection` is `Send` but not `Sync` (`docs/plans/preview-latency.md`), so a pool is N connections each owned by one thread, never one connection shared behind a lock |
| `host` | Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS (16 threads), 28 GiB RAM, fixture on tmpfs |
| `commit` | the `spikes/ipc-bench` extension committed with this file, parent `ca3c61c895df945454a5472c66d30a9ff82b2044` (branch `daemon`) |

## The constraint the shape has to honour

`rusqlite::Connection` is `Send` but not `Sync`.
`docs/plans/preview-latency.md` records it twice: every read opens a fresh connection with its
pragmas (`src/store/mod.rs:210`) precisely because a connection cannot be parked on a shared
`AccountState`, and the idea of caching one there is filed as blocked on the same fact.

A pool therefore cannot be a connection behind a `Mutex` handed round the tokio runtime, and it
cannot be a connection captured by several tasks.
It is N threads, each owning one `Connection` for its whole life, fed by a channel, with the async
side waiting for a free one.
That is what the spike implements (`spikes/ipc-bench/src/pool.rs`): N worker threads each with their
own `Store`, a semaphore whose permits are the idle connections, and a `oneshot` per job for the
answer.
The writer is a separate connection on its own thread, outside the pool, so a write never occupies a
read slot.

## What was measured

The measured client asks for W1, one envelope plus its body, 2000 samples per run, three runs per
configuration, the median of the three per-run percentiles reported.
Beside it, on the same account store:

- a second client asking for the whole 5000-row `Bulk` listing back to back, each answer 12 ms of
  store read and 1.2 MB of payload;
- a writer thread committing sync-like transactions back to back on its own connection, each one
  flipping `\Seen` on a band of 200 real rows and inserting 50 more, 1.55 ms per commit at p50.

Both clients speak to the same daemon over the same socket, so the pool is the only thing that
decides whether the preview waits.

| pool | load | preview p50 | preview p95 | preview max |
| ---: | --- | ---: | ---: | ---: |
| 1 | list + writer | 51.84 us | 219.69 us | 11.36 ms |
| 2 | list + writer | 48.41 us | 84.19 us | 1.09 ms |
| 4 | list + writer | 47.40 us | 79.70 us | 0.81 ms |
| 1 | writer only | 43.20 us | 57.58 us | 0.17 ms |
| 2 | writer only | 41.20 us | 64.60 us | 0.21 ms |
| 4 | writer only | 42.85 us | 60.10 us | 0.21 ms |
| 1 | idle | 44.72 us | 60.43 us | 0.22 ms |
| 2 | idle | 46.85 us | 60.20 us | 0.21 ms |
| 4 | idle | 42.73 us | 62.56 us | 0.19 ms |

The list load itself was answered in 11.73 ms at p50 with one connection and 12.72 ms with four; the
writer committed 1.55 ms transactions at every size.
Neither of them improves with the pool, which is expected: each is one request at a time from one
caller, and the pool only decides who waits behind whom.

## Reading the numbers

Size 1 is the only configuration with a bad tail, and the tail has one cause.
The worst preview sample under contention is 11.36 ms, which is one whole-list read (12 ms), so a
preview that arrives while the list is being read waits for all of it.
That is head-of-line blocking and not a store or a scheduler effect, and it is the failure the pool
exists to prevent: at 60 Hz a frame is 16.7 ms, so a single blocked preview eats most of one.

The writer is not what hurts readers.
With the writer running and no list load, preview p95 is 57.58 us at size 1, which is the idle
figure (60.43 us) to within the run-to-run spread, and the maximum stays at 0.17 ms.
WAL is doing exactly what it promises: a writer does not block readers, and 65 committed
transactions during a run cost the preview nothing measurable.
So pool sizing is about concurrent long reads, and the sync write is a red herring the measurement
had to rule out rather than a load to size against.

Going from one to two removes the block: p95 falls from 219.69 us to 84.19 us, a factor of 2.6, and
the maximum falls by a factor of ten, from 11.36 ms to 1.09 ms.
Going from two to four moves p95 by 4.5 us, 5%, and the maximum by 0.28 ms, both close enough to the
spread that a fourth run could reorder them.
Two connections is where the curve flattens.

The cost of the connections that would buy that 5% is not nothing.
Each is a thread, an open file descriptor set, a SQLite page cache and a statement cache, per
account and not per daemon, so a user with six accounts pays for 24 connections instead of 12 to
remove a quarter of a millisecond from a tail that is already a twentieth of a frame.
Two also matches the load: at any moment an account has at most one long read (a listing, a search)
and one short one (a preview), which is the shape the pool has to keep from colliding.

## The shape this implies

- Each per-account runtime owns exactly two read connections, each pinned to its own thread for the
  life of the runtime, opened through `Store::open` with the existing pragmas.
- A read is dispatched to a free connection and waits when both are busy, FIFO, so a preview behind
  a preview is still fast and a preview behind two long reads degrades to the size-1 case rather
  than to an error.
- The writer connection is separate and outside the pool, one per account, because a sync
  transaction must never take a read slot and because SQLite serialises writers anyway.
- The pool size is a constant in this phase, not a config key.
  Nothing in the measurement supports a knob, and a knob would need a support answer for what to set
  it to.

## Reopen the decision if

- Two long reads become normal on one account, which is what a second GUI window on the same account
  or a full-text search running beside a listing would do.
  Under that load the size-2 configuration behaves like size 1 does here, and the table says the fix
  is four.
- A listing stops being 12 ms.
  The whole argument is that a preview must not queue behind a read of that length; a read path an
  order of magnitude slower would move the p95, not just the maximum.
- The daemon moves reads onto `spawn_blocking` instead of dedicated threads.
  That is a different pool with a different queueing discipline, and the numbers here would not
  carry over.

## What was not measured

- Real IMAP sync writes.
  The writer here is a synthetic transaction of 200 updates and 50 inserts on the fixture; a real
  sync also writes blobs, touches the FTS index and commits in bursts rather than continuously.
  The WAL result (a writer costs concurrent readers nothing) is a property of the journal mode
  rather than of the workload, so the conclusion should survive, but the figure is not the figure a
  real sync would produce.
- More than two clients.
  A daemon serving a TUI, a GUI and a CLI at once would put three readers on the pool, which is the
  first case where size 4 might repay its cost.
- Checkpointing.
  The runs are short enough that no WAL checkpoint of consequence happened during one, and a
  checkpoint is the one moment a writer does interfere with readers.
- Memory.
  The per-connection cost is argued above from what SQLite allocates, not from a measurement of the
  daemon's RSS at each pool size.

## Reproducing

```sh
cargo run --release --example mkfixture -- --out /tmp/mp-ipc-fixture --rows 5000
cp -a /tmp/mp-ipc-fixture /tmp/mp-ipc-fixture-pool   # the writer mutates its fixture
cd spikes/ipc-bench
for p in 1 2 4; do
  cargo run --release -- --fixture /tmp/mp-ipc-fixture-pool --pool $p --contend     --samples 2000 --json
  cargo run --release -- --fixture /tmp/mp-ipc-fixture-pool --pool $p --writer-only --samples 2000 --json
  cargo run --release -- --fixture /tmp/mp-ipc-fixture-pool --pool $p               --samples 2000 --json
done
```

The spike is deleted at the Phase 1a exit gate (P1a-U7), so after that commit this file is the
record and the commands above no longer run.
