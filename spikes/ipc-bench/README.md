# ipc-bench

Phase 1a risk spike (ticket #0119): what a JSON-RPC round trip over a Unix socket costs against the
same work done as a direct library call (P1a-U1, P1a-U2), what a 5000-row list costs whole against
paged (P1a-U3), how a payload over the frame cap should travel (P1a-U4), and how many read
connections an account needs under load (P1a-U5).

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

Flags: `--workload <id>`, `--samples N` (default 2000), `--warmup N` (default 20, untimed),
`--fixture <dir>` (default `/tmp/mp-ipc-fixture`), `--json`, `--direct-thread`, `--direct-first`,
`--delivery <single|chunked|handle>`, `--handle-dir <dir>`, `--pool N`, `--contend`,
`--writer-only`.

`--direct-thread` runs the direct half of the A/B on a dedicated OS thread outside the tokio runtime
instead of inline on the runtime, and `--direct-first` takes it before the round trip rather than
after. Both exist for one question, why the server's `dispatch` stage runs above the direct call on
w2; the answer is in `docs/baselines/decisions/transport.md`.

`--release` matters: a debug round trip is roughly five times a release one, and the ratio between
the stages is not the same either.

## Workloads

| id | work | answer |
|---|---|---|
| `w1` | one envelope plus its body, `<alpha-inbox-42@fixture.invalid>` (the cursor-move preview) | ~1.2 kB, 1 frame |
| `w2` | `list_mailbox(alpha, Bulk)`, 5000 rows | ~1.9 MB, 1 frame |
| `w3` | `list_account(alpha)`, the whole-account dump, chunked in 200-row frames by default | ~2.1 MB, 28 frames |
| `w4` | the 10 MiB body, `<big-body@fixture.invalid>` | ~10.2 MB, 1 frame |
| `whole` | `list_mailbox(alpha, Bulk)`, 5000 rows, compact positional encoding | ~1.23 MB, 1 frame |
| `page` | 200 rows of the same listing, `LIMIT`/`OFFSET`, compact | ~50 kB, 1 frame |
| `count` | the mailbox total the paged client cannot compute itself | 39 B, 1 frame |
| `jump` | `message.jump_to_date`: the index, plus the page it lands in | ~50 kB, 1 frame |
| `filter` | `message.filter`: the unread count, plus the first page of matches | ~49 kB, 1 frame |
| `select_all` | `message.select_all`: 5000 row ids | ~24 kB, 1 frame |

Two composites run several round trips as one sample and sum their stages: `paged-sync` is
`page` + `count`, what every sync event costs under paging, and `paged` adds `jump`, `filter` and
`select_all`, what a sync event plus one of each interaction costs. A composite run prints a
per-step table and carries a `steps` array in its JSON.

The P1a-U3 workloads encode a row as a positional JSON array rather than an object (`CompactEnvelope`
in `src/proto.rs`), same fifteen fields in the same order, which is the "compact encoding" the unit
asks for. The paged ones run the `list_mailbox` SQL with a `LIMIT`/`OFFSET` added, because the
product read path has no paged variant and the spike may not add one to `src/`; that gives paging
credit for the smaller read it really makes.

`w2`, `w3` and `w4` all answer over the plan's 1 MiB frame cap. The cap is enforced on requests only:
what to do about the responses is P1a-U4's decision, and a harness that refused to send them could
not measure the thing that decision needs.

## Delivery (P1a-U4)

`--delivery` sends the same answer three ways, which is the comparison
`docs/baselines/decisions/large-payloads.md` rests on:

| value | what the client sees |
|---|---|
| `single` | one response frame carrying the whole payload, cap or no cap |
| `chunked` | `bench.chunk` notifications (200 rows, or 256 KiB of a body) then a response with the counts |
| `handle` | a response carrying `{"handle": {"path", "expires_at", "bytes"}}`, the payload in a file the client reads and decodes |

The default is `chunked` for `w3`, the whole-account dump, and `single` for everything else, so the
P1a-U1 and P1a-U3 recipes measure what they measured before this flag existed.

A handle holds NDJSON, one record per line, when the answer is rows; when it is a body it holds the
raw bytes and the envelope stays inline in the frame. The file is written under `--handle-dir`
(default: the system temp dir, tmpfs on this host) and is never fsynced. The server unlinks the
previous handle when it writes the next one, so a run leaves one file behind and not `samples` of
them. The client verifies the file length against `handle.bytes` on every sample.

Stages `handle_write` (server) and `handle_read` (client) are reported beside the others, and
`file_bytes` and `throughput_mbps` join `bytes` in the JSON.

## The read pool (P1a-U5)

`--pool N` replaces the whole A/B with a different scenario: the server holds N read connections,
each owned by one thread (`Connection: Send + !Sync`), and the measured client asks for `w1` in a
loop. `--contend` puts a second client asking for the whole 5000-row listing back to back and a
writer thread committing sync-like transactions on its own connection; `--writer-only` keeps the
writer and drops the list load, which separates what a WAL writer costs a reader from what the
reader queue costs it.

The writer mutates the store it runs against (it flips `\Seen` on `Bulk` rows and inserts rows into
a `SyncScratch` mailbox), so point `--fixture` at a copy rather than at the one the other workloads
read.

The scenario prints its own object: `{scenario, pool, contend, load, samples, preview_p50_us,
preview_p95_us, preview_max_us, load_samples, load_p50_us, load_p95_us, writes, write_p50_us,
write_p95_us}`. `src/pool.rs` has its own connection handler, simpler than `src/server.rs`: this
scenario asks about queueing, so it carries neither chunking nor handles.

## Output

`--json` writes one object to stdout:

```json
{"workload":"w1","delivery":"single","samples":2000,"p50_us":38.37,"p95_us":85.31,"max_us":267.94,
 "stages":{"serialize":{...},"frame_write":{...},"round_trip":{...},"dispatch":{...},
           "server_serialize":{...},"deserialize":{...},"direct":{...}},
 "framing":{"delimiter_scan_us_p50":0.02,"delimiter_scan_us_max":1.46,
            "scanned_bytes":1291,"share_of_p50_pct":0.05},
 "bytes":1197,"frames":1,"file_bytes":0,"throughput_mbps":31.42,
 "steps":[{"method":"w1","bytes":1197,"frames":1,"p50_us":38.41,"p95_us":62.01}],
 "direct_mode":"inline","order":"rpc-first"}
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
