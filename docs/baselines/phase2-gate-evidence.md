# Phase 2 exit gate

The eight gate lines of Phase 2 of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the test or command that proves it, with the result recorded rather than asserted.

Ticket [#0120](../tickets/0120-daemon-transport-and-read-only-methods.md).
Evidence taken on 2026-09-09 at `ef6878e` on branch `daemon`, twelve commits past the Phase 1a exit `a8ec8ef`, all twelve of them Phase 2's own.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0.

Seven lines pass as written.
One is provable on Linux only, and half of one moves to Phase 3b; both are stated below rather than papered over.

## The two whole-tree runs

Every line below is a slice of one of these two runs, so they come first.

```sh
timeout 900 cargo test --workspace --features daemon --offline
timeout 600 cargo test --workspace --offline
```

1477 tests pass under the feature, 0 failed, 3 ignored in the doc-tests plus 1 skipped for want of root (below).
1352 pass without it, 0 failed, across 20 result lines.

The 125 difference is the daemon's own: 94 in the six gated `[[test]]` targets (`daemon_framing` 29, `daemon_runtime_paths` 17, `daemon_protocol_fixtures` 17, `daemon_handshake` 12, `daemon_lifecycle` 10, `daemon_read_only_methods` 9) and 31 in the inline `#[cfg(test)]` modules inside `src/daemon/`, which is `cargo test --lib` at 1176 under the feature against 1145 without it.

The figures moved by 7 in the P2-U12 review pass: two fixture tests for the documented field sets of the read-only responses, three for the read-only store probe in `methods/account.rs`, and two for the response cap in `server.rs`.

The plain figure is 1352 against Phase 1a's 1335, and all 17 additions are new files: `tests/test_selection_guard.rs` (5), the `mp-client` unit tests (5) and doc-test (1), and the `mp-protocol` unit tests (6).
No pre-existing test changed.

## mp daemon run, status and stop work over a socket guarded by the startup lock, with an initialize handshake and committed fixtures

Units P2-U4 to P2-U9.

```sh
cargo test --workspace --features daemon --offline \
  --test daemon_lifecycle --test daemon_handshake --test daemon_protocol_fixtures
```

Green: 10 in `daemon_lifecycle`, 12 in `daemon_handshake`, 17 in `daemon_protocol_fixtures`, 39 total, 0 failed.

`daemon_lifecycle` drives the real binary in a temp data root: `run` binds the socket and answers `status`, `status` without a daemon exits 1 and says so, `stop` removes only its own socket, `stop` without a daemon exits 0, `restart` yields a new instance id, `SIGTERM` to a foreground daemon exits 0 and unlinks the socket, and `run` performs the `MIG-04` legacy config-directory move before the first config read.

`daemon_protocol_fixtures` is the committed-fixtures half: every required fixture is present, each round-trips through its type byte-identically, each is stored canonically, each survives a frame round trip, each declares JSON-RPC 2.0, `initialize` is the only unnamespaced method, and the read-only request and response fixtures carry exactly the fields `docs/daemon-protocol.md` documents.

Passes.

## account.list and message.list answer through mp --daemon for one mailbox, with no engine lock acquired and no existing client path changed

Units P2-U10 and P2-U11.

```sh
cargo test --features daemon --offline --test daemon_read_only_methods
MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
```

Green: 9 passed, 0 failed.
The help diff is empty, and it is empty for both builds: the capture was taken once from `cargo build --offline` and once from `cargo build --offline --features daemon`, and neither differs from the pre-daemon baseline by a byte.

The three lines the gate turns on:

- `serving_reads_holds_no_engine_lock` locks `<account_dir>/store.lock` from the test process while the daemon serves both methods, so the daemon provably did not take it.
- `the_routed_cli_prints_what_the_direct_cli_prints` compares the routed rendering against the direct one byte for byte, which is what makes the hidden `mp account list` an oracle rather than a second implementation.
- `the_help_surface_still_matches_the_pre_daemon_baseline` is the `hide = true` proof inside the suite, so the diff above is belt to its braces.

`the_daemon_flag_does_not_fall_back_to_the_direct_path` covers the other half of "no existing client path changed" from the opposite direction: a routed command with no daemon exits 4 rather than answering in process.

Passes.

## Concurrent startup launches exactly one daemon

Units P2-U6 and P2-U7.

```sh
cargo test --features daemon --offline --test daemon_lifecycle two_concurrent_starts
```

Green: `two_concurrent_starts_yield_one_pid_file_and_one_instance_id`, 1 passed.

The same property is proved a second time below the CLI, in `tests/daemon_runtime_paths.rs`: `two_threads_racing_for_the_start_lock_yield_exactly_one_some` races two `acquire_start_lock` calls and asserts exactly one `Some`.
The pair matters because the CLI test proves the outcome and the lock test proves the mechanism.

Passes.

## Crash and stale-socket recovery pass on macOS and Linux

Units P2-U4 and P2-U5.

```sh
cargo test --features daemon --offline --test daemon_runtime_paths
```

Green on Linux: 17 passed, 0 failed, 1 ignored.

The recovery cases: a socket with no listener probes `Stale` and is removed, a live socket probes `Live` and `remove_stale_socket` refuses it, a socket with group or other bits probes `Unsafe` and is never removed, a regular file at the socket path probes `Unsafe`, a missing path probes `Absent`, and `remove_stale_socket` refuses a path that does not exist.
The directory half is there too: `ensure_runtime_dir` creates at 0700, tightens a loose directory, and is idempotent across group and other bits.

Two gaps, neither of them a Linux failure.

`a_socket_owned_by_another_uid_probes_unsafe_and_is_never_removed` is ignored with the reason printed by the harness: a test process cannot create a file owned by another uid.
It is skipped on any host without root, this one included, so the wrong-owner branch of `probe_socket` is covered by reading rather than by running.

The macOS half of the gate line cannot be proved here at all.
This is a Linux host, and the plan already carries the macOS run as an escalation to Sylvain rather than as a line a Linux agent may sign off.
Everything the suite touches is POSIX (`flock`, `stat`, `connect`, `unlink`, `umask`), and `docs/plans/daemon-dependencies.md` verified in the vendored tokio source that peer credentials resolve on both platforms, but neither observation is a test run.

Passes on Linux, escalated on macOS.

## Incompatible clients fail clearly, including a client whose directory pair does not match

Units P2-U8 and P2-U9.

```sh
cargo test --features daemon --offline --test daemon_handshake identity_mismatch
cargo test --features daemon --offline --test daemon_handshake protocol_incompatible
cargo test --features daemon --offline --test daemon_handshake capability_missing
```

Green: `a_differing_config_dir_is_an_identity_mismatch` and `a_differing_data_dir_is_an_identity_mismatch` (2 passed), `an_unsupported_protocol_range_is_protocol_incompatible` (1), `a_missing_required_capability_is_capability_missing` (1).

Both directions of the identity case are covered, which is the point of sending the pair unconditionally: `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` are independent, so a client with only one override set is the case that would otherwise slip through.
The refusal names both directories on both sides, so a user can see all four and tell which override to drop.

`a_second_initialize_is_an_invalid_request` and `a_domain_method_before_initialize_is_not_initialized` cover the adjacent refusals, and `daemon_status_answers_without_an_initialize` proves the exemption that keeps `mp daemon status` usable against a daemon it cannot negotiate with, which is what "fail clearly" means in practice.

Passes.

## A daemon started with no config.toml serves config.* and reports zero accounts

Units P2-U6 and P2-U7.

```sh
cargo test --features daemon --offline --test daemon_lifecycle run_without_a_config_file
```

Green: `run_without_a_config_file_serves_zero_accounts`, 1 passed.
The daemon starts, binds, answers `daemon.status` with `"accounts": []`, and reports `config_status.state` as `absent` with the path it read, so a client can tell "no accounts yet" from "your config does not parse".
`config_status_states_map_onto_the_client_variants` in `daemon_handshake` pins all three states.

Half met, and the other half is not Phase 2's to meet.
There is no `config.*` method family in this build: the plan puts it in Phase 3b (P3b-U7 to P3b-U10, `tests/daemon_config.rs`), and Phase 2's advertised capabilities are exactly `daemon.status`, `daemon.stop`, `account.list` and `message.list`.
What Phase 2 owes the line is that a missing `config.toml` is not a startup failure, and that is proved.
The `config.*` half moves to the Phase 3b checklist.

## A compatible client can connect repeatedly without leaking tasks or descriptors

Units P2-U8 and P2-U9.

```sh
cargo test --features daemon --offline --test daemon_handshake fifty_connect
```

Green: `fifty_connect_initialize_close_cycles_leak_no_descriptor`, 1 passed.
Fifty sequential connect, initialize and close cycles against one daemon, with the descriptor count read from the daemon's own `/proc/self/fd` rather than inferred from the 51st connection succeeding.

`malformed_json_closes_only_the_offending_connection` is the other side of the same property: one connection dying takes nothing else with it.

Passes.

## CI runs the workspace test selection introduced with crates/, and the golden-frame and TUI test counts are unchanged

Units P2-U1a and P2-U1b.

```sh
cargo test --offline --test test_selection_guard
git show f25e40f --stat -- .github/workflows/ci.yml AGENTS.md
```

Green: 5 passed, 0 failed.
`tui_test_count_has_not_dropped`, `golden_frame_test_count_has_not_dropped` and `snapshot_file_count_has_not_dropped` hold their floors of 368, 20 and 18, and the two self-tests prove the counters see both `#[test]` spellings, ignore look-alikes, walk nested directories and skip non-snapshots.

`.github/workflows/ci.yml` carries both commands, added in the same commit as the workspace conversion:

```
cargo test --workspace
cargo test --workspace --features daemon
```

`AGENTS.md` carries the same pair, so the file a contributor reads and the file CI runs cannot disagree.

Passes.

## Housekeeping

No daemon was left running by this evidence run: `pgrep -af 'mp daemon'` returns nothing.
Every test that starts one points `MAILYPOPPINS_DATA_DIR` at a `TempDir` and stops it in the same test, so nothing here touched the real data directory.

## What Phase 2 does not answer

The two interactive measurements escalated out of Phase 0 and again out of Phase 1a are still open, and Phase 2 could not close either: the held-key preview A/B (W1) and cold first paint (W5) both need a terminal, a configured account and a warm store.
W5 gains urgency here rather than losing it, because `mp daemon start` is now a real thing that can sit on the launch path.

`docs/baselines/pre-daemon/measurements.md` is where the numbers go when they are taken.
