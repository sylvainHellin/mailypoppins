# Decision: a temp-file handle above 1 MiB, one frame below it

Unit P1a-U4 of `.agents/workflow/native-gui-daemon/plan.md`, ticket #0119.
The question is how the daemon answers a client whose result does not fit the frame budget: one
oversized frame, a stream of chunk frames, or a file the client reads itself.

The chosen option is the temp-file handle, above a threshold of 1 MiB, with everything at or below
that threshold staying in a single response frame.
Chunked frames are measured here and not adopted.

| field | value |
| --- | --- |
| `option` | temp-file handle above the threshold, single frame below it |
| `threshold` | 1 MiB = 1 048 576 bytes of encoded result payload, the same number as the request frame cap |
| `handle shape` | `{"handle": {"path": str, "expires_at": rfc3339, "bytes": u64}}` |
| `handle content` | NDJSON, one record per line, for record streams; the raw bytes for a body or an attachment |
| `verdict` | handle above 1 MiB; chunked frames not adopted |
| `host` | Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM, fixture on tmpfs |
| `commit` | the `spikes/ipc-bench` extension committed with this file, parent `ca3c61c895df945454a5472c66d30a9ff82b2044` (branch `daemon`) |

## What was measured

The P1a-U1 spike, `spikes/ipc-bench`, extended with a `--delivery single|chunked|handle` flag that
sends the same answer three ways.
Release build, warm cache, 20 untimed warm-ups, three runs per configuration, the median of the
three per-run percentiles reported.
Chunk sizing: 200 rows for a record stream, 256 KiB for a body, which puts the largest chunk frame
at about 259 kB and every option except `single` inside the 1 MiB cap.

Three payloads, each the thing the unit names:

- `w3`, the whole-account `dump-mailbox` NDJSON stream, 2.08 MB of rows;
- `whole`, the 5000-row `message.list` answer in the compact encoding, 1.18 MiB, which
  [list-transfer.md](list-transfer.md) hands to this unit as a large payload it cannot avoid;
- `w4`, the 10 MiB body.

| payload | delivery | wire bytes | file bytes | frames | p50 | p95 | MB/s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 5000-row list | single | 1 232 703 | 0 | 1 | 13.72 ms | 15.10 ms | 89.8 |
| 5000-row list | chunked | 1 234 707 | 0 | 26 | 13.83 ms | 14.95 ms | 89.3 |
| 5000-row list | handle | 147 | 1 232 662 | 1 | 12.04 ms | 12.57 ms | 102.4 |
| account dump | single | 2 082 197 | 0 | 1 | 15.32 ms | 16.92 ms | 135.9 |
| account dump | chunked | 2 084 285 | 0 | 28 | 17.57 ms | 18.26 ms | 118.7 |
| account dump | handle | 147 | 2 082 159 | 1 | 16.55 ms | 17.48 ms | 125.8 |
| 10 MiB body | single | 10 652 622 | 0 | 1 | 27.16 ms | 28.37 ms | 392.2 |
| 10 MiB body | chunked | 10 655 832 | 0 | 42 | 21.33 ms | 22.63 ms | 499.7 |
| 10 MiB body | handle | 531 | 10 485 760 | 1 | 18.44 ms | 19.44 ms | 568.8 |

The store read is the floor under the two record payloads: dispatch alone is 8.55 ms for the listing
and 9.47 ms for the dump, so a delivery option can only move the 4 to 6 ms above it.
The body is the opposite case: its dispatch is 8.87 ms of a 27.16 ms single-frame answer, and the
rest is JSON escaping, one 10.65 MB socket write and one 10.65 MB decode.

## Why the handle wins

The 10 MiB body is where the options separate, and the handle takes it by 32%: 18.44 ms against
27.16 ms for one frame, 21.33 ms against chunked.
Two costs disappear rather than move.
The body is written as its own bytes, so the 166 862 bytes of JSON escaping the wire options pay are
never spent, and the client's decode goes from 2.88 ms to 0.01 ms because there is no JSON to decode,
only a file to read.
What replaces them is a 3.25 ms write on the server and a 4.94 ms read on the client, which is why
the win is 8.7 ms and not 12.

The 5000-row listing goes the same way by a smaller margin, 12.04 ms against 13.72 ms, because it is
mostly store read.

The account dump is the one payload where a single frame beats the handle, 15.32 ms against
16.55 ms, an 8% loss.
The reason is the format and not the mechanism: NDJSON encodes and decodes 5501 records one at a
time, where a single frame encodes and decodes one array in one call.
The loss is real and is accepted, because the ordering contract of `mp dump-mailbox --json` is
per-record and NDJSON is what preserves it, and because 1.2 ms on a whole-account dump is not a
figure any interactive path waits on.

The numbers alone would leave `single` defensible for records and pick `handle` only for bodies.
Three things that a stopwatch does not show decide it the other way:

- A single frame over the cap is not a size, it is the absence of a cap.
  A 2 MB frame today is a 200 MB frame on a large account, and both peers must buffer the whole of
  it before the first byte is usable, so the failure mode is memory rather than latency.
- The handle is needed anyway for attachments (ANO-6), whose bytes are already on disk and whose
  natural answer is a path.
  Chunked frames would be a second large-payload mechanism, with its own reassembly, its own
  ordering rules and its own cancellation semantics, for the two callers a handle already serves.
- Chunked frames earn their keep only on the body, where they land halfway between the other two,
  and lose to a single frame on both record payloads.
  Nothing in the measurement asks for a third option.

The threshold is where a handle starts paying for itself:

| payload | wire bytes | single | handle | handle against single |
| --- | ---: | ---: | ---: | ---: |
| preview (W1) | 1 197 | 38.38 us | 69.15 us | +80% |
| one 200-row page | 49 639 | 463.77 us | 513.14 us | +11% |
| 5000-row list | 1 232 703 | 13.72 ms | 12.04 ms | -12% |
| account dump | 2 082 197 | 15.32 ms | 16.55 ms | +8% |
| 10 MiB body | 10 652 622 | 27.16 ms | 18.44 ms | -32% |

A handle costs about 15 us of file write and read that a frame does not, plus an open and a stat on
each side, and that fixed cost is 80% of a preview round trip and 11% of a page.
It disappears into the noise somewhere between 50 kB and 1.2 MB.
1 MiB sits inside that band, and it is already the request frame cap, so one number governs both
directions of the protocol and no method needs its own.

## The shape, in the form Phase 2 will implement

A response over the threshold answers with the handle in place of the payload:

```json
{"jsonrpc":"2.0","id":7,
 "result":{"handle":{"path":"/home/u/.local/share/mailypoppins/runtime/handles/7f3a.ndjson",
                     "expires_at":"2026-01-01T12:34:56+00:00",
                     "bytes":2082159}}}
```

- `path` is absolute, inside `<data_dir>/runtime/handles/`, mode 0600, in a directory of mode 0700.
  The daemon and the client already agree on `data_dir` (the identity check does it), so a path is
  meaningful to both and to nobody else.
- `expires_at` is RFC 3339 in UTC.
  After it the daemon may unlink the file, and a client that reads late gets an ordinary
  no-such-file error rather than a truncated answer.
  The daemon also unlinks on the next handle for the same connection and on shutdown, so a crash
  leaves at most one stale file per connection, swept at the next start.
- `bytes` is the exact file length, which is what lets a client tell a complete read from a
  truncated one without a checksum.
  The spike's client checks it on every sample.
- The file is not fsynced.
  It is a cache between two processes on one host, and the measurement below says the filesystem
  does not appear in the numbers as long as nobody asks for durability.

Content, by what is being answered:

- Record streams (`message.list` over the threshold, `mailbox.dump`) are NDJSON, one JSON record per
  line, the same record shape the inline answer would have carried, which is exactly today's
  `mp dump-mailbox --json` output.
- A body or an attachment is the raw bytes, unescaped, with the envelope staying inline in the
  frame.
  That is what makes the body case cheap, and it is the shape ANO-6 needs.

Not adopted, and deliberately absent from the protocol:

- Chunk notifications for large results.
  The `state.event` notification stays a notification about state, not a transport for payload
  fragments.
- A response frame cap different from the request cap.
  Both are 1 MiB, which settles the "default 1 MiB pending that decision" the plan leaves open in
  section 3.0.

## The filesystem does not show up

The handle was measured twice, once with the file on tmpfs (`/tmp`) and once on the ext4 root
filesystem, three runs of 100 samples each:

| payload | handle on tmpfs | handle on ext4 |
| --- | ---: | ---: |
| 5000-row list | 12.04 ms | 12.03 ms |
| account dump | 16.55 ms | 16.46 ms |
| 10 MiB body | 18.44 ms | 16.20 ms |

The differences are inside the run-to-run spread, and the body run on ext4 came out faster than on
tmpfs, which is a page-cache effect and not a claim about disks.
Nothing is fsynced, so a write is a memcpy into the page cache and the read that follows it, seconds
later at most, is served from the same pages.
This is what makes `<data_dir>/runtime/handles/` an acceptable location even when `data_dir` is on
spinning rust: the handle never has to reach the platter.
It also means a handle is not free of the disk forever.
An account whose dumps exceed the page cache would pay the real device, which is a reason to keep
the expiry short rather than a reason to change the location.

## Consequences for other units

- P2 fixes the response cap at 1 MiB and the `frame_too_large` error (-32004) applies to responses
  the daemon refuses to inline, which it now never has to: over the threshold it answers a handle.
- `message.list` at 5000 rows is 1.18 MiB compact and 1.86 MiB named ([list-transfer.md](list-transfer.md)),
  so the listing method crosses the threshold at about 2700 named rows, or 4200 in the compact
  encoding, and answers a handle above that.
  Phase 5's client therefore needs the handle reader on the listing path and not only on dumps,
  which is the consequence list-transfer.md predicted.
- ANO-6 attachments reuse this shape unchanged: the file already exists in the blob store, so the
  daemon can hand out a path to it rather than a copy, and only the expiry and the mode need
  thinking about.
- The handle directory is part of the runtime layout in section 3.0 and needs the same 0700
  treatment as the socket.

## What was not measured

- The daemon materialises the whole payload in memory before writing the file.
  A daemon that streamed rows to the file as it read them would hold less, and that is the shape to
  build, but the spike measures the delivery and not the daemon's peak RSS.
- One reader per handle.
  Two clients reading one handle at once, or a client reading a handle while the daemon writes the
  next one, is not covered here.
- Cancellation.
  What a client does with a handle whose request it cancelled, and when the daemon may unlink it, is
  P3's `operation.*` work.
- Handles under disk pressure, and the eviction policy for a handle directory that grows faster than
  its expiry drains it.

## Reproducing

```sh
cargo run --release --example mkfixture -- --out /tmp/mp-ipc-fixture --rows 5000
cd spikes/ipc-bench
for d in single chunked handle; do
  for w in whole w3 w4; do
    cargo run --release -- --workload $w --delivery $d --samples 100 --json
  done
done
cargo run --release -- --workload w1   --delivery handle --samples 500 --json   # the threshold
cargo run --release -- --workload page --delivery handle --samples 500 --json
cargo run --release -- --workload w4   --delivery handle --handle-dir ~/.cache/ipc-bench-handles --samples 100 --json
```

The spike is deleted at the Phase 1a exit gate (P1a-U7), so after that commit this file is the
record and the commands above no longer run.
