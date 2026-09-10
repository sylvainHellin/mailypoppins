# Phase 3b exit gate

The three gate lines of Phase 3b of the daemon migration (`.agents/workflow/native-gui-daemon/plan.md`, section 4), each mapped to the test or command that proves it, with the result recorded rather than asserted.

Ticket [#0122](../tickets/0122-daemon-ownership-moves.md).
Evidence taken on 2026-09-10 at `38f823c` on branch `daemon`, twelve commits past the Phase 3a exit, all twelve of them Phase 3b's own, plus the two-line clippy fix in `tests/daemon_account_runtime.rs` that this sweep commit carries.
Toolchain `rustc 1.96.0 (ac68faa20 2026-05-25)`, host Ubuntu 26.04 LTS, Linux 7.0.0.

All three pass as written.

## The two whole-tree runs

Every line below is a slice of one of these two runs, so they come first.

```sh
timeout 900 cargo test --workspace --offline
timeout 900 cargo test --workspace --features daemon --offline
```

1365 tests pass without the feature, 0 failed, 3 ignored (all three in the doc-tests), across 21 result lines.
1802 pass under it, 0 failed, 4 ignored (the same three plus one for want of root in `daemon_runtime_paths`), across 36 result lines.

The 437 difference is the daemon's own: 366 in the fifteen gated `[[test]]` targets and 71 in the inline `#[cfg(test)]` modules inside `src/daemon/`, which is the lib target at 1218 under the feature against 1147 without it.

The plain figure is 1365 against Phase 3a's 1355.
Nine of the ten additions are `tests/engine_lock_ingest.rs`, the one Phase 3b test file that is deliberately not gated, and the tenth is an inline test in the library.
No pre-existing test changed its assertions.

The featured figure is 1802 against Phase 3a's 1605.
Phase 3b adds 197: 167 in the five new gated suites, 9 in `engine_lock_ingest` which counts in both runs, 20 inline in `src/daemon/` and 1 inline in the library.

The six new files, gated ones first:

- `daemon_draft_watch` 44
- `daemon_sync_outcome` 41
- `daemon_handles` 41
- `daemon_config` 25
- `daemon_account_runtime` 16
- `engine_lock_ingest` 9, in the default suite

The other nine gated targets are unchanged from Phase 3a: `daemon_operations` 30, `daemon_events` 29, `daemon_framing` 29, `daemon_bootstrap` 28, `daemon_dispatcher` 18, `daemon_protocol_fixtures` 17, `daemon_runtime_paths` 17, `daemon_handshake` 12, `daemon_lifecycle` 10, `daemon_read_only_methods` 9.

## The six T-unit proofs

The gating convention of section 3.0 is that a T unit's test file compiles only once its I unit exists, and the T unit's report quotes the unresolved-import errors naming exactly the contract items.
Each proof was taken at its own commit, before the paired implementation, and is recorded in that commit's message.

P3b-U1 is the exception the plan itself names: `tests/engine_lock_ingest.rs` is not feature-gated, because the engine lock on the ingest path is a change to library behaviour that every build ships, so a red test file would break the default suite.
It landed with its nine tests `#[ignore]`d and P3b-U2 removed the markers in the same commit (`1f83463`), which is the convention the plan recommends for exactly this unit.

The five gated T units are P3b-U3 (`b37b87a`), P3b-U5 (`01c13ac`), P3b-U7 (`4832d47`), P3b-U9 (`da8de27`) and P3b-U11 (`675b8ed`).
Each left `cargo test --workspace --offline` green and unchanged and `cargo test --workspace --features daemon --offline` failing to compile with unresolved imports naming only its own contract items.

Four contract-test lines were amended by an implementer, each approved before the implementing commit and each recorded in the ticket: two in `tests/daemon_account_runtime.rs` (`7308a45`), one in `tests/daemon_sync_outcome.rs` and one in `tests/daemon_bootstrap.rs` (both `0a807d2`).

## A second writer against the same account is refused rather than admitted

Units P3b-U1 and P3b-U2, at the ingest entry point as well as at the two drains that already refuse.

```sh
cargo test --workspace --offline --test engine_lock_ingest --test outbox_integration
```

Green: 9 in `engine_lock_ingest`, 31 in `outbox_integration`, 40 total, 0 failed.

The refusal is asserted to cost nothing rather than merely to return `None`: the fake backend's `fetch_targets` panics, so a `run_sync_guarded` that opened a session would fail the test rather than pass it quietly.
The holder proceeds normally, and the four `store_ingest_integration` invariants (`n_listed_copies_of_one_message_id_get_n_rows`, `a_second_pass_over_the_same_copies_changes_no_row`, `the_unconditional_policy_still_rebinds_onto_a_listed_uid`, `a_reset_pass_maps_n_copies_onto_n_rows`) hold unchanged under the lock.
At the callers, the refusal is a success: `mp sync` prints its skip line and exits 0, and the TUI shows the same sentence as an info status line.

Passes, with one carve-out recorded in the ticket: `graph::sync_mailboxes_graph` runs outside the engine and is not guarded, so the gate holds for the IMAP path only.

## The daemon survives client disconnects and continues watchers and durable operations

Units P3b-U3, P3b-U4, P3b-U9 and P3b-U10.

```sh
cargo test --workspace --features daemon --offline --test daemon_account_runtime --test daemon_draft_watch
```

Green: 16 in `daemon_account_runtime`, 44 in `daemon_draft_watch`, 60 total, 0 failed.

The runtime holds its account's engine lock for its lifetime rather than for one operation, so a client disconnecting does not release it; a contended lock is a successful start that reports `Blocked` and still serves reads.
The watcher runs on the daemon's own interval, reads no contents while looking, opens no store and takes no engine lock, so it runs whether or not `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES` is set, and the socket half of `daemon_draft_watch` drives it through a subscribed client rather than in process.

Passes.

## Config and draft external edits propagate correctly

Units P3b-U7 to P3b-U10.

```sh
cargo test --workspace --features daemon --offline --test daemon_config --test daemon_draft_watch
```

Green: 25 in `daemon_config`, 44 in `daemon_draft_watch`, 69 total, 0 failed.

A valid swap adds, updates and removes runtimes in a controlled order and answers only once every runtime it touched has settled, which is what makes a removed account's engine lock free on return.
An invalid edit keeps the previous snapshot live and travels as `-32007` and as a `config.invalid` event carrying the same file and line.
A secret appears in no log line, no error payload and no `config.get` result.
On the draft side, a direct write, an atomic-save rename, four saves inside one debounce window and a deletion each produce exactly one event about the final state, and a file the daemon cannot parse is left byte-identical on disk.

Passes.

## Nothing outside the feature changed for a client that never sets the debug flag

Not a Phase 3b gate line, because Phase 3b deliberately does change one thing outside the feature: the ingest lock.
The help surface is checked anyway, since it is the one artifact the migration promises not to move.

```sh
touch src/main.rs && cargo build --offline \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
touch src/main.rs && cargo build --offline --features daemon \
  && MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt
```

Both diffs are empty, exit 0, so the whole 50-screen help surface is byte-identical to the pre-daemon baseline in the unfeatured and the featured build alike.

The hidden surfaces are still hidden.
`mp --help` from the featured build contains no occurrence of the string `daemon`, as the baseline does not: the global `--daemon` flag and the `mp account` and `mp daemon` subcommands all carry `hide = true` behind `#[cfg(feature = "daemon")]`.
The account runtimes stay behind `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES`, an environment variable rather than a flag, so it cannot reach help either.

## Clippy

```sh
timeout 300 cargo clippy --workspace --features daemon --offline --all-targets
```

Exit 0.
No warning names a file under `src/daemon/` or under `crates/`.

Seven warnings sit in the phase's own test files, against the "zero" the phase aimed at, and two more were there before this sweep:

- `tests/daemon_account_runtime.rs` had 2 (`field_reassign_with_default` on the `AccountConfig` fixture, `cmp_owned` on a `Value::from(ACCOUNT)` comparison).
  Both are fixed in this sweep commit, which is the only code change it carries; the fix was approved in advance and is two one-liners that leave every assertion alone.
- `tests/daemon_draft_watch.rs` has 5 (four `doc_lazy_continuation` in the module header, one `cmp_owned`).
- `tests/daemon_config.rs` has 2 (`cmp_owned`, twice).

Those seven are left alone rather than fixed: they are T-unit files, the sweep's approval covered `daemon_account_runtime` only, and the convention is that an implementer edits a pinned test file only with a written approval naming it.
They are a `BACKLOG.md` follow-up.

`tests/engine_lock_ingest.rs`, `tests/daemon_sync_outcome.rs` and `tests/daemon_handles.rs` report none.

The rest of the run is pre-existing warnings elsewhere in the tree: `src/imap_client/search.rs` 5, `src/draft.rs` 4, `examples/mkfixture.rs` 4, `src/oauth2.rs` 3, `src/config.rs` 2, and one each in `src/parse.rs`, `src/ingest.rs`, `src/dump.rs`, `src/store/drafts.rs`, `src/tui/app/keymap.rs`, `src/tui/app/keys.rs` and `src/tui/app/types.rs`.

## Housekeeping

No daemon was left running by this evidence run: `pgrep -af '[m]p daemon'` returns nothing (exit 1).
Every test that starts one points `MAILYPOPPINS_DATA_DIR` at a `TempDir` and stops it in the same test, so nothing here touched the real data directory.

`src/store/sweep.rs` is not rustfmt-clean, with two `rustfmt --edition 2021 --check` hunks left.
Both predate #0122 and neither is in code P3b-U12 wrote: the file had eleven before that unit and has two after it.
The tree as a whole has never been rustfmt-clean, so this is a note rather than a regression, and it is in `BACKLOG.md`.

## One flake, measured

`tests/engine_lock_ingest.rs` fails about twice in twenty-five runs of its own binary, in `the_unconditional_policy_still_rebinds_onto_a_listed_uid_under_the_lock` or `the_lock_holder_runs_the_pass_and_releases_the_lock`, with a guarded pass returning the refusal against a lock file in its own tempdir.

`mp_sync_exits_zero_and_says_it_skipped_when_another_process_holds_the_lock` runs in the same binary and forks an `mp` child while sibling tests hold their own `flock`ed descriptors; the child inherits every open descriptor until `exec` closes the `O_CLOEXEC` ones, and an inherited copy keeps the `flock` alive for that window.

It was measured at 2 in 25 both with and without the config-ownership unit, so it is not that unit's doing, and the two whole-tree runs above both passed.
The fix is to keep the process-spawning test off the threads that hold locks, and it is a `BACKLOG.md` follow-up rather than a widened assertion.

## What Phase 3b does not answer

The two interactive measurements escalated out of Phase 0, and again out of Phases 1a, 2 and 3a, are still open: the held-key preview A/B (W1) and cold first paint (W5) both need a terminal, a configured account and a warm store.
`docs/baselines/pre-daemon/measurements.md` is where the numbers go when they are taken.

The macOS half of Phase 2's crash and stale-socket recovery line is unchanged and still escalated; Phase 3b adds no platform-specific code to it.

Two seams are built and proved but have no production caller yet.
`RuntimeTable::tick_account` is driven only by its own tests, because the periodic scheduler is Phase 5/6, and `store::sweep::sweep_pinned` is called only by `sweep` with an empty pin set, because nothing in the daemon sweeps yet.
Both are in `BACKLOG.md`.
