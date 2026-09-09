---
id: 0120
title: Phase 2 of the daemon migration, the transport, the handshake and the first read-only methods
type: feature
priority: now
status: done
created: 2026-09-09
---

Third ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md`), after #0118 and #0119.

Phase 2 is the first one that writes product code, and all of it is behind the `daemon` cargo feature.
It fixes the protocol, builds the socket and its lifecycle commands, negotiates a handshake, and serves two read-only methods over it.
`cargo install --path .` still ships an `mp` that contains not a byte of this.

## Why the protocol first

A protocol is expensive to change once fixtures pin it and clients read it, which is exactly why Phase 1a measured the four shapes a preference cannot settle before this phase started.
So the order here is wire, then socket, then handshake, then methods, and each pair of units is a contract test followed by the implementation that makes it green.
The test unit's proof is that `cargo test` is unchanged and green while `cargo test --features daemon` fails to compile with unresolved imports naming exactly the contract items; a test unit whose featured build succeeds has written a vacuous test.

## What landed

Twelve units in twelve commits.

- P2-U1a (`8142689`): `tests/test_selection_guard.rs`, landed before the workspace exists so its three floors (368 TUI tests, 20 golden-frame tests, 18 snapshot files) are a true "before". It counts `#[test]` attributes by scanning the sources rather than by asking the harness what it selected, because a guard that counted executed tests would disappear along with the tests it defends.
- P2-U1b (`f25e40f`): the workspace conversion. `crates/mp-protocol` and `crates/mp-client` as empty members, the `daemon` feature, the `[[test]] required-features` convention, `spikes/` and `desktop/` excluded, and both test commands into `AGENTS.md` and `.github/workflows/ci.yml`.
- P2-U2 (`6b9e9c2`): `tests/daemon_protocol_fixtures.rs` and `tests/daemon_framing.rs`, against the `mp_protocol` contract.
- P2-U3 (`820845c`): `crates/mp-protocol`, the eleven fixtures, and `docs/daemon-protocol.md` with its protocol changelog at version 1. Newline-framed JSON-RPC 2.0, a 1 MiB request cap counted inclusive of the terminator and enforced on the decoder's buffer rather than on a completed line, and the ten daemon error codes.
- P2-U4 (`d9e8c9b`) and P2-U5 (`4d97684`): `src/daemon/runtime.rs`. The 0700 runtime directory that tightens rather than tolerates, the 0600 socket bound under `umask(0177)`, the `flock` start lock whose file is never unlinked, and the four-way socket probe that classifies without touching anything.
- P2-U6 (`0f17193`) and P2-U7 (`3cbcb5b`): `src/daemon/{server.rs,lifecycle.rs}` and the `mp daemon` subtree. The startup order, the detached spawn through `setsid`, readiness as a real `daemon.status` round trip, graceful `SIGTERM` and `SIGINT`, and the exit-4 diagnostic.
- P2-U8 (`7b3428b`) and P2-U9 (`cf81a70`): `crates/mp-client` and `src/daemon/session.rs`. The `initialize` handshake, the `not_initialized` gate on everything but the two lifecycle methods, and the three refusals (identity, protocol range, capability).
- P2-U10 (`75e1752`) and P2-U11 (`ef6878e`): `src/daemon/methods/` and the two routed CLI handlers. `account.list` and `message.list`, neither taking an engine lock.
- P2-U12 (this ticket): `docs/daemon-operations.md`, the "Daemon and crate boundaries" section of `docs/architecture.md`, the gate evidence, and the exit sweep.

## The shapes worth remembering

The start lock is a separate file from the pid file because `flock` lifetime is the file descriptor's, so the kernel releases it however the process died.
A pid file cannot do that, so `daemon.pid` is diagnostic output and `daemon.start.lock` is the primitive.
The lock file is never unlinked: two starters that each created a different inode under the same name would both win.

`mp daemon start` holds the lock across the spawn *and* the readiness wait, because releasing it at spawn time would let a second starter observe a socket inode the first daemon has not bound yet.
The child is told so through `MAILYPOPPINS_DAEMON_START_LOCK_HELD=1`.

The socket probe classifies and never decides.
`Unsafe` outranks `Stale`, the metadata checks run before the connection attempt, and only `Stale` is ever unlinked, because a daemon that deletes a file it does not understand is a daemon that deletes user data.

`daemon.status` and `daemon.stop` answer before the handshake.
They are lifecycle surface, they touch no account data, and the exemption is what lets `mp daemon status` describe a daemon whose protocol range this build cannot negotiate and `mp daemon restart` end one.

The identity pair travels unconditionally in `initialize` because `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` are independent: a client with only the config override set would otherwise reach a daemon holding different config, secrets and signatures.

## Deviations from the plan

Three, all of them decided inside the units and recorded here rather than left to a reader's diff.

### `mp daemon` is hidden instead of moving the help snapshot

P2-U6 specifies the lifecycle commands as visible CLI surface and instructs the unit to update the help snapshot, flagging the move as deliberate.
That cannot work while the feature gate exists.
`tests/cli_help_snapshot.rs` holds one snapshot file, and both `cargo test --workspace` and `cargo test --workspace --features daemon` run it, so a snapshot with `mp daemon` in it fails the plain run and one without it fails the featured run.

The subcommand therefore carries `#[command(hide = true)]` on top of the `cfg`, and the snapshot did not move.
`mp --help` is byte-identical to `docs/baselines/pre-daemon/cli-help.txt` in both builds, which is a stronger property than the plan asked for and one `tests/daemon_read_only_methods.rs` asserts directly.
P4-U1 drops both the `cfg` and the `hide` and moves the snapshot once, when the daemon becomes the default and there is only one build to satisfy.

### `mp account list` exists, hidden and feature-gated

P2-U10 requires `mp --daemon account list` to route through `mp-client`, and the plan's own criterion is that the routed answer matches the direct one.
There was no direct one: nothing in the CLI listed the configured accounts, so the routed command had no oracle.

A hidden `mp account list` behind the same feature gate is the oracle, and `the_routed_cli_prints_what_the_direct_cli_prints` compares the two renderings byte for byte.
The command is not new surface a user can find; it becomes visible with the rest of the daemon work at P4-U1.
`--daemon` itself is gated for the same reason the subcommands are, one gate more than section 3.0 asked for, which specified only `hide = true`.

### `message.list` carries both dates

The wire shape the plan fixes for `message.list` carries `date_sort`, which is `tui::app::resolve_date`'s sort key and cannot be turned back into the date a listing prints.
The row therefore carries `date_display` beside it, the `Date:` header as the store holds it, so `mp --daemon list-messages` renders from the wire alone and opens no store of its own.

## Gate evidence

`docs/baselines/phase2-gate-evidence.md` maps each of the eight Phase 2 exit-gate lines to the test or command that proves it, with the commands as run and their results.

Seven pass as written.
The eighth, "crash and stale-socket recovery pass on macOS and Linux", is proved on Linux by `tests/daemon_runtime_paths.rs` and cannot be proved here on macOS; the plan already requires that half to be escalated.
One test inside it is skipped on any host without root, since a test process cannot create a file owned by another uid.

## Acceptance criteria

- The lifecycle commands work over a socket guarded by the start lock, with a handshake and committed fixtures. Met, 37 tests across `daemon_lifecycle`, `daemon_handshake` and `daemon_protocol_fixtures`.
- `account.list` and `message.list` answer through `mp --daemon` with no engine lock acquired. Met, 9 tests in `daemon_read_only_methods`, including `serving_reads_holds_no_engine_lock`.
- No existing client path changed. Met, `mp --help` is byte-identical to the pre-daemon baseline in both builds and the plain test count moved only by the phase's own additions.
- Concurrent startup launches exactly one daemon. Met, `two_concurrent_starts_yield_one_pid_file_and_one_instance_id`.
- CI runs both test selections and the golden-frame and TUI counts are unchanged. Met, `.github/workflows/ci.yml` and `tests/test_selection_guard.rs`.

## Files

- `crates/mp-protocol/**`, `crates/mp-client/**`
- `src/daemon/{mod.rs,runtime.rs,server.rs,session.rs,lifecycle.rs,methods/**}`
- `src/main.rs` (the hidden `--daemon` flag, the `daemon` and `account` subtrees, the two routed handlers)
- `tests/{test_selection_guard,daemon_framing,daemon_protocol_fixtures,daemon_runtime_paths,daemon_lifecycle,daemon_handshake,daemon_read_only_methods}.rs`
- `docs/daemon-protocol.md`, `docs/daemon-operations.md`, `docs/architecture.md`, `docs/baselines/phase2-gate-evidence.md`
- `Cargo.toml`, `.github/workflows/ci.yml`, `AGENTS.md`, `CHANGELOG.md`, `BACKLOG.md`, `docs/lessons-learned.md`
