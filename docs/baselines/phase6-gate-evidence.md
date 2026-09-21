# Phase 6 exit gate

The four gate lines of Phase 6 of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the test, the smoke run or the measurement that proves it, with the result recorded rather than asserted.

Ticket [#0125](../tickets/0125-daemon-hardening.md).
Evidence taken on 2026-09-21 at `1ee6bcb` on branch `daemon`, thirty-one commits past the Phase 5 exit, all of them Phase 6's own.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, `cargo 1.96.0 (30a34c682 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 16 threads, 28 GiB RAM.

Three of the four pass as written.
The third, "lifecycle commands and service-manager modes pass platform smoke tests", passes on this host's systemd half and is **escalated** for launchd, exactly as the plan's own line anticipated.

The benchmark rerun the unit owes is the second half of this file, and it is the first one taken against the complete dispatcher.

## The whole-tree run

Every count below is a slice of this one run.

```sh
TMPDIR=/var/tmp timeout 1500 cargo test --workspace --offline
```

2343 pass, 0 fail, 5 ignored.
`pgrep -af '[m]p daemon'` afterwards returns nothing of this tree's.

2343 against Phase 5's 2204: 139 rows are Phase 6's own.
The suites this phase created, each green on its own:

| suite | tests |
|---|---:|
| `daemon_send_hold` | 7 |
| `daemon_shutdown` | 12 |
| `daemon_service` | 23 |
| `daemon_diagnostics` | 36 |
| `daemon_soak` | 7 |
| `src/tui/hold_tests.rs` | 16 |
| `src/tui/diagnostics_tests.rs` | 9 |

`phase5_parity_gate` is unmoved at 11 and `phase5_undo_send_hold` at 2, which is the rerun the fourth gate line asks for.

## The daemon can run unattended across client churn

Unit P6-U9.

```sh
TMPDIR=/var/tmp cargo test --offline --test daemon_soak -- --nocapture
```

7 passed, three runs at 15.5 s, 15.0 s and 15.0 s wall, and 7 passed at `MAILYPOPPINS_SOAK_SECS=60` in 65.6 s.
Row (a) is 200 connect/bootstrap/disconnect cycles from 8 threads and row (g) runs everything at once for the configured duration, ending in a `daemon.stop` that answers `clean: true`.
The measured descriptor and memory numbers, the ceilings they justify and the host they were taken on are in [phase6-soak.md](phase6-soak.md); they are not repeated here, because a number quoted twice is a number that drifts.

What the rows pin for this line: descriptors return to a warm baseline (+3 at 200 cycles, +4 at 1200, against an `FD_MARGIN` of 12), `diagnostic.health.clients` returns to 1, and the mixed row's 14 667 churn cycles over 60 seconds leave the daemon answering and stopping cleanly.

Passes.

## Memory remains bounded under slow clients and repeated syncs

Unit P6-U9, rows (b), (c) and (g).

Row (b) runs 50 syncs against 4 readers and one client that never reads, under a 4000-event burst per bootstrap; row (c) runs 200 `sync.quick` passes; row (g) mixes everything.
RSS growth is bounded by `RSS_CEILING_BYTES` (192 MiB) and the worst measured growth is 76.7 MiB.

**This is the line that found the one real leak of the phase.**
`OperationRegistry` never forgot a settled operation, so memory grew with the work rather than with the working set: 0.8 KiB per settled operation, +61.9 MiB at 10 s of the mixed row, +77.9 at 60 s, +86.1 at 180 s.
The fix is `operations::HISTORY = 256`, a `start` that forgets the oldest settled entries past that window, and live operations never forgotten.
Afterwards the mixed row's growth stops tracking duration (+61.1 MiB at 10 s, +62.8 at 60 s, +83.7 at 180 s while doing 4.4x the work of the pre-fix 180 s run), and the daemon is much faster under sustained load because `live()` and every disconnect walked that table: 113 holds a second became 767.

The numbers, their scales and the ceiling arithmetic are in [phase6-soak.md](phase6-soak.md).

Passes, with the leak found, fixed and pinned by a unit test in `src/daemon/operations.rs`.

## Lifecycle commands and service-manager modes pass platform smoke tests

Units P6-U5 and P6-U6.

```sh
TMPDIR=/var/tmp cargo test --offline --test daemon_service
```

23 passed, 0 failed.
Twenty of the rows write into a `TempDir` under `MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN=1` and reach no service manager at all; three run with the dry run off and `PATH` pointing at a sandbox holding a fake `systemctl` that records its argv and exits with a chosen code, which is what pins *which* commands run and in what order.

### The systemd smoke, by hand

Run at `1ee6bcb` from the release binary, in a sandbox `HOME`, `XDG_CONFIG_HOME`, `MAILYPOPPINS_CONFIG_DIR` and `MAILYPOPPINS_DATA_DIR` under `/var/tmp/mp-p6u10-service`, with the dry run armed so no user unit of this machine was touched:

| command | first line | exit |
|---|---|---|
| `mp daemon install-service` | `✓ wrote …/systemd/user/mailypoppins.service` | 0 |
| `mp daemon install-service` again | `✓ … is already installed` | 0 |
| `mp daemon install-service --check` | `✓ service installed at …` | 0 |
| `mp daemon uninstall-service` | `✓ removed …` | 0 |
| `mp daemon install-service --check` after it | `✗ no service installed` | 1 |
| the same four with `MAILYPOPPINS_DAEMON_SERVICE_OS=darwin` | `✓ wrote …/LaunchAgents/dev.mailypoppins.daemon.plist`, `✓ removed …` | 0 |

Each block carried its two `systemctl --user` lines (or its one `launchctl` line) and the `dry run: … nothing was run` line under them.
The written unit names `ExecStart="<abs>/mp" daemon run`, the two directory variables the installing `mp` resolved as double-quoted `Environment=` assignments, `Restart=on-failure`, `KillSignal=SIGTERM` and `TimeoutStopSec=15`, which is `DEFAULT_GRACE_SECS + 5`.

The lifecycle commands were smoked over the benchmark fixture in the same session: `mp daemon start`, `status` (running, with both directories and both accounts), `health` (`✓ daemon healthy, 1 check needs attention`, exit 0, one `⚠` for the account still opening its store), `logs --lines 3 --level info` (three lines, no banner), `support-bundle` (`files: 5`), `restart`, and `stop --grace-secs 3` -> `✓ daemon stopped`, exit 0, with the runtime directory left holding only `daemon.start.lock`.

### launchd is ESCALATED and NOT TAKEN

`launchctl` and `plutil` are both absent from this host, so `MAILYPOPPINS_DAEMON_SERVICE_OS=darwin` proves the bytes of the plist against the committed fixture and nothing else.
The live check is owner action on the Mac, as plan risk 8 anticipated: `mp daemon install-service`, log out and back in, `mp daemon status` reporting a running daemon, then `mp daemon uninstall-service`.
Two questions ride on it and are written up in `docs/daemon-operations.md`: whether `bootstrap` refuses a label it has already loaded, and what `current_exe()` resolves to under a Homebrew `mp`.

Passes on Linux; the macOS half is escalated, not claimed.

## The daemon-owned hold reproduces the behaviour the parity gate recorded, and the last client exiting mid-hold cancels the hold and leaves the draft approved

Units P6-U1 to P6-U4.

The plan's proof column names `tests/daemon_hold.rs`.
The file is **`tests/daemon_send_hold.rs`**, the name the send family's other socket-level files already use (`daemon_send_slice.rs`, `daemon_send_attachments.rs`); the rename is recorded in the P6-U1 section of the ticket and is the only deviation from the gate's own wording.

```sh
TMPDIR=/var/tmp cargo test --offline --test daemon_send_hold --test phase5_undo_send_hold
TMPDIR=/var/tmp cargo test --offline --lib hold_tests
```

7, 2 and 16 passed, 0 failed.

The seven socket rows are what "reproduces the behaviour the parity gate recorded" means over a wire: a hold nobody cancels fires and retires the draft, the window that did not send can cancel it, the sender may close its window while another client watches and the hold still fires, a zero window sends at once and publishes no `send.hold_*` event, `mp send-approved` bypasses the hold against the clock, and a client that joins mid-countdown finds the hold in its own `state.bootstrap`.
The sixteen in-process rows are the presentation half: `Sending in {n}s (press u to undo)`, `Sending...` and `Send cancelled; the draft is untouched` are asserted as literals, so the move out of `src/tui/actions.rs` could not reword them.

**The second half of the line is `tests/phase5_undo_send_hold.rs`, rerun unchanged.**
`quitting_the_last_client_mid_hold_leaves_the_draft_approved_and_sends_nothing` passed before the hold moved and passes after it, and for a different reason: the answer used to be "it never had a hold to fire" and is now "it had one and the daemon cancelled it", which is `HoldScheduler::cancel_all` running in `handle_connection` once `subscriber_count()` reaches zero.
Its twin `the_same_fixture_records_a_send_when_a_client_really_sends` fills the fake transport's ledger, so the negative assertions are not vacuous.
Not one line of that file moved in this phase, which is the evidence that the criterion is the same criterion.

`tests/daemon_shutdown.rs` (12) is the third path to the same state: a shutdown cancels every armed hold at step 2, before it answers the stop, so a hold never appears in `pending` or in `unsettled`.

Passes.

## Benchmarks: the Phase 1a workloads against the complete dispatcher

Unit P6-U10, the plan's "re-run the Phase 1a workloads against the complete dispatcher".

### Method

The same harness Phase 0 fixed and Phase 4 reused, unchanged, so the numbers sit in the same column as the earlier ones: the three-line `bench` helper of [pre-daemon/workloads.md](pre-daemon/workloads.md), one discarded warm-up, then eleven runs, median with min and max beside it.
`hyperfine` is still not installed on this host and was still not installed for this.

- Fixture: `target/release/examples/mkfixture --out /var/tmp/mp-p6u10-fixture --rows 5000`, the Phase 0 fixture with the Phase 0 flags, 5501 messages, 25.1 MiB of store and blobs.
- Filesystem: **ext4 on `/dev/nvme0n1p2`**, the root filesystem, because `/var/tmp` is where a daemon-era measurement belongs on this host. Phase 4's table was taken on **tmpfs** (`/tmp`), so the two are not the same column; the oracle figures below are 4 to 9 ms above Phase 4's own oracle figures and that gap is the filesystem.
- Binaries: `~/.cache/mp-oracle/pre-daemon/mp` (the `pre-daemon` tag, `f8af44b`) for the oracle column, `target/release/mp` built from `1ee6bcb` with `cargo build --release --offline` for the other two, all three in one session.
- **Warm** = a daemon is already up and serving the fixture root; the command is one round trip.
- **Cold** = `mp daemon stop` before every one of the eleven runs, so each includes an on-demand daemon start against a fixture the daemon has never opened in that lifetime.
- Sandbox: `HOME`, `MAILYPOPPINS_CONFIG_DIR` and `MAILYPOPPINS_DATA_DIR` all inside the fixture root, and every daemon the run started was stopped at the end of it. The one `mp daemon` in this host's process list throughout is the owner's own against `~/.local/share/mailypoppins`, which no sandbox here could reach.
- No account is configured on the fixture, so both binaries print the SMTP-secret warning on stderr for every run; stdout and stderr are discarded and do not enter the timing.

Every workload that produces output was diffed against the oracle over the same root before it was timed: nine of the ten, stdout, stderr and exit code, **byte-identical**, `mp search --local 'body:zolvertrix'` included at its 20 hits.

### The table

Milliseconds, `median min max`.
The last column is Phase 4's warm figure at `db2d67d` on tmpfs, for the comparison the unit owes; it is a different filesystem and is read as a direction rather than as a delta.

| workload | oracle (pre-daemon) | warm | cold | P4 warm |
|---|---|---|---|---|
| `mp --version` (floor) | 8 7 9 | 8 7 9 | 9 8 9 | 8 |
| `mp list-messages --mailbox inbox -n 20` (W6) | 45 44 47 | **11** 10 12 | 76 75 78 | 10 |
| `mp list-messages --mailbox Bulk -n 5000` (W2) | 55 54 57 | 94 92 97 | 177 177 181 | 56 |
| `mp show <ordinary body>` (W4) | 46 44 50 | **11** 10 12 | 76 75 79 | 11 |
| `mp show <10 MiB body>` (W4) | 80 77 85 | 68 65 82 | 151 149 156 | 73 |
| `mp show --json <10 MiB body>` (W4) | 76 75 79 | 72 61 76 | 144 141 151 | 68 |
| `mp dump-mailbox --json -A alpha` (W3) | 91 89 92 | 116 112 119 | 204 198 210 | 121 |
| `mp search --local 'body:zolvertrix' -n 100` (W7) | 46 45 48 | **11** 10 12 | 76 75 79 | 10 |
| `mp search --local 'body:concrete' -n 100` (W7) | 50 48 51 | **17** 15 18 | 81 80 83 | 15 |
| `mp list` (drafts) | 45 42 48 | **9** 9 10 | 38 37 39 | 9 |

### What the numbers say

**Every small answer is where Phase 4 left it, and the acceptance rules hold.**
A one-shot query that read the store in process costs 45 to 50 ms and costs 9 to 17 ms over a warm socket, which is W6's "at most 25 ms above the pre-daemon figure" satisfied by 34 ms in the other direction, and W7's 1.2x satisfied four times over with an identical hit count.
W4 is 0.85x and 0.95x against a 1.5x ceiling, and no `frame_too_large` appeared on any path: a 10 MiB body travels as a handle, which is `docs/baselines/decisions/large-payload.md` working.

**A cold command costs one daemon start, and that start is about 65 ms**: each small query's cold figure is its warm figure plus 64 to 65 ms, and the three multi-MiB answers add 72 to 88, which is the start plus the two stores it opens under a bigger first read. W6 allows 1 s for an invocation that has to start a daemon. `mp --version` is on the no-daemon list and starts nothing, so its three columns are one number; `mp list` is the other exception, its cold figure (38 ms) *below* its oracle figure, because a drafts listing needs no message store and the daemon it starts opens none either.

**W3 is still the one workload that pays more than it saves, and it improved.**
`mp dump-mailbox --json -A alpha` is 116 ms routed against 91 in process, 1.27x, where Phase 4 measured 121 against 86, 1.41x.
W3's acceptance rule is 1.2x, so the rule is still breached, by less; the output is byte-identical, which is the other half of that rule.
It is a batch export rather than an interaction, and streaming or a handle-backed NDJSON answer is the fix, already in `BACKLOG.md` since Phase 4.

**W2 is the one regression of the two phases since, and it is the row shape rather than the transport.**
`mp list-messages --mailbox Bulk -n 5000` is 94 ms warm here against Phase 4's 56 ms, while its oracle moved by 4 ms and every other row moved by less than that.
Measured by row count against the same daemon in the same session: 500 rows 21 ms, 1000 rows 28 ms, 2000 rows 48 ms, 5000 rows 94 ms, which is about 17 µs per row over an 11 ms floor where Phase 4's figures give about 9 µs.
The cause is in the tree rather than in the socket: `message.list`'s `list` projection carried nine keys per row at `db2d67d` and carries fifteen now (`id`, `to`, `cc`, `reply_to`, `bcc`, `is_invite` and the `flagged` flag, all added in Phase 5 when the TUI started reading its list rows over the wire), so a 5000-row answer is a bigger answer to build, serialise, ship and decode.
W2's acceptance rule for the CLI lower bound is 2x the pre-daemon figure and 1.71x passes it, so this is recorded rather than escalated, and the row cost is what a paging decision would be reopened against if the rule is ever tightened.

### Cold first paint, re-taken

The P5-U11 harness (`/var/tmp/mp-p5u11-firstpaint.sh`, one `mp` in a 120x40 pty, the launch instant taken inside the pty immediately before the exec and the first frame read out of the process's own `[TIMING] tui_draw` line) is still on this host and was rerun at `1ee6bcb` over the P5-U11 fixture, eleven runs per column after one discarded warm-up.

| column | median | min | max | P5-U11 |
|---|---:|---:|---:|---:|
| pre-daemon (`f8af44b`) | 7 | 5 | 8 | 6 |
| daemon-backed, warm daemon | 7 | 6 | 8 | 7 |
| daemon-backed, no daemon running | 35 | 34 | 36 | 35 |

Unchanged by the whole of Phase 6, to the millisecond on the two columns that matter: the hold, the shutdown sequence, the diagnostics and the bounded registry cost the first frame nothing.
The pre-daemon column's median moved by one millisecond inside a spread it shares with the warm column, which is noise rather than a change in a binary that has not been rebuilt since Phase 0.

### The preview p95, the W1 lower bound

```sh
TMPDIR=/var/tmp cargo test --offline --lib queries_tests -- --ignored
```

1 passed: `the_preview_query_stays_inside_the_p95_delta_ceiling`, over a 200-row fixture walked down and back up through both the daemon-backed and the store-backed preview read.
The row asserts that the daemon-backed p95 exceeds the store-backed p95 by at most `PREVIEW_P95_DELTA_CEILING_MS` (5 ms, the transport budget the plan fixed for P1a-U2), and it holds at this HEAD.
It prints no numbers, so what is recorded is the verdict rather than a figure; it is a lower bound on W1 in any case, since it prices the dispatcher in one process and not the socket, the framing or the session thread's hop.

**W1 itself stays `NOT TAKEN`.** It needs a real terminal, a real account and the twenty named rows of #0108, which is owner action and unchanged since Phase 0.

### Which `measurements.md` rows stay NOT TAKEN

Four, all owner action, unchanged by this phase:

- **W1**, cursor-move preview p50/p95 - needs a terminal and an account. The offline lower bound above is green.
- **W2**, the TUI half (frames over 50 ms during a sync burst). The CLI lower bound is in the table and has a routed column.
- **W6**, the cold-cache half - needs root (`sysctl -w vm.drop_caches=3`) and a disk-backed data directory. The *cold-daemon* half is taken above and is a different quantity.
- **W8**, mutation write plus propagation.

W5 closed at P5-U11 and is re-taken above.
No synthetic number was substituted for any of the four.

## Clippy

```sh
timeout 600 cargo clippy --workspace --offline --all-targets
```

38 distinct warnings, the count every unit of this phase reported since `7c96613`, none of them on a line P6-U10 wrote (this unit writes no code).
The 45-line count under the default format is the same 38 plus the per-target `generated N warnings` summaries.

## Housekeeping

`pgrep -af '[m]p daemon'` after every test run, every benchmark pass, the first-paint runs and both smoke runs returns exactly one line, the owner's own daemon (pid 3667325) against `~/.local/share/mailypoppins`, which was never touched: every benchmark daemon ran under a sandbox `HOME` and data directory inside `/var/tmp` and was stopped explicitly at the end of its pass.

The help walk and the key dump, from a binary built out of this working tree in this session:

```sh
touch src/main.rs && timeout 600 cargo build --offline \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
diff <(./target/debug/mp dump-keys --json) docs/baselines/pre-daemon/tui-keys.json
```

Both empty, exit 0.
Phase 6 added five subcommands (`install-service`, `uninstall-service`, `health`, `logs`, `support-bundle`) and one flag on `stop`, and moved neither capture, because the whole `daemon` subtree is hidden and the walk recurses only into the names clap lists under `Commands:`.

```sh
TMPDIR=/var/tmp timeout 900 cargo install --path . --offline
```

Installs the Phase 6 daemon-backed `mp`.

## The follow-ups the ten units left

Every open item the phase's ten unit reports named, and where it went.
Nothing on this list blocks the exit; the first three are the ones a Phase 7 unit is most likely to meet.

| follow-up | where it went |
|---|---|
| The operation registry never forgot a settled operation | **fixed** in P6-U9 (`operations::HISTORY = 256`), pinned by a unit test and written up in `docs/lessons-learned.md` |
| `snapshot.holds` was hard-coded to `[]`, so a client joining mid-countdown could not render the hold | **fixed** in P6-U4 (`CanonicalState::attach_holds`) |
| `mp daemon restart` takes no `--grace-secs`, so a restart cannot pass the grace `stop` accepts | `BACKLOG.md` |
| `mp daemon restart` prints the stop's line and says nothing about the start it performed | `BACKLOG.md` |
| `REPORT_DEADLINE` in `src/daemon/shutdown.rs` names a connection-close ceiling rather than a report deadline | `BACKLOG.md` |
| `Snapshot::holds` and `operations` stay `Vec<Value>` while the event and `send.hold_status` are typed | `BACKLOG.md` |
| `AccountView.health` is only ever `SyncHealth::default()`, so every snapshot reports `sync_health.state: "unknown"` | `BACKLOG.md` |
| The log reader stamps every line with `Local::now()`'s offset, so a line written across a DST change is an hour out | `BACKLOG.md` |
| `DAEMON_ENV_HOOKS` in `tests/support/parity.rs` clears twelve of the fifteen hooks | `BACKLOG.md` |
| The activity overlay's `/filter` matches the level's `Debug` spelling as a substring of the same query that matches the message | `BACKLOG.md` |
| No protocol fixture pins a `state.bootstrap` whose `snapshot.diagnostics` is non-empty | `BACKLOG.md` |
| Every `mp daemon` subcommand inherits the global `-s/--signature`, `--no-signature` and `-A/--account` flags, which mean nothing to a lifecycle command | `BACKLOG.md` |
| The live launchd check (install, log out and back in, `status`, uninstall) and the two questions on it | **owner action**, escalated above, in `docs/daemon-operations.md` and `docs/release-process.md` |
| A draft whose `from:` names an account other than the one whose drafts directory holds it is now refused by `send.draft` | **accepted**, recorded in the P6-U2 section of the ticket |
| The two service hooks are deliberately not in `DaemonFixture`'s clearing list | **accepted**, recorded in the P6-U6 section of the ticket and in `docs/daemon-operations.md` |
| The periodic tick with no client anywhere ("Phase 6's" in three documents) is not built | `BACKLOG.md`, retargeted off Phase 6 |
| `spikes/ipc-bench`, kept past the Phase 1a gate so Phase 6 could re-run its workloads | **still there**. This unit re-ran the Phase 0/1a *workloads* through the product binaries rather than through the spike, so the spike bought nothing here; its deletion is a `git rm` nobody has asked for yet and it stays a `BACKLOG.md` line |

## What Phase 6 does not answer

- **The macOS half of the service units** is written, fixture-pinned and never run. So is the macOS half of Phase 2's crash and stale-socket recovery line, unchanged and still escalated.
- **P5-U10a/b/c**, the crate boundary for the TUI, is still the next sequence and still deferred; P6-U2 removed one of its eleven allow-list rows by moving the hold, which is why the list is at ten.
- **A periodic scheduler** is still not built. The daemon's account runtime watches, and a tick with no client anywhere is the shape that would keep a store fresh; the plan put it in Phase 5/6 and neither built it.
- **W1, W2's TUI half, W6's cold-cache half and W8** are still owner action.
- **No automated test drives a TUI and a CLI against one daemon at the same instant**, nor two `mp` processes concurrently. Phase 4 and Phase 5 each recorded the gap and Phase 6 does not close it.
- **The hold has no golden frame.** The three status-line sentences are pinned as literals in `src/tui/hold_tests.rs`; no frame captures a countdown, deliberately, because a frame whose status line is a literal three tests already assert pins the same string twice.
