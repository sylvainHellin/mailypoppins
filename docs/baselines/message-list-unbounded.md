# `message.list` with `limit: null` at 50 000 rows

The TUI's `Action::LoadMailbox` and the desktop's mailbox open both send `message.list` with `limit: null`, the whole mailbox in one answer (`docs/baselines/decisions/list-transfer.md`).
This file measures what that answer costs the daemon at ten times the 5000-row size the list-transfer decision was taken at, before and after the (perf) change of 2026-10-01 that removed the per-row date re-parse, the per-row clones and the Tokio worker the read used to hold.

| field | value |
| --- | --- |
| `rows` | 50 000, `alpha/Bulk` of `mkfixture --rows 50000` |
| `frame_bytes` | 24 387 191, about 488 per row |
| `response_cap` | 16 777 216 (`mp_protocol::MAX_RESPONSE_BYTES`) |
| `before` | `legacy_row` inside the bench at `1a7bffdc`, the old `json!` code timed in the same run |
| `after` | `1a7bffdc` |
| `host` | macOS 26.6.2, Apple M4 Pro (12 cores), 48 GiB, APFS, `rustc 1.98.1` |

## The answer does not fit in a frame

At 50 000 rows of this fixture's shape the answer is 24.4 MB, over the 16 MiB response cap, so the daemon answers `frame_too_large` (`-32004`) and the client gets no listing at all.
This was so before the change and is so after it, because the change leaves every byte of the answer as it was:

```text
$ mp list-messages --mailbox Bulk -n 50000
Error: the response is 24387191 bytes, over the 16777216-byte response cap
```

At 488 bytes a row the cap falls at about 34 000 rows, fewer for a mailbox with long subjects and recipient lists.
A real mailbox past that size cannot be opened in the TUI or the desktop client.
Fixing it needs paging or a streamed answer, both a change to the `message.list` contract, which this unit was asked not to make; it is the open half of the `BACKLOG.md` line this file closes.

## Method

`message_list_unbounded_bench`, an `#[ignore]`d test in `src/daemon/methods/message.rs`, calls the method in process against the fixture and times each step with the protocol of [pre-daemon/workloads.md](pre-daemon/workloads.md): one discarded warm-up, then eleven runs, median with min and max beside it.
It rebuilds the pre-change answer from the old `json!` literal (`legacy_row`, the oracle `a_wire_row_is_the_json_literal_it_replaced` checks the new rows against byte for byte) in the same run, so the before and after columns are taken under the same host load.

```sh
cargo build --release --example mkfixture
target/release/examples/mkfixture --out /var/tmp/mp-bench-50k --rows 50000   # about 15 minutes
MP_BENCH_FIXTURE=/var/tmp/mp-bench-50k cargo test --release --lib \
  message_list_unbounded_bench -- --ignored --nocapture
```

The host was shared with three other builds throughout (load average 18 to 42 on 12 cores), so absolute figures moved by up to 2x between runs.
The table is the quietest run (load 18); the ratio between the two columns held in every run.

## The table

Milliseconds, `median min max`, 50 000 rows.

| step | before | after |
| --- | --- | --- |
| `message.list`: store read plus the rows as a `Value` | 186.3 180.1 205.3 | **156.0** 153.2 173.0 |
| of which the store read alone (`read::list_mailbox`) | | 41.1 40.5 41.6 |
| `frame::encode` of the reply, the same bytes either way | | 22.1 20.2 22.8 |

The store read selects one column more than before (`date_sort`) and keeps its plan, the `messages_list` index walk.
Two earlier pairs, taken at higher load, read 189.8 against 171.3 and 245.3 against 209.7.
The first bench run, on the old code at `3b442cea` before any change and at a lower load than any pair above, measured 164.2 ms for the method, 36.8 for the store read and 18.4 for the encode.

Two reference timings in the same run say where the rest of the time goes:

| reference | ms |
| --- | --- |
| building the 50 000 borrowed `WireRow`s, no JSON | 9.7 9.7 9.8 |
| the same rows serialised straight to bytes, no `Value` | 31.5 31.3 31.8 |

## What the numbers say

The change saves 10 to 16% of the method, 19 to 35 ms at 50 000 rows across the three pairs, all of it per-row work: the RFC 2822 parse of `date_display` that ingest had already done into the `date_sort` column, and the two clones per string field the `json!` literal made.

The rest of the method is the `serde_json::Value` tree, not the rows: about 115 ms of the 156 build and drop 50 000 objects of fifteen keys each, where serialising the same borrowed rows straight to bytes takes 31.5 ms.
An answer that skipped the tree would cost about 41 + 32 = 73 ms and save the 22 ms of `frame::encode` on top, but `Outcome::result` is a `Value` that the server and twelve in-process test fixtures read, so that is a dispatcher change rather than a `message.list` one.

The read no longer runs on a Tokio worker: the `list` projection's store read and row building run on the blocking pool (`list_off_thread`), so a large listing stops holding a worker every other connection shares.
That is a scheduling change and has no row in the table.

## Not taken

The end-to-end CLI figure (`mp list-messages --mailbox Bulk -n 30000` over a warm sandboxed daemon, the W2 harness at a larger `-n`) was taken before and after, but the host load made the same binary vary by 2x between rounds, so no figure from it is recorded.
In a quiet moment before the change it read 59.5 ms at `-n 5000` and 255.9 ms at `-n 30000`, CLI rendering included.
