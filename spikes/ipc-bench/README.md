# ipc-bench

Phase 1a risk spike (ticket #0119, unit P1a-U1): what a JSON-RPC round trip over a Unix socket costs
against the same work done as a direct library call.

Standalone Cargo package with its own `Cargo.lock` and an empty `[workspace]` table, so it is never a
member of the root package: `cargo test` at the repo root neither builds it nor counts its tests.
It does take a path dependency on `mailypoppins`, because both sides of the comparison must read the
real store through the real read path (`mailypoppins::store::read`). Everything here is deleted at
the Phase 1a exit gate; only the measurements survive, under `docs/baselines/`.

## Run recipe

```sh
cargo run --release --example mkfixture -- --out /tmp/mp-ipc-fixture --rows 5000   # from the repo root
cd spikes/ipc-bench
cargo run --release -- --workload w1 --samples 2000 --json
```

The fixture is the one `docs/baselines/pre-daemon/workloads.md` defines. `--fixture <dir>` points at
another one; when the directory holds no store the harness builds it with exactly the command above
(`--rows 5000`), so a first run on a clean machine works with no arguments.

Flags: `--workload w1|w2|w3|w4`, `--samples N` (default 2000), `--warmup N` (default 20, untimed),
`--fixture <dir>` (default `/tmp/mp-ipc-fixture`), `--json`.

`--release` matters: a debug round trip is roughly five times a release one, and the ratio between
the stages is not the same either.

## Workloads

| id | work | answer |
|---|---|---|
| `w1` | one envelope plus its body, `<alpha-inbox-42@fixture.invalid>` (the cursor-move preview) | ~1.2 kB, 1 frame |
| `w2` | `list_mailbox(alpha, Bulk)`, 5000 rows | ~1.9 MB, 1 frame |
| `w3` | `list_account(alpha)`, 5501 rows, streamed in 200-row chunks | ~2.1 MB, 28 frames |
| `w4` | the 10 MiB body, `<big-body@fixture.invalid>` | ~10.2 MB, 1 frame |

`w2`, `w3` and `w4` all answer over the plan's 1 MiB frame cap. The cap is enforced on requests only:
what to do about the responses is P1a-U4's decision, and a harness that refused to send them could
not measure the thing that decision needs.

## Output

`--json` writes one object to stdout:

```json
{"workload":"w1","samples":2000,"p50_us":38.37,"p95_us":85.31,"max_us":267.94,
 "stages":{"serialize":{...},"frame_write":{...},"round_trip":{...},"dispatch":{...},
           "server_serialize":{...},"deserialize":{...},"direct":{...}},
 "framing":{"delimiter_scan_us_p50":0.02,"delimiter_scan_us_max":1.46,
            "scanned_bytes":1291,"share_of_p50_pct":0.05},
 "bytes":1197,"frames":1}
```

Top-level `p50_us` / `p95_us` / `max_us` are the client-observed round trip: everything from the
first byte of request encoding to the last byte of response decoding. The stages:

| stage | measured where | covers |
|---|---|---|
| `serialize` | client | encoding the request struct |
| `frame_write` | client | payload + `\n` + flush |
| `round_trip` | client | flush to the last byte of the last frame; contains `dispatch` and `server_serialize` |
| `dispatch` | server, reported in the response | the store read and the row conversion |
| `server_serialize` | server, reported in the response | encoding the result payload (all frames) |
| `deserialize` | client | decoding every frame into typed values |
| `direct` | harness | the same `Fixture::run` call in process, no socket |

`round_trip` is not the sum of the server stages plus the socket: it also carries the server's frame
writes and both scheduler hops, which is the point of measuring it as one span.

Read and decode are deliberately not interleaved on the client: all frames are read as bytes first,
then decoded, so the two stages can be charged separately. A production client would overlap them,
which makes the split an upper bound on each stage rather than on their sum.

Percentiles are nearest-rank, so every figure printed was observed.

`framing` is the NDJSON tax on its own: the cost of finding the `\n` in every frame of one real
answer, which a length-prefixed framing would not pay. `share_of_p50_pct` puts it against the round
trip p50.

## Shape notes

- One connection, requests answered in order. The store handle lives in the connection task and is
  never shared, which is the `rusqlite::Connection: Send + !Sync` constraint
  `docs/plans/preview-latency.md` records.
- Store reads are blocking calls made from an async task on purpose: a `spawn_blocking` hop would
  add a scheduler cost a per-account runtime would not pay the same way, and the spike measures
  transport.
- The server encodes the result payload once into a `RawValue` and splices it into the response
  envelope, so `server_serialize` is the payload encode and nothing else.
- Chunk frames are told from response frames by their `"method":` prefix, without decoding: the same
  test a real client makes.
- "No embedded raw newlines" holds for free: `serde_json` escapes `\n` inside strings, which is also
  why the 10 MiB body arrives as 10.15 MB of JSON.
