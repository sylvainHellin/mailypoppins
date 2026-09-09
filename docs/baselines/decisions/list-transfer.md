# Decision: whole-list transfer with row-level delta events

Unit P1a-U3 of `.agents/workflow/native-gui-daemon/plan.md`, ticket #0119, risk ANO-17.
The question is what the daemon sends a client that has a 5000-row mailbox open when the store's
revision moves: the whole list, or one page plus the round trips paging turns the in-memory list
operations into.

The chosen option is the whole list, transferred once per mailbox open, kept current by row-level
delta events.
It is also the plan's recommendation, but the numbers below decide it rather than the preference:
per sync event the two options are both sub-millisecond, the difference is a one-off at mailbox
open, and paging spends its saving back on the three interactions it would have to move to the
server.

| field | value |
| --- | --- |
| `option` | whole-list transfer, row-level delta events |
| `rows` | 5000, `alpha/Bulk` of the `mkfixture --rows 5000` fixture |
| `whole_list_bytes` | 1 232 703 compact, 1 947 700 with named fields |
| `whole_list_p50_ms` | 13.79 compact, 13.84 named |
| `page_plus_count_p50_ms` | 0.601 for 49 678 bytes |
| `paged_full_p50_ms` | 4.220 for 172 984 bytes over five round trips |
| `host` | Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM, fixture on tmpfs |
| `commit` | `b678ed42571d27e570d286f6e397699f9a09d07b` (branch `daemon`) |

## What was measured

The P1a-U1 spike, `spikes/ipc-bench`, extended for this unit with six workloads and two composites.
Release build, warm cache, 20 untimed warm-ups, 200 timed samples per run, three runs, the median of
the three per-run percentiles reported.
Both options read through the real store: the whole-list option through
`mailypoppins::store::read::list_mailbox`, the paged option through the same SQL with a
`LIMIT`/`OFFSET` added, so paging is credited with the smaller server-side read it really performs.

| option | bytes | frames | p50 | p95 |
| --- | ---: | ---: | ---: | ---: |
| whole list, compact rows | 1 232 703 | 1 | 13.79 ms | 15.09 ms |
| whole list, named fields (w2) | 1 947 700 | 1 | 13.84 ms | 15.57 ms |
| page of 200 plus total count | 49 678 | 2 | 0.601 ms | 0.734 ms |
| page, count, jump, filter, select all | 172 984 | 5 | 4.220 ms | 4.994 ms |

The five paged round trips on their own, as measured inside the composite:

| method | bytes | p50 | p95 |
| --- | ---: | ---: | ---: |
| page of 200 rows | 49 639 | 0.984 ms | 1.182 ms |
| total count | 39 | 0.168 ms | 0.389 ms |
| jump to date, index plus the page it lands in | 49 675 | 0.696 ms | 0.998 ms |
| filter, match count plus the first page of matches | 49 243 | 1.927 ms | 2.235 ms |
| select all, 5000 row ids | 24 388 | 0.384 ms | 0.471 ms |

A compact row is 247 bytes, a named-field row 390 bytes.

Two results are worth stating before the decision.
The compact encoding saves 37% of the bytes and no measurable time, 13.79 ms against 13.84 ms, which
is inside the run-to-run spread: at 1 to 2 MB over a Unix socket the cost is the store read and the
row conversion, not the wire.
The filter is the most expensive paged call because the unread predicate has no index, so a paging
implementation would owe an index that the in-memory filter does not need.

## Why the whole list wins on these numbers

Per sync event, which is the comparison the unit asks for, both options are cheap once the chosen
option carries deltas.
A sync event under the chosen option ships the rows that changed, not the list: at 247 bytes a row a
ten-row delta is 2.5 kB, twice the 1197-byte W1 payload whose whole round trip measured 38 us at p50
(see [transport.md](transport.md)), so it is two orders of magnitude below the paged 0.601 ms.
The paged option cannot go below its page: the client holds only what it can see, so every revision
move costs a page refetch plus a count.

The whole list is therefore not paid per sync event at all.
It is paid once when the mailbox is opened, and the byte totals say how long that one-off takes to
amortise: 1 232 703 bytes against 49 678 per event is about 25 sync events in the same mailbox,
about 7 if the user also jumps, filters and selects all.
On latency the crossover is 23 events, or 3 if the interactions are counted.
A mailbox that stays open across a working day passes both crossovers, and the sessions that do not
are the sessions where 13.8 ms once was never the problem.

Paging also spends its saving back.
`jump_to_date`, `filter` and `select_all` are answered today from the list the client already holds,
at no round trip and no new method.
Under paging they cost 3.0 ms of round trips per interaction set, three new methods, an index for
the filter predicate, and 24 kB every time the user presses select-all.
More expensive than the round trips: they become new behaviour inside a migration whose exit gate is
behaviour parity, which is the plan's own argument and the one the numbers do not overturn.

The one-off itself stays inside the budgets that exist.
W2 in [../pre-daemon/workloads.md](../pre-daemon/workloads.md) sets a 50 ms frame ceiling, and
13.8 ms at p50, 15.1 ms at p95, is under one 60 Hz frame.
The pre-daemon CLI needs 50 ms to list the same 5000 rows in one process, so the daemon-era open is
not a regression the user can perceive.

Even the degenerate case is affordable: a client that ignored the deltas and refetched the whole
list on every sync event would pay 13.8 ms per event, still inside W2's ceiling.
The delta events are what make the option comfortable, not what make it viable.

## The method set this implies, in Phase 5 shape

Adopted:

- `message.list`, params `{"account":str,"mailbox":str,"limit":u32|null}` with `limit: null` meaning
  the whole mailbox, answering `{"account":str,"mailbox":str,"total":u64,"messages":[...]}`, exactly
  the shape P2-U10 pins.
  There is no `offset` parameter, in this phase or a later one: an offset is the paging option, and
  adding it later is a protocol change with a client behaviour change behind it.
- Row-level deltas over the `state.event` notification
  (`params = {instance_id, revision, kind, payload}`), in the P3a-U5 event vocabulary:
  - `Event::Replace { kind: "message.row", payload: {account, mailbox, message} }` for an inserted or
    updated row, keyed on the row id so a repeat replaces the client's row rather than appending a
    second one;
  - `Event::Remove { resource: message(account, row_id) }` for a delete, and for a move out of the
    open mailbox, which is a remove from this list and a replace in another;
  - `Event::Invalidate { resource: mailbox listing, scope: {account, mailbox} }` as the coarse
    fallback that makes the client re-issue `message.list`, which is also what a queue overflow
    degrades into through `state.resync_required`.
- `total` stays a field of the `message.list` response and is maintained by the delta events, so
  there is no separate count method.
- The `messages` rows keep the named-field encoding P2-U10 pins.
  The compact positional encoding measured here buys 37% of the bytes and no time, and a positional
  row makes adding a field a breaking change where a named one is additive.
  It is recorded as a lever, not as the choice: if a later phase needs the bytes, the encoding
  change is free in latency terms and reversible.

Not adopted, and deliberately absent from the protocol:

- `message.jump_to_date`, `message.filter`, `message.select_all`.
  All three stay client-side over the list the client holds, which is what preserves the current
  semantics of jump-to-date, the metadata filter, the flagged filter, `G` and select-all.

## Consequences for other units

P1a-U4 inherits a large payload it cannot avoid.
The whole-list answer is 1.18 MiB compact and 1.86 MiB with named fields, both over the 1 MiB frame
cap, so `message.list` is a large-payload path by construction and the compact encoding does not
rescue it: 5000 rows land 18% over the cap even positionally.
At the spike's 200-row chunk size that is 25 chunk frames, comparable to the 28 frames w3 already
measures, and the P1a-U4 decision therefore has to cover a listing and not only `dump-mailbox` and
bodies.

Phase 3a inherits the delta events as the load-bearing half of this decision.
If row-level deltas degrade to a whole-list invalidation in practice, this option degrades to the
13.8 ms per sync event above, which is inside the ceiling but no longer comfortable, so the
coalescing rules in P3a-U5 are what keep this decision cheap.

Reopen the decision if one mailbox exceeds roughly 18 000 rows, where the one-off transfer crosses
W2's 50 ms frame ceiling at this host's rate of about 2.8 us a row, or if a client is measured
opening and closing the same mailbox often enough to sit below the 25-event crossover.

## What was not measured

The delta-event figure is derived, not observed: it is the compact per-row size times a plausible
delta, anchored on the measured W1 round trip, because the spike has no revision stream to attach a
delta to and building one is P1a-U6's and Phase 3a's work.

The TUI half of W2, frames over 50 ms during a real sync burst, stays `NOT TAKEN (no account /
headless host) - owner action` in [../pre-daemon/measurements.md](../pre-daemon/measurements.md).
This file measures the transfer, not the paint that follows it.

## Reproducing

```sh
cargo run --release --example mkfixture -- --out /tmp/mp-ipc-fixture --rows 5000
cd spikes/ipc-bench
cargo run --release -- --workload whole      --samples 200 --json
cargo run --release -- --workload w2         --samples 200 --json
cargo run --release -- --workload paged-sync --samples 200 --json
cargo run --release -- --workload paged      --samples 200 --json
```

The spike is deleted at the Phase 1a exit gate (P1a-U7), so after that commit this file is the
record and the commands above no longer run.
