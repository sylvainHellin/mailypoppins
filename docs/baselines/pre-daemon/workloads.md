# Benchmark workloads

The eight workloads every later phase of the native-GUI/daemon migration is measured against
(`.agents/workflow/native-gui-daemon/plan.md`, phase 0, ticket #0118).
Each one names its exact command, the single number it produces, and the rule that decides whether
a daemon-era rerun passes.
The numbers themselves live in [measurements.md](measurements.md), never in this file: a workload
definition is meant to survive every phase unchanged, and a definition that carried its own results
would be edited every time one was taken.

Four of the eight run offline against a generated fixture and were taken on this host.
Four need a terminal, a real account or a server round trip, and are recorded as owner action.

## The fixture

`examples/mkfixture.rs` builds the store the offline workloads read.
It is deterministic by construction: one fixed seed, a fixed epoch for the `Date:` headers, explicit
`Message-ID`s, and a fixed body vocabulary, so two runs into two directories produce byte-identical
`mp dump-mailbox --json` output.

```sh
cargo run --release --example mkfixture -- --out /tmp/mp-fixture --rows 5000
export MAILYPOPPINS_CONFIG_DIR=/tmp/mp-fixture/config
export MAILYPOPPINS_DATA_DIR=/tmp/mp-fixture/data
```

What it contains:

- two accounts, `alpha` and `beta`, both declared in the generated `config.toml`;
- six mailboxes: `alpha/inbox` (201), `alpha/sent` (50), `alpha/archive` (100), `alpha/Bulk`
  (`--rows`, 5000 by default), `beta/inbox` (100), `beta/archive` (50);
- one oversized message, `<big-body@fixture.invalid>` in `alpha/inbox`, whose body blob is exactly
  10 MiB (`--big-mb`), dated far past every other message so it sorts last;
- one attachment on every 17th message, a `Cc` on every 11th, `\Seen` on every 3rd;
- the rare token `zolvertrix` in every 250th body of `alpha/Bulk`, so a full-text search has a hit
  count that is a function of `--rows` and not of the vocabulary (20 hits at 5000 rows).

The fixture is written through `ingest::ingest_message`, the same call the sync engine makes, so the
rows are the rows a real sync produces and no benchmark reads a shape the product cannot produce.
No account, network or credential is involved: `mp` prints one SMTP-secret warning on stderr against
this fixture and that warning is expected.

## Run protocol

Pin all of it, or the number is not comparable with the next one.

- Release build, installed with `cargo install --path . --locked`, never `cargo run`.
- Record the commit (`git rev-parse --short HEAD`) with every figure.
- Build the fixture with the exact flags above; a different `--rows` is a different workload.
- Discard one warm-up run, then take the median of eleven, and record min and max beside it.
- Keep the fixture and the binary on the same filesystem across a before/after pair, and say which
  filesystem it was: a tmpfs figure is a lower bound on a disk-backed one.

The harness is three lines of shell, so nothing has to be installed:

```sh
bench() {                       # median-of-11 wall clock in ms
  "$@" >/dev/null 2>&1          # warm-up, discarded
  for i in $(seq 11); do s=$(date +%s%N); "$@" >/dev/null 2>&1; e=$(date +%s%N);
    echo $(( (e-s)/1000000 )); done | sort -n | sed -n '6p'
}
```

## W1: cursor-move preview

One `j` in the TUI message list onto a row with a body and attachments, from a settled UI.
This is the workload the daemon puts a round trip in the middle of, so it is the one that decides
whether the transport is affordable.

Conditions are the ones ticket [#0108](../../tickets/0108-coalesce-key-events.md) pins: release
build, named terminal emulator, recorded OS key-repeat delay and rate, named account and mailbox,
the same twenty rows walked each time, warm page cache after one discarded pass.

```sh
mp                                  # navigate with j/k over the named rows
rg '\[TIMING\] tui_preview_query' "$(ls -1 <data_dir>/logs/mailypoppins-*.log | tail -1)"
rg '\[TIMING\] tui_draw'          "$(ls -1 <data_dir>/logs/mailypoppins-*.log | tail -1)"
```

- Metric: p50 and p95 of `tui_preview_query done: N ms` over twenty cursor moves, and the p95 of the
  `tui_draw` pair that follows each of them.
- Acceptance: the daemon-era p95 of `tui_preview_query` may exceed the pre-daemon p95 by at most
  5 ms, the same budget the transport decision (P1a-U2) is held to, and `tui_draw` p95 must stay
  under one 60 Hz frame plus the preview query.
- Takeable here: no, needs a terminal and an account.

## W2: list refetch at 5000 rows per sync event

What a sync event costs the client that has a 5000-row mailbox open: the whole visible list is
refetched and re-sorted when the store's revision moves.
Pre-daemon this is a direct store read on the UI thread; post-daemon it is a list transfer over the
socket, which is what makes it the input to the paging decision (P1a-U3).

```sh
# TUI, cursor parked in alpha/Bulk, with a sync driving revisions
rg '\[TIMING\] tui_draw' "$(ls -1 <data_dir>/logs/mailypoppins-*.log | tail -1)"
```

The offline lower bound on the same work, which this host can take, is the CLI listing of the same
5000 rows:

```sh
mp list-messages --mailbox Bulk -n 5000
```

- Metric: wall clock of the refetch, and the count of frames over 50 ms during one sync burst.
- Acceptance: no frame over 50 ms, and the daemon-era refetch within 2x the pre-daemon figure; a
  breach is the trigger for paging rather than a bug.
- Takeable here: only the CLI lower bound; the real figure needs a terminal, an account and a sync.

## W3: whole-account `dump-mailbox`

The largest read the CLI can make: every envelope of every mailbox, serialised to NDJSON.
It is the bulk-transfer workload, and the one whose daemon version cannot fit in a single 1 MiB
frame, so it is also the regression test for whatever streaming shape P1a-U4 settles on.

```sh
mp dump-mailbox --json -A alpha > /dev/null
```

- Metric: wall clock of the whole command, median of eleven.
- Acceptance: the daemon-era figure within 1.2x the pre-daemon figure, and the output byte-identical
  to the pre-daemon output for the same fixture.
- Takeable here: yes.

## W4: 10 MiB body fetch

One message whose body blob is 10 MiB, read end to end.
It is the frame-cap workload: the body alone is ten times the 1 MiB request cap, so the daemon
cannot answer it with one frame and the answer shape has to be decided rather than discovered.

```sh
mp show 'inbox/<big-body@fixture.invalid>' > /dev/null
mp show 'inbox/<alpha-inbox-42@fixture.invalid>' > /dev/null   # control, ordinary body
```

- Metric: wall clock of both, median of eleven; the difference is the body cost with process start
  and store open removed.
- Acceptance: the daemon-era figure within 1.5x the pre-daemon figure, and no `frame_too_large`
  (-32004) on the path, ever.
- Takeable here: yes.

## W5: cold first paint

Launching the TUI onto a warm store and painting the first usable frame.
Post-daemon this may include starting the daemon, which is new cost the user did not pay before.

```sh
mp                        # from a shell, to the first painted list
rg '\[TIMING\] tui_draw' "$(ls -1 <data_dir>/logs/mailypoppins-*.log | tail -1)" | head -1
```

- Metric: wall clock from process start to the first `tui_draw done`, median of five launches.
- Acceptance: within 1.25x the pre-daemon figure when the daemon is already running, and within
  1.25x plus 250 ms when the launch has to start the daemon.
- Takeable here: no, needs a terminal and an account.

## W6: cold and warm one-shot CLI

The cost a scripted `mp` invocation pays before it does any work: process start, config load, store
open, one small query.
This is the workload the daemon changes most, because the store open is replaced by a socket
round trip, and the workload that decides whether a cron job should talk to the daemon at all.

```sh
mp --version                                   # process floor, no store touched
mp list-messages --mailbox inbox -n 20 > /dev/null   # warm: repeat invocation
sudo sysctl -w vm.drop_caches=3                # cold: page cache dropped first
mp list-messages --mailbox inbox -n 20 > /dev/null
```

- Metric: wall clock of each, median of eleven for the warm figure and a single run for the cold one
  (a second cold run is not cold).
- Acceptance: the warm daemon-era figure may exceed the pre-daemon figure by at most 25 ms, and an
  invocation that has to start the daemon must stay under 1 s and print nothing on the happy path.
- Takeable here: the floor and the warm figure, yes; the cold figure needs root and a disk-backed
  data dir.

## W7: search

The local full-text index answering a query over 5501 messages.
Two shapes, because they stress different halves: a rare term whose cost is the FTS lookup, and a
common term whose cost is ranking and envelope hydration.

```sh
mp search --local 'body:zolvertrix' -n 100 > /dev/null   # 20 hits at --rows 5000
mp search --local 'body:concrete'   -n 100 > /dev/null   # hundreds of hits, capped at 100
```

- Metric: wall clock of each, median of eleven, with the hit count recorded beside it.
- Acceptance: the daemon-era figure within 1.2x the pre-daemon figure and the hit count identical;
  a changed hit count is a correctness failure, not a performance one.
- Takeable here: yes.

## W8: mutation

A state change one client makes, and the delay before a second client sees it.
Pre-daemon there is no second client and no propagation, so the pre-daemon figure is the write alone
and the propagation figure is structurally absent, which is exactly what the daemon has to improve
on rather than merely match.

```sh
mp                        # toggle \Seen on a row with u, note the wall clock to the repaint
mp show 'inbox/<alpha-inbox-42@fixture.invalid>'   # second process, observe the new flag
```

- Metric: wall clock of the write as the writing client sees it, and the delay until a second client
  reports the new state.
- Acceptance: the write within 1.5x the pre-daemon figure, and the second client seeing the change
  within 200 ms of the write completing, without polling.
- Takeable here: no; every offline mutation the CLI exposes either needs a server (`archive`,
  `delete`) or a draft the fixture does not generate, and the flag toggle is a TUI key.

## Decisions this file had to take

The plan fixed the eight workload names and required an exact command, a metric and an acceptance
rule for each, but not the rules themselves.
The multipliers above (1.2x, 1.25x, 1.5x, 2x), the 25 ms warm-CLI budget, the 250 ms daemon-start
allowance, the 200 ms propagation bound and the 50 ms frame ceiling are this unit's recommendation,
chosen to be loose enough that ordinary noise does not fail a gate and tight enough that a real
regression cannot pass one.
The 5 ms in W1 is not: it is the transport budget the plan already fixed for P1a-U2.
Anyone may tighten a number here before Phase 5 consumes it; changing one afterwards means saying
which recorded figure it invalidates.
