---
id: 0124
title: Phase 5 of the daemon migration, the TUI cutover
type: feature
priority: now
status: in-progress
created: 2026-09-10
---

Status: in-progress. P5-U1 and P5-U2 have landed; P5-U3 is next.

Seventh ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.7), after #0118, #0119, #0120, #0121, #0122 and #0123.

Phase 4 took the CLI off the direct path.
Phase 5 takes the TUI off it: the six `open_store` call sites in `src/tui/app/{mod.rs,types.rs}` become typed queries, every `Action` becomes a daemon method or a documented client-only one, the watcher threads become event subscriptions, and `src/tui/` becomes a crate that depends on `mp-client` and `mp-protocol` and on nothing else.
The gate is the parity-gate oracle suite, five oracles, of which the daemon-backed golden frames are the first to exist.

## The unit table

| unit | kind | commit | subject | status |
|---|---|---|---|---|
| P5-U1 | T | this commit | daemon-backed golden frames | done (tests) |
| P5-U2 | I | this commit | TUI initialisation via handshake + bootstrap | done |
| P5-U3 | T | | the query layer contract | open |
| P5-U4 | I | | the query layer | open |
| P5-U5 | T | | actions to commands, the contract | open |
| P5-U6 | I | | actions to commands | open |
| P5-U7 | T | | events replace watcher threads, the contract | open |
| P5-U8 | I | | events replace watcher threads | open |
| P5-U9 | T | | the parity-gate oracle suite | open |
| P5-U10 | I | | the crate boundary for the TUI | open |
| P5-U11 | I | | the gate run, the report, the docs | open |

## P5-U1: the daemon-backed golden frames

`src/tui/ui/golden_frames_daemon.rs` (555 lines), a sibling module of `src/tui/ui/golden_frames.rs`, registered behind `#[cfg(test)]` in `src/tui/ui/mod.rs`.
The plan recommended the sibling module over a `tests/` file so it shares `frame_snapshot`, `pin_theme`, the 120x40 size and the two-section snapshot format, and it does: the capture helpers, the size and every fixture are `pub(super)` now, so the two families cannot drift.

22 tests: a daemon-backed variant of each of the 18 snapshot frames, a daemon-backed `frames_are_reproducible`, and three rows that have no hand-built counterpart (the `opening` account, one mailbox row more, one count different).
`legend_tags_the_states_it_claims_to` is the twentieth test of the hand-built file and gets no variant: it is about the tag table, not about an `App`.

### The contract

Two names, and nothing else.
P5-U2 supplies them; no other item is missing, which the stub proof below is the evidence for.

1. **`mp_protocol::state::Bootstrap`**, a `Deserialize` decode of the whole `state.bootstrap` result, so a client turns one JSON answer into one value (`serde_json::from_value::<Bootstrap>(result)`).
   Its *fields* are deliberately not pinned: the test decodes one and hands it straight to the constructor, so P5-U2 owns the field set and only has to keep the decode working against the shape `docs/daemon-protocol.md` documents.
2. **`App::from_bootstrap(global_config: crate::config::GlobalConfig, bootstrap: &mp_protocol::state::Bootstrap) -> App`**.

What the frames require of the constructor, pinned by what they render rather than by a field assertion:

- One `AccountState` per snapshot account, in the snapshot's order, which is `config.toml`'s, with `active_account` at 0.
- `mailboxes` from that account's mailbox rows: `{role, slug, label}` becomes the `MailboxInfo` `build_mailboxes` gives that role, so the icon and the `MailboxKind` a renderer branches on come from the role and the label comes from the wire.
- `mailbox_counts` from each row's `total`, in the same order.
- `opening` from the account's `state`: `opening` keeps the #0003 loading state (zeroed counts and the `··` marker), `ready` clears it.
- No store opened, no engine lock taken, no mail directory walked, no message row invented.

Everything the fixture needs beyond those two names already exists and is used as-is: `DaemonState::new` assembles a daemon over a fixture data root, `CanonicalState::apply` commits the `MailboxCounts` and `AccountReady` changes a live runtime would publish, and `Dispatcher::dispatch` serves `state.bootstrap`.

### The oracle, and why there are only two `…_daemon` snapshots

Each of the 18 frames asserts the daemon-built frame is **byte-identical to the hand-built frame of the same fixture**, which is the stronger of the two options the unit was given: those 18 frames are already pinned by the reviewed snapshots in `src/tui/ui/snapshots/`, so a daemon-built frame that matches one is pinned by it too and no second family of snapshot files has to be approved on trust.
The sidebar is the only chrome a bootstrap feeds today, and the sidebar titles itself after the account only when there is more than one, so a one-account fixture renders the frame #0049 reviewed.

Two frames have no hand-built counterpart and carry a `…_daemon` insta snapshot of their own: `golden_opening_account_daemon` and `golden_extra_mailbox_daemon`.
Those two `.snap` files are **not in this commit**, because a T unit cannot mint a snapshot it has no implementation to render: **P5-U2 mints them at its first `cargo insta review`**, and a diff there is a decision, as the plan says.
Both rows assert their point before they reach the snapshot (the `··` marker and the absence of a count; the fifth mailbox label and `assert_ne!` against the four-mailbox frame), so neither is carried by the snapshot alone.

The third sensitivity row, `a_different_count_in_the_bootstrap_changes_the_frame`, needs no snapshot at all: it asserts the count column moved.
Together the three are what a wrong snapshot has to change: a `from_bootstrap` that hand-assigned the four mailboxes and ignored its argument passes all 18 frames and fails all three.

One mailbox row *more* rather than one fewer, by the way: `build_mailboxes` emits the four roles unconditionally, so a configured account cannot have fewer than four rows.

### What the bootstrap owns, and what waits for P5-U3

A `state.bootstrap` snapshot carries accounts, mailboxes, counts, drafts and outbox totals.
It carries no message row, by design: the whole-list transfer `docs/baselines/decisions/list-transfer.md` chose is a query per mailbox open, which is P5-U3's.

So a daemon-backed frame is a join, and the test says which half is whose: `App::from_bootstrap` builds `accounts`, `account_config`, `mailboxes` and `mailbox_counts`, and the frozen fixture of `golden_frames.rs` still supplies the message rows, the cursor, the overlays and the preview memos.
P5-U3/U4 replace the second half; the four fields are the first half and are what these frames pin.

### Determinism

The three rules the hand-built frames keep, plus one.
The theme is pinned before the first `App` is built rather than only at capture time, because `from_bootstrap` selects a theme the way `App::new` does and `theme::init` is a `OnceLock`.
The data root is a per-thread tempdir (`config::test_env::TestDataDir`, #0077) held alive for as long as the `App`, so per-account path resolution cannot reach the developer's tree or a real keyring.
The daemon is seeded from a literal `GlobalConfig` and reads no file, the instance meta is literal (no pid, no clock, no machine path), and the counts are committed as changes rather than read from a store.

### Approved test edits

- **`src/tui/ui/golden_frames.rs`** (this commit). `WIDTH`, `HEIGHT`, `pin_theme`, `frame_snapshot` and the four base fixtures (`mail_fixture`, `calendar_fixture`, `contacts_fixture`, `drafts_fixture`) are `pub(super)`, and each of the 14 frames that mutated a fixture inline now has a `pub(super) fn …_app() -> App` builder of its own, with the test body reduced to `let mut app = <builder>(); assert_snapshot!(frame_snapshot(&mut app, WIDTH, HEIGHT));`.
  Mechanical and behaviour-preserving: every assertion, every fixture literal and every `assert_snapshot!` expression string is unchanged, so no `.snap` file moves and no snapshot metadata goes stale.
  The alternative was a second copy of 18 fixtures in the daemon module, which is the drift the sibling module exists to avoid.
  One cosmetic consequence: a frame's doc comment now sits on the builder that defines the frame rather than on the `#[test]` that captures it.

## Validation

`timeout 900 cargo test --offline --lib golden_frames` -> 20 passed, 0 failed, after the `golden_frames.rs` refactor and before the new module was registered: the extraction moves no snapshot.

The T-unit proof of a stub-free contract, in the shape Phase 4 used (`cargo test` fails to compile and the report quotes the errors, which must name exactly the contract items and nothing else).
The `daemon` cargo feature that gated the first three phases is gone since P4-U1, and this file is a `#[cfg(test)] mod` inside the library rather than a `tests/` target, so **the committed tree's `--lib` test target does not compile until P5-U2 lands**, and `cargo test --workspace` therefore fails in its build phase.
That is wider than a Phase 4 T unit's failing `tests/` target, and it is the cost the plan accepted when it recommended the sibling module.

`timeout 900 cargo test --offline --lib golden_frames_daemon --no-run`, on the tree as committed:

```
error[E0432]: unresolved import `mp_protocol::state`
  --> src/tui/ui/golden_frames_daemon.rs:93:18
error[E0599]: no associated function or constant named `from_bootstrap` found for struct `app::App` in the current scope
   --> src/tui/ui/golden_frames_daemon.rs:160:26
error: could not compile `mailypoppins` (lib test) due to 2 previous errors
```

Two errors, naming the two contract items, and nothing else.

The stub proof, in a throwaway `git worktree` at `~/.cache/mp-stub-p5u1` with `CARGO_TARGET_DIR=~/.cache/mp-stub-target`, never committed: `pub mod state` with `pub struct Bootstrap {}` (`Deserialize`) and `App::from_bootstrap` as `todo!()` make `timeout 900 cargo test --offline --lib golden_frames_daemon --no-run` compile, which is the evidence that the contract is complete and that the file needs nothing else.

The rest of the tree is green: with the `mod golden_frames_daemon;` line commented out, `timeout 1200 cargo test --workspace --offline` -> **2 075 passed, 0 failed**, and `pgrep -af '[m]p daemon'` empty afterwards.
That is #0123's 2 071 plus the four rows `744f338` added (two in `tests/daemon_send_attachments.rs`, two in `src/send.rs`); this unit adds none to a compiling tree, because its 22 are in the target that does not compile.
The line was restored before the commit.

`rustfmt --edition 2021` on `src/tui/ui/golden_frames.rs` and `src/tui/ui/golden_frames_daemon.rs`, both clean (`src/tui/ui/mod.rs` was not rustfmt-clean at `a5f93f1` and was left alone).

### The oracle was checked, not assumed

The equality oracle is only worth writing if it is reachable, so a plausible `from_bootstrap` was written in the same throwaway worktree (and discarded there) purely to run the suite: **20 passed, 2 failed**, the two failures being exactly the two unminted `…_daemon` snapshots.
All 18 equality frames, the reproducibility row and the count-sensitivity row pass against an implementation that does what the contract section above describes, so P5-U2 is not being handed an assertion no implementation can satisfy.
That implementation is not this commit's and is not proposed as P5-U2's; it exists only as the answer to "can these 18 frames be equal".

## P5-U2: initialisation via handshake and bootstrap

Three pieces, in the order a reader meets them.

`crates/mp-protocol/src/state.rs` (248 lines before its tests) is the typed decode of the whole `state.bootstrap` result: `Bootstrap`, `Snapshot`, `AccountSnapshot`, `MailboxRow`, `DraftRow`, `OutboxCounts`, and the two closed enums `AccountState` and `SyncHealthState`.
The daemon still renders the object by hand in `src/daemon/state/snapshot.rs`, which owns the state these are a projection of; this module is what every client decodes it with.

`src/tui/app/bootstrap.rs` (195 lines before its tests) is `App::from_bootstrap` and the `App::apply_bootstrap` it delegates to.
`App::new`'s ~90-field struct literal was extracted into `App::shell(global_config, accounts)` so the two constructors cannot drift; nothing else in `App::new` moved.

`src/tui/session.rs` (233 lines) is the connection: a thread of its own with a current-thread runtime on it, reached over channels.

### Decisions

**The connection is created on the thread that owns it.** `mp`'s `main` is `#[tokio::main]`, so `tui::run` is already inside a runtime and cannot block on one; `run_loop` is a synchronous paint loop and cannot become async without moving frame painting onto a worker thread. A `Connection` holds a `tokio::net::UnixStream` registered with the runtime that created it, so connecting in `main` and moving the handle would bind it to a driver that is not running. The session thread therefore calls `client_session` itself, and the UI thread talks to it with `dispatch` (post and forget) or `call` (block for the answer, which is the door P5-U3 needs).

**The session comes up before the terminal does.** `client_session` may start a daemon and may end the run with the exit-4 diagnostic, and neither reads well through a terminal already in raw mode on the alternate screen. It costs the connect and the handshake before the first paint; a cold auto-start costs its own budget on top, which is the number P5-U11 measures against `docs/baselines/pre-daemon/measurements.md` W5.

**A bootstrap never overwrites an account that already opened.** `apply_bootstrap` skips any account whose `opening` is already clear, because until P5-U4 the store-backed background open of #0003 is still what reads the real counts. A daemon in this build starts no account runtime, so every account it reports is `opening` with zeroed counts, which is exactly what `App::new` already built: a bootstrap that arrives first changes nothing visible, and one that arrives second cannot replace a real count with a zero. The `opening` -> ready transition therefore still happens on `BgResult::AccountOpened`, the same signal as today, which is what keeps the frames byte-identical.

**A wedged session is not fatal.** `Session::connect` returns `Err` only when the session thread does not come up within 30 s; every ordinary failure to reach a daemon has already exited by then. The TUI starts without a session in that case and the run's own `MAILYPOPPINS_DAEMON_REQUIRE` check fails on the way out, which is where a parity test looks.

**The event stream is connected and not consumed.** The connection is a subscriber from its first `state.bootstrap` on, and `mp_client::Connection` buffers a notification it meets while reading a reply, so nothing is mistaken for an answer. That buffer is unbounded, which is safe only because this build starts no account runtime and therefore produces no events. Draining it is P5-U7/U8's.

### The two minted snapshots

`golden_opening_account_daemon` renders the four mailbox rows with `··` in the count column and the frozen fixture's list and preview beside them: the frame a daemon-backed TUI paints first, every time.
`golden_extra_mailbox_daemon` renders five rows, the fifth being `󰉇 Projekte  0` with the extra-mailbox icon and no count of its own, and everything below the sidebar shifted by one row.
Both were inspected against the frame their test describes before they were committed.

### Follow-ups

- The unbounded notification buffer above (P5-U8).
- `mailbox_counts` takes each row's `total` unconditionally, including for an `opening` account. A real daemon reports zeros for one, so the two readings agree today; a daemon that reported counts for an account still coming up would show them behind the `··` marker rather than instead of it.
- `App::apply_bootstrap` leaves an account the snapshot does not name alone rather than removing it: dropping an `AccountState` would invalidate `active_account` mid-frame, and a configuration reload is `config.changed`'s business (P5-U8).

### Validation

`timeout 1200 cargo test --workspace --offline` -> **2 108 passed, 0 failed**, `pgrep -af '[m]p daemon'` empty afterwards.
That is P5-U1's 2 075 plus its 22 frames plus this unit's 11 unit tests (5 in `mp-protocol`, 6 in `app/bootstrap.rs`).

`cargo test --offline --lib golden_frames` -> 20, `--lib golden_frames_daemon` -> 22, both green three times over.
`git diff --stat 96c4b85..HEAD -- src/tui/ui/golden_frames*.rs tests/` is empty: no test file was edited.

`mp --help` recursive and `mp dump-keys --json` byte-identical to the Phase 0 captures, from a binary rebuilt in the same run.
`cargo clippy --workspace --offline --all-targets` -> 39 warnings, the baseline, none of them in a file this unit touched.

A manual smoke in a pty, against a sandbox root in the `tests/support/parity.rs` layout (`HOME`, `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` all pointing at one temp tree with a one-account `config.toml`): `mp` connected, painted the shell, applied the bootstrap 250 ms later (one idle poll tick, so after the first frame), rendered `Inbox ··` and `Drafts ··` at 120x40, and quit on `q` with exit 0 under `MAILYPOPPINS_DAEMON_REQUIRE=1`, the session closing before the process left.
The same run against a tree with no daemon printed the exit-4 diagnostic on a terminal still in its normal mode and exited 4.
