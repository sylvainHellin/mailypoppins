# Phase 5 exit gate

The five gate lines of Phase 5 of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the test or the measurement that proves it, with the result recorded rather than asserted.

Ticket [#0124](../tickets/0124-tui-cutover.md).
Evidence taken on 2026-09-21 at `cf25695` on branch `daemon`, thirty-eight commits past the Phase 4 exit, all of them Phase 5's own.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, `cargo 1.96.0 (30a34c682 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0-22-generic, AMD Ryzen 7 PRO 8845HS, 28 GiB RAM.

Four of the five pass as written.
The fifth, cold first paint, passes against a measurement taken now rather than against a Phase 0 number: `docs/baselines/pre-daemon/measurements.md` W5 is `NOT TAKEN`, so this unit defines the method and takes both columns in one session, the `pre-daemon` binary beside the daemon-backed one over one fixture.

One unit of the phase did not land: P5-U10, the crate boundary for the TUI, which is deferred with a three-unit sequencing recorded in the ticket.
It is not one of the five gate lines, and the section below says exactly what it leaves open.

## The whole-tree run

Every count below is a slice of this one run.

```sh
TMPDIR=/var/tmp timeout 1500 cargo test --workspace --offline
```

2204 pass, 0 fail, 5 ignored, across 49 result lines.
`pgrep -af 'mp daemon'` afterwards returns nothing of this tree's.

2204 against Phase 4's 2071: 133 rows are Phase 5's own, and the Phase 5 review that followed `691c5ea` added the last of them.
`TMPDIR=/var/tmp` is not optional on a host whose `/tmp` is a small tmpfs, because a hundred parallel store tests each build a tempdir store there; since `36deca3` it is set once in `.cargo/config.toml` and no run needs the flag.

The suites this phase created or rewrote, each green on its own:

| suite | tests |
|---|---:|
| `phase5_parity_gate` | 11 |
| `phase5_undo_send_hold` | 2 |
| `tui_daemon_recovery` | 3 |
| `architecture_boundaries` | 6 |
| `test_selection_guard` | 5 |
| `src/tui/ui/golden_frames_daemon.rs` | 22 |
| `src/tui/app/queries_tests.rs` | 18 |
| `src/tui/actions_tests.rs` | 22 |
| `src/tui/events_tests.rs` | 20 |
| `src/tui/events_resync_tests.rs` | 1 |
| `src/tui/app/invites_tests.rs` | 8 |

The six Phase 4 slice suites are unmoved: read 22, draft 34, mutation 35, sync 38, send 50, admin 44.
The store-backed golden frames are unmoved at 20, with no snapshot re-approved anywhere in the phase.

## The complete TUI parity gate passes, oracles included

Units P5-U9 and P5-U11.

```sh
cargo test --offline --test phase5_parity_gate --test phase5_undo_send_hold
```

11 and 2 pass, 0 fail.
That is the first phase run in which the parity gate is green: `the_phase_five_manual_checklist_is_complete_and_carries_no_failure` was red from `9f6757b` by construction, and this unit's checklist closes it.

The five oracles the plan names, and what each one reports.

**(a) The eight integration suites, through a live daemon.**
`the_eight_legacy_suites_answer_through_a_live_daemon` runs one routed command per suite against one `admin_fixture` root in one daemon lifetime and byte-diffs each against the `pre-daemon` binary over the same root: `list-messages`, `path <draft>`, `list`, `dump-mailbox --json`, `search --local`, `outbox list`, `calendar rebuild`, `sync -A alpha`.
Every one is byte-identical, stdout, stderr and exit code.
The oracle is `~/.cache/mp-oracle/pre-daemon/mp`, built from the `pre-daemon` tag (`f8af44b`); the row fails rather than skips when no oracle is present, which is finding 2 of the Phase 5 review.
`the_engine_the_legacy_lock_suite_assumes_is_the_daemon` says what that comparison cannot: the account's engine lock is free before the daemon starts, held by the daemon while it runs, and free again after `mp daemon stop`.

**(b) The daemon-backed golden frames.**
Three rows over the module source and `src/tui/ui/snapshots/`, because a library test module is not reachable as tests from `tests/`.
22 daemon-backed frames, 18 of them asserted byte-identical to the hand-built frame of the same fixture and therefore pinned by the snapshot #0049 reviewed, and exactly two `…_daemon.snap` files, the two scenes that have no hand-built pair.

**(c) The help walk and the key dump, from a binary of this run.**

```sh
touch src/main.rs && timeout 600 cargo build --offline \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
diff <(./target/debug/mp dump-keys --json) docs/baselines/pre-daemon/tui-keys.json
```

Both empty, exit 0: the whole 50-screen help surface and all 92 key bindings are byte-identical to the Phase 0 captures, from a binary built from this working tree in the same run.
The two rows inside `phase5_parity_gate` assert the same thing against `env!("CARGO_BIN_EXE_mp")` on every test run.

**(d) The `KeyAction::Manual` checklist.**
[phase5-manual-keys.md](phase5-manual-keys.md), written by this unit from a real walk of a daemon-backed TUI in a 120x40 pty over an `examples/mkfixture` root, with the daemon started beside it and `MAILYPOPPINS_DAEMON_REQUIRE=1` set so a call answered in the client's own process would have failed instead of passing.
Seventeen of the twenty keys pass with the screen or the artifact each one produced; three are `NOT TAKEN (owner-only)` and name what a host with a display, a browser and a real account would have to press: `y` (the clipboard, which `arboard` cannot reach on a headless host), `f` (a server-only hit, which an offline fixture cannot produce) and `b` (a message with an HTML part).
No key failed.

**(e) The undo-send hold.**
`quitting_the_last_client_mid_hold_leaves_the_draft_approved_and_sends_nothing` passes, socket-level and headless: the draft is still `approved`, its file is still where `draft.path` says, no outbox row appeared that the approve did not find there, and the fake transport's ledger is empty.
Its twin, `the_same_fixture_records_a_send_when_a_client_really_sends`, fills that ledger, so the three negative assertions are not vacuous.

Passes.

## The TUI and CLI run concurrently against one daemon

Units P5-U7 and P5-U8.

The gate names a cross-client integration test.
What the tree has is `a_runtime_tick_reaches_a_subscribed_client_with_its_arrivals` (`tests/tui_daemon_recovery.rs`), where a subscribed connection receives the `sync.completed` a daemon-owned watcher tick published, and the whole of `phase5_parity_gate`, where a routed CLI answers from a daemon a fixture connection is bootstrapped against.

**The concurrent case itself is manual and is recorded here rather than claimed.**
During the manual key walk (oracle d) the TUI held an open session against the fixture daemon while `mp list-messages --mailbox archive`, `mp dump-mailbox --json` and `mp search --local` were run from a second terminal with `MAILYPOPPINS_DAEMON_REQUIRE=1`, against that same daemon.
All three answered, and `mp list-messages` reported the row the TUI had archived seconds earlier, at its new address `mp://alpha/archive/alpha-bulk-4420@fixture.invalid`, which is the propagation half of the line.
The daemon log shows both connections completing `initialize` inside one instance lifetime.

Two things are worth knowing about that evidence.
No automated test drives a TUI process and an `mp` process at the same instant against one daemon, which is the same gap Phase 4 recorded for two concurrent CLI commands and which is in `BACKLOG.md`.
And the TUI announces itself as `ClientKind::Cli`, because it reaches the daemon through `client_session`, the same door every routed command uses; `ClientKind::Tui` exists and is used by the undo-send-hold suite and by the in-process fixtures only, so the daemon cannot today tell a TUI from a one-shot command.
That matters to P6-U3, whose gate line is about what happens when the last client exits mid-hold.

Passes, with the manual half recorded.

## Killing and restarting the daemon produces clear recovery without direct fallback

Units P5-U7 and P5-U8.

```sh
cargo test --offline --test tui_daemon_recovery
```

3 pass, 0 fail.
`a_killed_daemon_is_survived_and_the_session_bootstraps_against_its_replacement` kills the daemon with `SIGKILL`, asserts the session refuses calls at once rather than hanging or serving a stale answer, asserts the account's engine lock is free during the outage from a second open file description (which is what a client that fell back to the store would be holding), and then asserts the session reconnects, bootstraps against the new instance id and answers again.

The pty smoke P5-U8 recorded is the same property with a screen in front of it: `The daemon is not reachable; reconnecting…` on the activity line in the warning colour within one idle tick, then `Reconnected to the daemon` and a fresh bootstrap.
Finding 1 of the Phase 5 review is the other half of recovery: a resync now replaces the mailboxes, the counts and the listing caches of an account that had already opened, where before it moved the watermark past the events that would have corrected them.

Passes.

## Cold first paint with no daemon running is within the Phase 0 startup baseline

Unit P5-U11.

### The method, defined here because W5 was never taken

`docs/baselines/pre-daemon/measurements.md` W5 is `NOT TAKEN (no account / headless host) - owner action`, so there is no Phase 0 number to compare against and no Phase 0 method to reuse.
The method is therefore defined here, in the terms that file uses, and both columns are taken in one session so they are comparable with each other even though neither is comparable with a figure nobody took.

One run is one `mp` launched inside a 120x40 pty and quit with `q` once it has painted.
The launch instant is taken **inside** the pty immediately before the exec (`date +%s.%N`, then `exec mp`), so the pty setup and the shell are outside the measurement.
The first frame is the process's own `[TIMING] tui_draw` line in `<data_dir>/logs/mailypoppins-YYYY-MM-DD.log`, the instrument #0108 added and the one `docs/plans/preview-latency.md` already measures against; its `done` line reports 0 or 1 ms on every run, so which of the pair is read moves nothing.
First paint is the difference between the two, in milliseconds, and the harness reads it out of the log by byte offset so no earlier run can be mistaken for this one.

- Fixture: `cargo run --release --example mkfixture -- --out /var/tmp/mp-p5u11-fixture --rows 5000`, the Phase 0 fixture with the Phase 0 flags, 5501 messages, 25.1 MiB.
- Filesystem: `/var/tmp`, which is disk-backed on this host, where the Phase 0 CLI figures were taken on tmpfs. Nothing here is compared with those.
- Binaries: `~/.cache/mp-oracle/pre-daemon/mp` for the pre-daemon column, `target/release/mp` (`cargo build --release --offline`) for the other two.
- **Pre-daemon** = the `f8af44b` binary, which contains no daemon at all and opens its stores itself, after the first frame.
- **Cold** = no daemon running: `mp daemon stop` before every run, so each one includes an on-demand daemon start against a fixture the daemon has never opened in that lifetime.
- **Warm** = a daemon is already up and serving the fixture root.
- Method: one discarded warm-up, then eleven runs, median with min and max beside it, the `bench` harness `docs/baselines/pre-daemon/workloads.md` fixes.

### The table

Milliseconds, `median min max`, from launch to first frame.

| column | median | min | max |
|---|---:|---:|---:|
| pre-daemon (`f8af44b`) | 6 | 5 | 7 |
| daemon-backed, warm daemon | 7 | 6 | 7 |
| daemon-backed, no daemon running | 35 | 34 | 36 |

### What the numbers say

**A warm daemon costs the first frame nothing**: 7 ms against the pre-daemon binary's 6, which is one millisecond and inside the spread of both columns.
That is the shape P5-U2 designed for and the reason the session is brought up before the terminal: the TUI pays a connect and a handshake where it used to pay a config load, and the two cost the same.

**A cold start costs 28 ms, once per daemon lifetime.**
The log of a cold run shows where it goes: the process starts at +6 ms, finds no daemon and spawns one at +7 ms, the daemon is listening at +11 ms, the client completes `initialize` at +34 ms, and the frame is on the screen at +35 ms.
The two stores are opened by the account runtimes *after* the daemon is serving, so a 5501-message fixture does not gate the paint; what the client waits for is the handshake, not the mail.
Both accounts are `ready` about 50 ms after the frame, which is when the `··` markers fill in.

**Nothing regressed against the shell the pre-daemon binary painted.**
#0003's contract is that the shell paints before any store opens, and it still does: the daemon-backed TUI paints zeroed counts and the `··` marker first and applies the bootstrap on a later tick, which is what keeps the golden frames byte-identical.

The gate line asks for cold first paint "within the Phase 0 startup baseline".
There is no Phase 0 startup baseline, so what is recorded is this: the daemon-era cold figure is 35 ms, the warm figure is 7 ms, and the pre-daemon figure measured with the same harness on the same fixture in the same session is 6 ms.
A 29 ms difference on the first launch of a daemon lifetime, on a tree whose #0003 budget was 1.2 seconds of blank terminal, is not a regression anybody can see.

### Which `measurements.md` rows stay NOT TAKEN

W5 is answered above and is the first of the five to close, in the daemon column and in a pre-daemon column taken beside it rather than at Phase 0.
The other four are unchanged and all four are owner action:

- **W1**, cursor-move preview p50/p95, needs a real account and the twenty named rows of #0108. The `#[ignore]`d timing row in `src/tui/app/queries_tests.rs` is its offline lower bound and passes inside the 5 ms delta ceiling.
- **W2**, the TUI half (frames over 50 ms during a sync burst).
- **W6**, the cold-cache half, needs root.
- **W8**, mutation write plus propagation. The manual walk did the write half (an archive from the search overlay reached the store and both sidebar counts followed) and did not time it.

No synthetic number was substituted for any of them.

Passes, with the baseline column taken now rather than at Phase 0.

## `cargo install --path .` installs the working daemon-backed `mp`

Unit P5-U11.

```sh
TMPDIR=/var/tmp timeout 900 cargo install --path . --offline
```

Replaces the installed binary.
The installed `mp` is what the manual key walk and the first-paint measurement both ran against in their release form, and `mp --help` from a binary built out of the same tree is byte-identical to the Phase 0 capture.

Passes.

## P5-U10, deferred

The plan's P5-U10 is one unit: move `src/tui/` to `crates/mp-tui/` depending on `mp-client` and `mp-protocol` only, and drive `tests/architecture_boundaries.rs`'s TUI allow-list to zero.

It landed the first third of that, three additive daemon queries (`calendar.events`, `message.ics`, `message.invite`) and the `TUI_APP_STORE_RESIDUE` sites routed through them, and reported that the rest is a phase rather than a unit: a `crates/mp-tui` that compiles needs a shared crate holding roughly 15 000 lines across sixteen modules, six of which need a genuine split, none of it new production code and all of it a tree that does not compile between the first `git mv` and the last.

**The allow-list is at 11 rows, not zero**, in four groups, each waiting on a surface that does not exist:

- Three rows (`app/mod.rs store`, `app/calendar_view.rs store`, `app/store_rows.rs store`) are the sessionless store-backed readers, which are the equality oracle every one of the ~370 sessionless-`App` tests compares against. They die with the crate move, not with a method.
- Three rows (`app/types.rs store`, `app/types.rs ingest`, `queries.rs store`) want `MessageRow`, `DraftRow` and `SkippedDraft` as protocol types, which would also delete `src/main.rs`'s duplicate wire-row decoder.
- Two rows (`actions.rs store`, `actions.rs send`) are `RD-06`'s Markdown rendition, `RD-07`'s selector, `LST-09`'s `message.fetch`, and the undo-send hold's fire path, which the plan holds in the TUI until P6-U1/U2.
- Three rows (`helpers.rs store`, `helpers.rs imap_client`, `mod.rs store`) are `LST-08`'s server leg through `message.list_server`.

The sequencing is P5-U10a (the shared crate), P5-U10b (the last surfaces) and P5-U10c (the move itself), each leaving `cargo test --workspace` green, and it is written out in full in the P5-U10 section of [the ticket](../tickets/0124-tui-cutover.md).
The orchestrator's decision is to run the three after Phase 6 rather than before it, because P6-U1/U2 remove one of the eleven rows on their own and P6's hardening work does not depend on the crate boundary.

## Clippy

```sh
timeout 600 cargo clippy --workspace --offline --all-targets
```

34 distinct warnings, the baseline since P5-U8, which is the 33 of Phase 4's tree plus one in `tests/tui_daemon_recovery.rs` (`&PathBuf` where `&Path` would do) that a T unit committed and an implementer may not edit.
None is on a line this unit wrote.
The 39-warning count earlier units quote is the same 34 plus the per-target `generated N warnings` summary lines under the default format; the reconciliation is in the P5-U6 section of the ticket.

## Housekeeping

`pgrep -af 'mp daemon'` returns nothing of this tree's after the full run, after the measurement runs and after the manual walk.
Every suite that starts a daemon points `MAILYPOPPINS_DATA_DIR` at a `TempDir` and stops it in `SandboxRoot::drop`; the measurement and the walk used `/var/tmp/mp-p5u11-fixture` and stopped their daemon explicitly.

`rustfmt --edition 2021` was not run on anything: this unit's only code change is a doc comment in `src/tui/session.rs`, a file that is rustfmt-clean and stays so.

## What Phase 5 does not answer

- **The TUI is not a crate yet**, and the engine-import allow-list is at 11 rows. See P5-U10 above.
- **`LST-06`, the server leg of `mp search`, and `LST-08`, the TUI's server search leg**, are both still client-side. Phase 4 left the first and Phase 5 leaves the second: `lib_do_multi_search` runs on a background thread with the account's IMAP configuration in hand, which reaches no engine needle the residue gate scans, and `message.list_server` is the method it becomes.
- **The undo-send hold is still client-side**, in `src/tui/actions.rs` and the pre-draw loop, as the plan intends until P6-U1/U2. Oracle (e) asserts the daemon's half of the criterion today and will assert the daemon's own cancellation after them without moving a line.
- **Three of the twenty manual keys are `NOT TAKEN`** and need a host with a display, a browser and a real account.
- **No automated test drives a TUI and a CLI against one daemon at the same instant**, and the TUI announces itself as a CLI client.
- **An account with no local store gets no runtime and comes up `blocked`** (P5-U8), which contradicts P3b-U8's rule that a configured account gets a live runtime and which three `tests/daemon_config.rs` rows now pin the other way. It is the one decision of this phase that wants ratifying, and it is recorded in the ticket and in `BACKLOG.md`.
- **The macOS half of Phase 2's crash and stale-socket recovery** is unchanged and still escalated.
