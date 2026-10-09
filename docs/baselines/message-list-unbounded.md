# `message.list` with `limit: null` at 50 000 rows

Until #0138 the TUI's `Action::LoadMailbox` and the desktop's mailbox open both sent `message.list` with `limit: null`, the whole mailbox in one answer (`docs/baselines/decisions/list-transfer.md`).
This file measures what that answer costs the daemon at ten times the 5000-row size the list-transfer decision was taken at, before and after the (perf) change of 2026-10-01 that removed the per-row date re-parse, the per-row clones and the Tokio worker the read used to hold.
Both clients now open a mailbox with `message.list_stream`, whose figures are in "The stream" below.

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
`message.list` with `limit: null` still answers `frame_too_large` there, and keeps doing so for a client that still sends it.

#0138 added `message.list_stream`, which answers with the listing's head and streams the rows in `message.rows` frames of about 1 MiB, and both clients open a mailbox with it.
At 50 000 rows of this fixture that is 24 frames, 24 389 557 bytes in all, the largest 1 049 141 bytes.
`tests/daemon_list_stream_cap.rs` holds the regression: 50 000 rows of 526 bytes, `message.list` with `limit: null` refused with `-32004`, every row collected through `mp-client`'s session thread, and no frame over the chunk budget plus one row plus the envelope.

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

## The stream

`message_list_unbounded_bench` gained two steps for `message.list_stream`, on the same fixture and with the same protocol:

- the daemon's side, the store read the handler makes before it answers plus every row encoded into `message.rows` frames as bytes, the producer's work without the socket writes;
- the client's side, as `mp-client`'s collector did it until PERSO-106, every frame through the line decoder into a `serde_json::Value`, the notification out of it, and every row through `row_from_wire` into one `Vec`.

The baseline host above was not available, so the figures were taken on the home server, with the `message.list` steps re-run beside them so the two methods compare under one load.

| field | value |
| --- | --- |
| `rows` | 50 000, `alpha/Bulk` of `mkfixture --rows 50000`, regenerated for this run |
| `stream` | 24 frames, 24 389 557 bytes, the largest 1 049 141 |
| `commit` | `feb5280` plus the bench steps, branch `perso-0138-list-stream` |
| `host` | Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS (16 threads), 28 GiB, ext4 on NVMe, `rustc 1.96.0`, load average 0.8 to 1.9 |

Milliseconds, `median min max`, 50 000 rows, two runs.

| step | run 1 | run 2 |
| --- | --- | --- |
| store read alone (`read::list_mailbox`) | 89.0 88.7 90.8 | 88.1 87.6 89.5 |
| `message.list`: store read plus the rows as a `Value` | 490.6 486.2 493.9 | 482.6 479.6 487.2 |
| `frame::encode` of that reply | 43.5 43.0 43.7 | 42.8 42.2 44.1 |
| `message.list_stream`: store read plus every chunk as bytes | **110.9** 110.1 112.1 | **110.6** 110.2 111.7 |
| client decode of the chunks into rows, through a `Value` | 183.5 182.3 186.1 | 173.6 170.8 177.9 |

On this host the stream's daemon side costs 111 ms against 534 ms for `message.list` plus its encode, a factor of 4.8, because it builds no `Value` tree: the read is 89 ms and the chunks about 22 ms.
This host builds and drops the `Value` tree three times slower than the baseline host (490 against 156 ms for the method), where the store read is only twice as slow (89 against 41), so the ratio does not carry over directly.
On the baseline host the expected figure is the 41 ms read plus about 15 to 30 ms of chunks, under the 80 ms the ticket set; that figure has not been measured there.

In this run the client's decode was the larger half of a mailbox open: every chunk became a `Value` before its rows were typed, because `mp_protocol::frame::Decoder` yielded `Value`s.

## The typed chunk decode

PERSO-106 decodes each `message.rows` frame straight into a `MessageRowsChunk`: `Decoder::push_with` hands `mp-client`'s connection the frame line, and `MessageRowsChunk::from_frame_line` decodes a frame in the daemon's own spelling without the `Value` tree.
The bench's client step now times that path, and prints the old one beside it as a reference line.

It has not been run on the `mkfixture` fixture.
A standalone timing of the two client paths, on 50 000 synthetic rows of 623 bytes in 30 frames of about 1 MiB, measured the typed decode about three times faster, median of eleven runs:

| client decode of 50 000 rows | run 1 | run 2 |
| --- | --- | --- |
| through a `Value`, then `row_from_wire` | 124.3 | 140.0 |
| typed, straight from the frame | **39.7** | **44.0** |

That run was on the baseline host, macOS 26.6.2 on the Apple M4 Pro, `rustc 1.98.1`, at a load average of 36 to 39 on 12 cores, so the absolute figures are loose and the ratio is the result.

## Not taken

The end-to-end CLI figure (`mp list-messages --mailbox Bulk -n 30000` over a warm sandboxed daemon, the W2 harness at a larger `-n`) was taken before and after, but the host load made the same binary vary by 2x between rounds, so no figure from it is recorded.
In a quiet moment before the change it read 59.5 ms at `-n 5000` and 255.9 ms at `-n 30000`, CLI rendering included.
