# Phase 3a exit gate

The four gate lines of Phase 3a of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the test or command that proves it, with the result recorded rather than asserted.

Ticket [#0121](../tickets/0121-daemon-dispatcher-and-state-model.md).
Evidence taken on 2026-09-09 at `a493229` on branch `daemon`, eight commits past the Phase 2 exit `b80f3bd`, all eight of them Phase 3a's own.
Re-taken after the review fixes commit that follows `fda9718`, which is where the counts below come from.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0.

All four pass as written.

## The two whole-tree runs

Every line below is a slice of one of these two runs, so they come first.

```sh
timeout 900 cargo test --workspace --features daemon --offline
timeout 600 cargo test --workspace --offline
```

1605 tests pass under the feature, 0 failed, 4 ignored (3 in the doc-tests, 1 for want of root in `daemon_runtime_paths`), across 30 result lines.
1355 pass without it, 0 failed, 3 ignored, across 20 result lines.

The 250 difference is the daemon's own: 199 in the ten gated `[[test]]` targets and 51 in the inline `#[cfg(test)]` modules inside `src/daemon/`, which is `cargo test --lib` at 1197 under the feature against 1146 without it.

Both figures moved by the review fixes and not by the phase: three unit tests in `src/daemon/state/mod.rs` (the merged drain's ordering, twice, and the RAII unsubscribe) and one in `src/store/read.rs` (the grouped unread count), the last of which counts in both runs.

The ten gated targets, Phase 3a's four in bold: `daemon_operations` **30**, `daemon_events` **29**, `daemon_framing` 29, `daemon_bootstrap` **28**, `daemon_dispatcher` **18**, `daemon_protocol_fixtures` 17, `daemon_runtime_paths` 17, `daemon_handshake` 12, `daemon_lifecycle` 10, `daemon_read_only_methods` 9.
Phase 2 left the featured figure at 1477 and Phase 3a adds 124: 105 in the four new suites, 17 inline in `src/daemon/`, and the 2 `mp-client` unit tests that count in both runs.

The plain figure is 1355 against Phase 2's 1352.
The additions are `mp-client`'s two `StateTracker` unit tests, which live in a new file (`crates/mp-client/src/state.rs`), and the review fixes' grouped-count test in `src/store/read.rs`.
No pre-existing test changed its assertions, which is the whole of the fourth gate line below.

## The four T-unit proofs

The gating convention of section 3.0 is that a T unit's test file compiles only once its I unit exists, and the T unit's report quotes the unresolved-import errors naming exactly the contract items.
Each proof was taken at its own commit, before the paired implementation, and is recorded in that commit's message.

| T unit | commit | `cargo test --workspace --features daemon` | plain suite |
|---|---|---|---|
| P3a-U1 `tests/daemon_dispatcher.rs` | `b6b07fe` | one `E0432` naming `mailypoppins::daemon::dispatch` and nothing else | 1352, unchanged |
| P3a-U3 `tests/daemon_bootstrap.rs` | `bcd55c4` | two `E0432` naming `mailypoppins::daemon::state` and the two `mp_client` items, plus one `E0599` for `Connection::next_notification`, and nothing else | 1352, unchanged |
| P3a-U5 `tests/daemon_events.rs` | `f01ace1` | one `E0432` naming `mailypoppins::daemon::state::events` | 1352, unchanged |
| P3a-U7 `tests/daemon_operations.rs` | `cf78730` | four errors naming only `CancelScope`, `daemon::operations`, `MethodSpec::new` and `MethodSpec.cancel_scope` | 1354, unchanged |

The one amendment to a pinned file is P3a-U7's own: `CancelScope` became the fourth field of `MethodSpec`, which broke four struct literals in `tests/daemon_dispatcher.rs`, and the mechanical fix was pre-approved in the P3a-U7 file header before P3a-U8 applied it.
`git show a493229 --stat -- tests/` shows those 8 changed lines in `daemon_dispatcher.rs` and no other test file touched, and `git diff --stat b80f3bd..HEAD -- tests/ src/tui/` shows the four new files and nothing else in the whole phase.

## Two clients observe ordered authoritative state

Units P3a-U3 to P3a-U6.

```sh
cargo test --workspace --features daemon --offline --test daemon_bootstrap --test daemon_events
```

Green: 28 in `daemon_bootstrap`, 29 in `daemon_events`, 57 total, 0 failed, in parallel and under `--test-threads=1` alike.

The lines the gate turns on:

- The five forced race boundaries of P1a-U6 are driven in process through a race hook rather than by racing two client processes, because Phase 3a registers no `Command` method and a race forced by sleeping two processes against each other is a flake rather than a test.
- `revisions are dense: N commits from revision 2 upwards` asserts the counter never skips under concurrent commits from two connections.
- `a_reading_client_receives_the_whole_burst_in_revision_order` is the wire half: a second connection reads every burst event, each exactly one above the last, from the instance that answered its bootstrap.
- `an_instance_change_is_checked_before_the_revision` and `a_revision_gap_requires_a_resync_and_poisons_the_stream` cover the client's side of ordering.

Passes.

## Event overflow produces bounded recovery

Units P3a-U5 and P3a-U6.

```sh
cargo test --workspace --features daemon --offline --test daemon_events
```

Green: 29 passed, 0 failed.

A client that bootstraps and never reads is sent twenty thousand changes: it receives a bounded number of events, then one `state.resync_required` carrying `event_queue_overflow` and the instance id that answered its bootstrap, and no domain event behind it.
The queue's byte cap is asserted on the queue itself rather than inferred from a resident-set reading, which is why bounded memory is pinned in process and `daemon.status` grew no per-connection byte count.
A second client stays responsive throughout, which is the "a slow client does not delay a fast one" half, and a re-bootstrap clears the poison and resumes the stream.

Passes.

## A bootstrap taken before any account is ready converges by event without a second bootstrap

Units P3a-U3 and P3a-U4.

```sh
cargo test --workspace --features daemon --offline --test daemon_bootstrap
```

Green: 28 passed, 0 failed.

Every account is `opening` at a Phase 3a bootstrap with zeroed counts and empty draft lists, and `MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS` flips them to `ready` after the first bootstrap rather than after startup, so no client can lose the race and no test sleeps to win it.
The readiness arrives as one `account.state_changed` event per account, in `config.toml` order, at revisions above the one the bootstrap reported, and the connection issues no second `state.bootstrap`.

Passes.

## Nothing in this half changes the behaviour of a client that never sets the debug flag

Unit P3a-U9, over the whole phase.

```sh
touch src/main.rs && timeout 900 cargo build --offline \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
touch src/main.rs && timeout 900 cargo build --offline --features daemon \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
```

Both diffs are empty, exit 0, so the whole 50-screen help surface is byte-identical to the pre-daemon baseline in the unfeatured and the featured build alike.
`the_help_surface_still_matches_the_pre_daemon_baseline` in `daemon_read_only_methods` asserts the same property inside the suite.

The plain test count is 1355 against Phase 2's 1352, with every addition in a new test and no pre-existing assertion modified.
The golden frames and the TUI counts are untouched: `test_selection_guard` holds its floors of 368 TUI tests, 20 golden-frame tests and 18 snapshot files, and no Phase 3a commit touches `src/tui/`.

Passes.

## Clippy

```sh
timeout 300 cargo clippy --workspace --features daemon --offline --all-targets
```

Exit 0.
No warning names a file under `src/daemon/`, under `crates/`, or under `tests/`.
The warnings that used to sit in the four test files were cleared by the review fixes: seven of them at this toolchain (`tests/daemon_events.rs` 4, and one each in `tests/daemon_framing.rs`, `tests/daemon_read_only_methods.rs` and `tests/cli_help_snapshot.rs`), where the phase's own note counted eight because the `collapsible_match` warning spans two reported lines.
Every fix is a mechanical lint change that left the assertion alone: `0..=2` for an or-pattern, `is_multiple_of` twice, one collapsed `if let`, two redundant `trim_start()` calls before `split_whitespace()`, and an `#[allow(clippy::assertions_on_constants)]` on the protocol-range assertion that is constant only while `PROTOCOL_MIN` and `PROTOCOL_MAX` are both 1.
The warnings the run still reports are pre-existing ones elsewhere in the tree (`src/imap_client/search.rs` 10, `src/draft.rs` 4, `examples/mkfixture.rs` 4, `src/oauth2.rs` 3, `src/config.rs` 2, and one each in six other files).

## Housekeeping

No daemon was left running by this evidence run: `pgrep -af 'mp daemon'` returns nothing (exit 1).
Every test that starts one points `MAILYPOPPINS_DATA_DIR` at a `TempDir` and stops it in the same test, so nothing here touched the real data directory.

## What Phase 3a does not answer

The two interactive measurements escalated out of Phase 0, again out of Phase 1a and again out of Phase 2 are still open: the held-key preview A/B (W1) and cold first paint (W5) both need a terminal, a configured account and a warm store.
`docs/baselines/pre-daemon/measurements.md` is where the numbers go when they are taken.

The macOS half of Phase 2's crash and stale-socket recovery line is unchanged and still escalated; Phase 3a adds no platform-specific code to it.

Three properties are proved with test hooks rather than with product code, because Phase 3a commits no state change of its own: readiness (`MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS`), event volume (`MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST`) and long-running work (`MAILYPOPPINS_DAEMON_FAKE_OPERATIONS`).
They are documented in `docs/daemon-operations.md`, and the first real producers arrive with the account runtimes in Phase 3b and the sync methods in Phase 5.
The snapshot's `operations` projection is the one shape no end-to-end test covers: it is unit-tested against the registry, and no wire test bootstraps a connection while an operation is in flight.
