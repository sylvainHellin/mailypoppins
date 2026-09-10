---
id: 0124
title: Phase 5 of the daemon migration, the TUI cutover
type: feature
priority: now
status: in-progress
created: 2026-09-10
---

Status: in-progress. P5-U1 to P5-U6 have landed; P5-U7 is next.

Seventh ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.7), after #0118, #0119, #0120, #0121, #0122 and #0123.

Phase 4 took the CLI off the direct path.
Phase 5 takes the TUI off it: the six `open_store` call sites in `src/tui/app/{mod.rs,types.rs}` become typed queries, every `Action` becomes a daemon method or a documented client-only one, the watcher threads become event subscriptions, and `src/tui/` becomes a crate that depends on `mp-client` and `mp-protocol` and on nothing else.
The gate is the parity-gate oracle suite, five oracles, of which the daemon-backed golden frames are the first to exist.

## The unit table

| unit | kind | commit | subject | status |
|---|---|---|---|---|
| P5-U1 | T | this commit | daemon-backed golden frames | done (tests) |
| P5-U2 | I | this commit | TUI initialisation via handshake + bootstrap | done |
| P5-U3 | T | this commit | the query layer contract | done (tests) |
| P5-U4 | I | this commit | the query layer | done |
| P5-U5 | T | this commit | actions to commands, the contract | done (tests) |
| P5-U6 | I | `d0c0049`, `b51b3fa`, `723ba60`, `e3055e7` | actions to commands | done |
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

## P5-U3: the query layer contract

`src/tui/app/queries_tests.rs` (1 155 lines), registered behind `#[cfg(test)]` in `src/tui/app/mod.rs`, which is this commit's only production edit.

18 tests: four on the mailbox listing and the sidebar counts, two on the preview body, two on the drafts branch, six on row deltas, one on handle hygiene, one `#[ignore]`d timing row, and two on the gate over the six `open_store` call sites (plus a test for the source scanner itself).

### The contract

Eight names, all in one new module plus one `impl`.

```text
mailypoppins::tui::queries

trait queries::Queries {                                  // object safe
    fn call(&self, method: &str, params: serde_json::Value) -> anyhow::Result<serde_json::Value>;
}
impl queries::Queries for crate::tui::session::Session    // Session::call, verbatim

queries::list_emails(&dyn Queries, account: &str, mailbox: &str) -> anyhow::Result<Vec<EmailEntry>>
queries::mailbox_counts(&dyn Queries, account: &str, mailboxes: &[MailboxInfo]) -> anyhow::Result<Vec<usize>>
queries::message_body(&dyn Queries, account: &str, msg: MessageRef) -> anyhow::Result<Option<String>>

queries::MessageRowDelta: Debug                           // variants are P5-U4's
queries::MessageRowDelta::decode(&mp_protocol::EventEnvelope) -> Option<MessageRowDelta>
queries::apply_row_delta(&mut Vec<EmailEntry>, mailbox: &str, &MessageRowDelta) -> bool
```

**A trait plus free functions rather than three methods on `Session`.**
`Session::call` already has exactly the signature `Queries::call` declares, so `impl Queries for Session` is one line and no second connect path is invented; P5-U2 wrote that method as "the door the query layer (P5-U3/U4) will use" and this is it.
What the trait buys is a query layer testable without a socket: the file drives it over an in-process `Dispatcher`, the same fixture pattern P5-U1 established, so the contract is pinned against the daemon's real method bodies rather than against a hand-written JSON mock that could agree with nobody.

**Typed in the TUI's vocabulary, not in a wire row.** `EmailEntry`, `MailboxInfo`, `MessageRef` are what the six call sites need, and they are what make the oracle the strongest available: the daemon-backed answer is compared field for field against the answer `open_store` produces today, over one seeded store, in one process.
`EmailEntry` is not `PartialEq` and deriving it would be a production edit a T unit may not make, so rows are compared by their `Debug` rendering, which covers every field the list and the headers pane paint.

**`apply_row_delta` returns "nothing is owed".** `true` means the delta was folded into the held list, `false` means the caller must re-issue `message.list`.
A delta about another mailbox folds as a no-op and returns `true`: the sidebar's other three mailboxes move constantly, and refetching the open list on each of them would put back exactly the per-event whole-list transfer `docs/baselines/decisions/list-transfer.md` chose deltas to avoid.

**The delta wire shapes are the daemon's own, not the decision's prose.** `message.row` for a replace, and `state.remove` / `state.invalidate` (`src/daemon/state/events.rs`, `KIND_REMOVE` and `KIND_INVALIDATE`) for the other two, with the `message:<account>/<mailbox>/<uid>` and `mailbox:<account>/<mailbox>` resource strings the mutation methods and the count changes already publish.
The decision writes the remove as `message(account, row_id)`; the tree writes it by `(mailbox, uid)`, and following the tree keeps P5-U4 out of the mutation methods.

### What P5-U4 has to decide, and this file deliberately does not

Four gaps between what the wire carries and what an `EmailEntry` holds. The tests fail until each is closed, whichever way it is closed.

- **The row id.** `entry_from_row` puts `MessageRef(messages.id)` on every entry and it is the identity the selection set, the preview memo and every mutation hold (#0050). `message.list`'s row carries `uid` and `message_id` and no `messages.id`.
- **Six columns.** `to` (which Sent and Drafts display instead of `from`), `cc` / `reply_to` / `bcc` (#0096), `flagged` (#0007, left off the wire by P2-U10 on purpose) and `is_invite` (#0038). The named-field encoding the decision keeps makes adding them additive.
- **Addressing `message.get`.** It takes `id` as `"<mailbox>/<uid>"` or a selector; the preview path holds a `MessageRef`.
- **The drafts branch.** `mp_protocol::draft::DraftEntry` carries no `date` and no `cc`, both of which `entry_from_draft` reads.

### The gate, and the three sites that stay

`the_query_layer_replaced_every_open_store_it_could` is a source scan over the production code of `src/tui/app/{mod.rs,types.rs}` (line comments and `#[cfg(test)]` modules stripped), attributing every `open_store(` call to its enclosing function and comparing the set against `TUI_APP_STORE_RESIDUE`, a three-row table with a reason per row, in the shape and the intent of `tests/architecture_boundaries.rs`'s `CLI_ENGINE_RESIDUE`. It fails in both directions: a site still there belongs behind a query, a site gone belongs struck from the table in the same commit.

The three that stay are `mod.rs`'s `load_calendar_events`, `load_message_invite` and `load_message_ics`. None has a daemon method to move to: `calendar.rebuild` is an operation over the store rather than a query that answers the invite rows the Calendar view lists, the invite card folds the account's REPLY rows over one payload (`reconcile::*`), and no `message.*` method hands out an attachment blob inline. Contracting one is neither this unit's brief nor P5-U4's.

It lives in this module rather than in `tests/architecture_boundaries.rs` because it only becomes true when P5-U4 lands, and the plan requires `cargo test --workspace` to be green on a T unit's commit. This module is the target that does not compile, so a failing gate inside it costs the rest of the tree nothing; in `tests/architecture_boundaries.rs`, which compiles today, the same gate would turn the workspace run red.

### The timing row

`the_preview_query_stays_inside_the_p95_delta_ceiling`, `#[ignore]`d, run with `cargo test --offline --lib queries_tests -- --ignored`.
It walks 200 rows down and back up on each path and asserts the daemon-backed p95 exceeds the store-backed p95 by at most **5 ms**, the W1 budget `docs/baselines/pre-daemon/workloads.md` fixes ("the same budget the transport decision (P1a-U2) is held to").

Ignored for two reasons, both worth stating. It needs a 200-row fixture and a wall clock, which is a flake waiting for a loaded CI box next to rows that run in milliseconds on three messages. And it is a lower bound on W1 rather than W1: it prices the store read, the row conversion and the dispatch, and not the socket, the framing or the session thread's hop, and W1 itself is still `NOT TAKEN` in `docs/baselines/pre-daemon/measurements.md` (no account, no terminal on that host), so there is no recorded p95 for an absolute to be compared against. P5-U11 owns the real rerun.

### Handle hygiene

`a_preview_walk_leaves_no_handle_behind`. `message.get` answers inline: `docs/baselines/decisions/large-payloads.md` puts the threshold at 1 MiB and the three handle methods are the only minters in the daemon, so the preview owes no release when the cursor moves off. The test counts the directories under `<data>/runtime/handles/` after a walk down the list and back up, which is zero either way, whether because nothing was minted or because everything was released. That makes it the release contract the day a preview is routed through a minter instead.

### Approved test edits

None. No existing test file was touched.

### Validation

The T-unit proof of a stub-free contract, in the shape P5-U1 used: the file is a `#[cfg(test)] mod` inside the library rather than a `tests/` target, so the committed tree's `--lib` test target does not compile until P5-U4 lands.

`timeout 900 cargo test --offline --lib queries_tests --no-run`, on the tree as committed:

```
error[E0432]: unresolved import `crate::tui::queries`
   --> src/tui/app/queries_tests.rs:132:17
    |
132 | use crate::tui::queries::{
    |                 ^^^^^^^ could not find `queries` in `tui`
error: could not compile `mailypoppins` (lib test) due to 1 previous error
```

One error, naming the one contract item that is a path: the module. The other seven names are inside it, so the stub proof rather than the error text is what says the contract is complete.

The stub proof, in a throwaway `git worktree` at `~/.cache/mp-stub-p5u3` with `CARGO_TARGET_DIR=~/.cache/mp-stub-target`, never committed: a `src/tui/queries.rs` with the trait, the `impl` for `Session` and five `todo!()` bodies makes `cargo test --offline --lib queries_tests --no-run` compile, which is the evidence that the file needs nothing else.

The rest of the tree is green: with the two `mod queries_tests;` lines commented out, `timeout 1200 cargo test --workspace --offline` -> **2 108 passed, 0 failed**, and `pgrep -af '[m]p daemon'` empty afterwards. That is P5-U2's 2 108 unchanged; this unit adds none to a compiling tree, because its 18 are in the target that does not compile. The lines were restored before the commit.

`rustfmt --edition 2021` on `src/tui/app/queries_tests.rs`, clean (`src/tui/app/mod.rs` was not rustfmt-clean before this unit and was left alone).

### The oracle was checked, not assumed

An equality oracle is only worth writing if it is reachable, so a plausible query layer was written in the same throwaway worktree, and discarded there, purely to run the suite: **17 passed, 1 failed**, the failure being `the_query_layer_replaced_every_open_store_it_could`, which only a change to the six call sites can satisfy and which the plausible implementation deliberately did not make. The `#[ignore]`d timing row passed too, at 200 rows.

Closing the four gaps above cost, in that worktree: eight fields added to `message::to_json` (`id`, `mailbox`, `to`, `cc`, `reply_to`, `bcc`, `flagged`, `is_invite`), a `row_id` address on `message.get`, and `date` plus `cc` on `mp_protocol::draft::DraftEntry`. That is one plausible shape and is not proposed as P5-U4's; it exists only as the answer to "can these 18 rows be satisfied".

## P5-U4: the query layer

`src/tui/queries.rs` (502 lines) is the module the contract names, and `src/tui/app/store_rows.rs` (124 lines) is where the store-backed readers it replaced went. The rest is the call sites: `src/tui/app/mod.rs`, `src/tui/app/types.rs`, `src/tui/actions.rs`, `src/tui/mod.rs`, `src/tui/bg.rs`, `src/tui/session.rs`, and the four wire gaps in `src/daemon/methods/{message,draft}.rs` and `crates/mp-protocol/src/draft.rs`. 626 lines of new file plus about 130 of changed line, excluding tests.

### The four wire gaps, all closed additively

No field was renamed, none was dropped, and no command's output moved: `mp --help`, `mp dump-keys --json` and every parity row of the read, draft, mutation, send, sync and admin slices are byte-identical.

**A `message.list` row gained seven fields.** `id` (`messages.id`), the five columns a list and a header pane render and a CLI listing does not (`to`, `cc`, `reply_to`, `bcc`, `is_invite`), and `flags.flagged`, the store's fourth axis, which the row's own doc comment had been promising to "the version that adds it" since P2-U10.

`flagged` went **inside `flags`** rather than beside it, because the four axes are one thing and the doc sentence that deferred it was about that member. `cc`, `reply_to` and `bcc` travel **nullable** where `from`, `to`, `subject` and `date_display` stay flattened to `""`: the header pane prints each of the three only when the message carried one, so a flattened `""` would make an absent Cc indistinguishable from an empty one and the two paths would render differently. `mailbox` was **not** added, unlike the throwaway worktree's shape: the delta events carry the mailbox in their payload and `message.get` is addressed by row id, so nothing needed it.

**`message.get` gained `row_id`** as a third address, still exactly one of the three. The preview holds a `MessageRef` and nothing else (#0050); addressing by `"<mailbox>/<uid>"` would have made the client carry a second identity for every row and re-derive it on every cursor move.

**`DraftEntry` gained `cc` and `date`**, the index's own columns, which `entry_from_draft` reads. The client-side fallback the T unit offered instead (the filename stem through `resolve_date`) is what happens when `date` is absent and is not a substitute for it: a draft whose frontmatter *has* a `date:` would otherwise sort and display differently through the daemon than through the store, and nothing client-side can recover a `cc:` at all.

### The approved test edits, and why a protocol change costs them

Three pinned key sets moved, which is exactly what `tests/daemon_protocol_fixtures.rs` says a protocol change must cost: "a field silently added, renamed or dropped on the wire fails this test instead of regenerating a fixture that agrees with the new code and with nothing else. Changing one of these lists is a protocol change and needs a changelog entry." The changelog entry is in `docs/daemon-protocol.md`.

- `tests/daemon_read_only_methods.rs`: the live-row key list, its `flags` key list, `expected_message` and the module's shape sketch (P2-U10's pins).
- `tests/daemon_protocol_fixtures.rs`: `MESSAGE_ROW` and `MESSAGE_FLAGS`, plus `crates/mp-protocol/fixtures/message.list.response.json`, which now shows a populated `cc`, a `reply_to` and a flagged row so the fixture documents the nullable-versus-flattened split rather than only asserting it.
- `tests/daemon_draft_slice.rs`: `ENTRY_FIELDS` (8 -> 10) and the shape sketch.
- `tests/fixtures/tui-engine-imports.txt`: two rows, re-recorded through the documented `UPDATE_TUI_ENGINE_IMPORTS=1`. `app/store_rows.rs uses store` is the move of an import `app/types.rs` and `app/mod.rs` already had; `queries.rs uses store` is a genuine widening, and it is types only (`MessageRow`, `DraftRow`, `SkippedDraft`), which is what P5-U10 has to resolve when the TUI becomes a crate that cannot link the store.

No golden frame and no line of `queries_tests.rs` was touched.

### Decisions

**A wire row becomes a `MessageRow` and goes through `entry_from_row`.** The two paths are then equal by construction rather than by inspection: every derivation (the display name, the `(no subject)` fallback, `resolve_date`'s two strings, the flag axes) stays in the one function that owns it, and a change to it moves both paths together. The drafts branch does the same through `entry_from_draft` and `entry_from_skip`. The two `MessageRow` fields a listing does not carry, `body_blob` and `thread_id`, are the two an `EmailEntry` does not read. `src/main.rs` has a `row_from_wire` of its own for the CLI listing and it was left alone: it decodes what `mp list-messages` prints and nothing more, and unifying the two is P5-U10's when they are in one crate.

**The uid index.** A held list is keyed by `messages.id` and the daemon removes a row by `message:<account>/<mailbox>/<uid>`, which is the resource its mutation methods already invalidate and which the T unit chose to follow rather than change. Nothing in an `EmailEntry` is a uid, so the correspondence is remembered in the query layer: a process-wide `(account, mailbox) -> (uid -> id)` table that every listing replaces wholesale for its own mailbox and every row replace adds one entry to. A uid the table does not know owes a refetch rather than a guess, which is what `apply_row_delta` returns `false` for. The alternative, a `uid` field on `EmailEntry`, is 31 struct literals across seven files, two of them the frozen golden-frame fixtures, and it is worth revisiting when the TUI moves to its own crate.

**Two call sites keep their thread, one keeps the draw thread.** `Session::handle()` is new: a clone of the call channel that a worker thread can own, since the `App` owns the `Session` and the UI thread owns the `App`. The background mailbox load (`Action::LoadMailbox`) and the two-phase startup's per-account count both keep the `std::thread::spawn` they always had and block on a call instead of on a store open, so nothing moved onto the draw thread. The preview body is the one synchronous read, one `message.get` per cursor move behind the memo that already made an unchanged selection free, which is the shape `docs/plans/preview-latency.md` budgets. `recount_all_mailboxes` is synchronous as it always was: it follows a sync, not a keystroke.

**A refused preview is an empty pane, not an error.** `queries::message_body` logs and answers `None` for any refusal, which is what the store-backed path does with a stale reference *and* with a store it could not open. Its `Result` is therefore unreachable today; it is kept because the day a preview is routed through a minter (`docs/baselines/decisions/large-payloads.md`) a transport failure and a missing row stop being the same thing.

**The store-backed readers were kept, in a file of their own.** `load_emails`, `count_all_emails` and `App::load_message_body` are still here, in `src/tui/app/store_rows.rs`, for two callers and no third: they are the equality oracle `queries_tests.rs` compares every answer against, and they are what an `App` with no session reads, which is every one of the ~370 TUI unit tests and, in a real run, only a `Session::connect` that wedged. They are in that file rather than in the two the gate scans because the gate is a scan for `open_store(` in `src/tui/app/{mod.rs,types.rs}`; the move is recorded here rather than left to be discovered. No paint path reaches them when a session exists, and P5-U10, which moves `src/tui/` into a crate that may not link the store, is where they die.

That fallback is not the "direct fallback" P5-U8 forbids: nothing recovers a *failed* daemon call by reading the store, and a failed call degrades exactly as it did before (an empty list, zeroed counts, an empty preview, and a line in the log).

**A `state.invalidate` scoped to the counts is not a row delta.** The daemon publishes mailbox count changes as `state.invalidate` over `mailbox:<account>/<slug>` with `{"query": "counts"}`, and decoding those as list invalidations would refetch the open list on every count change, which is the per-event whole-list transfer the deltas exist to avoid.

### The delta consumer

P5-U2 left the event stream connected and drained by nobody, so there was no place to apply a delta to. `tui::bg::apply_row_delta` is that place: it folds one `MessageRowDelta` into the open list and its cache slot (they share an allocation), or reloads the mailbox through the same off-thread path a switch takes when the delta owes a refetch. It is called by nothing and carries `#[cfg_attr(not(test), allow(dead_code))]` with two unit tests over it; **P5-U8 turns it on** when it drains the stream, and the unbounded notification buffer P5-U2 recorded is the same unit's.

### Follow-ups

- `queries.rs uses store` in the TUI engine-import allow-list, and `src/main.rs`'s duplicate wire-row decoder, both for P5-U10.
- The uid index above, likewise: a `uid` on `EmailEntry` is the shape that would delete it.
- `message.get` computes the attachment list and the selector for every preview, because the result is `ShownMessage` whole. It is inside the budget (the timing row measures it) and is one query more than the pre-daemon read did.
- An empty stored body reads as `Some("")` through the store and `None` through `message.get`, which `shown_message` filters. The preview shows an empty pane either way, so no frame moves; a shape that cares would need `message.get` to stop filtering.

### Validation

`timeout 1200 cargo test --workspace --offline` -> **2 128 passed, 0 failed, 1 ignored**, `pgrep -af '[m]p daemon'` empty afterwards. That is P5-U3's 2 108 plus the 18 rows of `queries_tests.rs`, which compile and pass for the first time, plus the two delta rows in `src/tui/bg.rs`.

`cargo test --offline --lib queries_tests` -> 18 passed, three times over; the `#[ignore]`d timing row separately with `-- --ignored` -> 1 passed, at 200 rows, inside the 5 ms W1 delta ceiling.

`--lib 'ui::golden_frames::'` -> 20, `--lib golden_frames_daemon` -> 22, both unmoved and no snapshot re-approved. (`--lib golden_frames` matches both modules and reports 42.)

Every `daemon_*_slice` suite green: read 22, draft 34, mutation 35, send 50, sync 38, admin 44.

`git diff --stat aaf8ccd..HEAD -- src/tui/app/queries_tests.rs src/tui/ui/golden_frames*.rs` empty. Under `tests/`, the four files above, each with its reason.

`mp --help` recursive and `mp dump-keys --json` byte-identical to the Phase 0 captures, from a binary rebuilt in the same run. `cargo clippy --workspace --offline --all-targets` -> 39 warnings, the baseline.

A pty smoke against the `examples/mkfixture` fixture (two accounts, 541 messages, a 1 MiB body), `script -qec "stty rows 40 cols 120; mp"` under `MAILYPOPPINS_DAEMON_REQUIRE=1` against a daemon started beside it: the shell painted with `··` in every count column, then the daemon-backed list painted 18 rows of `alpha/inbox` with their dates and subjects, `j` moved the cursor, the headers pane filled from the row and the body pane from `message.get`, and `q` left with exit 0 and no daemon behind it. Started *beside* it rather than on demand because `MAILYPOPPINS_DAEMON_REQUIRE=1` is inherited by the auto-started `mp daemon run`, which is on the no-daemon list and refuses to start under it (worth knowing before the P5-U9 harness meets it).

`rustfmt --edition 2021` on the two new files and on the five touched files that were rustfmt-clean at `aaf8ccd`; the six that were not (`src/tui/{mod,bg,actions}.rs`, `src/tui/app/{mod,types}.rs`, `tests/daemon_draft_slice.rs`) were left alone. `cargo install --path . --offline` green.

## P5-U5: the action-to-command contract

`src/tui/actions_tests.rs` (1 175 lines), registered behind `#[cfg(test)]` in `src/tui/mod.rs`, which is this commit's only production edit.

22 tests: five on the classification table, ten on what a daemon-routed mutation issues, three on the invariants the unit preserves, and four on the residue gate (plus a test for the source scanner itself).

It lives beside the event loop rather than under `app/` for one reason: `MAX_COALESCED_EVENTS` and `COALESCE_BUDGET` are private constants of `src/tui/mod.rs`, and a child module sees its parent's private items, so the drain's bounds are asserted on the values rather than on a copy of them.

### The contract

Three names, all in one new module.

```text
mailypoppins::tui::commands

enum commands::ActionRoute: Debug + PartialEq {
    Daemon(&'static [&'static str]),  // the methods this action issues, in issue order
    ClientOnly(&'static str),         // the reason: editor, browser, clipboard, picker, terminal
    Local,                            // pure UI state; nothing leaves the process
}
fn commands::route(action: &Action) -> ActionRoute            // exhaustive, no wildcard arm

fn commands::dispatch(
    app: &mut App,
    commands: &dyn crate::tui::queries::Queries,
    action: &Action,
) -> bool
```

**`Queries` is the door, and there is no second trait.** `Session::call` is the only way to the daemon and `Queries` is already the object-safe wrapper over it that P5-U3 contracted and P5-U4 built, for both `Session` and `QueryHandle`. A `Commands` trait with the identical method would be a second name for one thing and a second place to implement it.

**`dispatch` returns a `bool` and takes the door as an argument.** `true` means the action was daemon-routed and is handled; `false` means `handle_action` still owns it. A refusal from the daemon is not a `false`: it lands on the status line exactly as a refused store mutation does today, which is why nothing returns a `Result` the caller would have to invent a second presentation for. The door is an argument rather than read off `app.session`, because `dispatch` needs `&mut App` and a borrow of the session inside it would collide; `Session::handle()` already exists for exactly that.

**A route names a slice of methods, not one method.** `Action::Delete` is one key over two kinds of row (`message.delete` for received mail, `draft.discard` for a draft and for a parse-skipped draft file, #0073/#0080) and `Action::ServerSearch` is the local pass then the server leg. A single-method route would have made the table lie about both.

### The table

`ACTION_ROUTING` is `(variant, route, suspends_terminal)` for all fifty variants, in the enum's declaration order. Completeness is compile-time: a `variant_name` match with **no wildcard arm** is where a new `Action` fails to build, and the runtime rows then fail until it has a route and a `suspends_terminal` value.

The third column is the one existing tests do not give. `src/tui/app/types.rs` has `suspending_actions_are_all_flagged` and `ordinary_actions_do_not_suspend_the_terminal`; each lists a subset, so neither fails when a *new* variant is classified wrongly. Neither was touched.

Thirty of the fifty are `Daemon`, fifteen `ClientOnly` and five `Local`. `every_daemon_route_names_a_registered_method` asks a live `Dispatcher` for its `specs()` rather than listing the `MethodSpec` arrays here, so a family that grows is covered without this file moving, and a method declared in an array but never registered still fails. It fails on the tree as committed for the three the parity matrix names and nothing serves: **`message.set_read` (MSG-03), `message.set_flag` (MSG-04) and `message.move` (MSG-05)**.

### What the mutation rows pin, and the two decisions behind them

Each row asserts three things about one action: the **method**, the **parameters** it resolved, and the **effect** the daemon's own method body had on the store. The third is what makes the first two more than a spelling check: not "the TUI said `message.archive`" but "the row is in `archive` and the `ServerOp::Move` it owes is queued", read back through `crate::store::read` and `crate::pending_ops`.

**A TUI mutation may not wait for the server.** `message.archive` and `message.delete` (P4-U8) resolve the backend before the store is touched and then drain the owed op synchronously, which is what gives `mp archive` its blocking UX. The TUI has never done either: it commits the row change and the owed op in one transaction and lets the next sync tick drain it (`src/tui/mutations.rs`, #0039), which is why `u` over a thousand-message selection costs no network and why the mark-read of an explicit open cannot stall a frame. Every row asserts the mutation **succeeded over an account with no credentials**, which is reachable only if the call queued rather than settled. How that is spelled is P5-U6's: a `settle: false` parameter defaulting to `true` so `mp archive` does not move, a separate durability, or a client-scoped variant. No row asserts the parameter's name.

**Addressing is by `row_id`.** The address P5-U4 added to `message.get` for exactly this reason: the TUI holds a `MessageRef` and nothing else (#0050), and `"<mailbox>/<uid>"` would make it carry a second identity for every row. `address` (`src/daemon/methods/message.rs`) already accepts it, so the two existing mutations need no change on this axis.

**A batch is one call per message, in selection order.** `MSG-06` sketches "batch forms"; this file pins the plain form instead, because today a reference to a row that is gone is skipped with a log line while the rest of the selection proceeds (`mutations::message_id_of`), which one call per row gives for free and a plural address would have to re-invent as a partial-failure shape. A plural address is worth taking the day a selection's round trips show up in a measurement.

#0110 gets two rows. `mark_as_read_addresses_the_row_the_open_resolved` moves the cursor between the resolution and the dispatch, which is what the #0108 coalescing does when a `Tab` and a `J` land in one batch, and asserts the command addressed the carried `MessageRef`. `a_cursor_move_issues_no_command` asserts three `j` presses queue no mark and call nobody: a daemon-backed TUI makes the #0087 failure mode a write per keystroke, so it is pinned as a call count and not only as an absent `Action`. The queueing half stays where it is, in `src/tui/app/keys.rs`, untouched.

### What `dispatch` does not handle

The actions whose route names an **operation**-kind method (`sync.quick`, `sync.full`, `send.draft`, `send.approved`, `calendar.rsvp`, `message.search`, `message.list_server`) keep the arm they have in `handle_action`: each already owns a `std::thread::spawn` and a `BgResult` reply channel, and an operation answers `{operation_id}` at once and finishes later, so its arm has to wait somewhere and post a result. That wait is P5-U8's to turn into an event subscription. Their contract is the routing table plus the residue gate: the table says which method they issue, the scan says they no longer reach `crate::sync`, `crate::send` or `imap_client` to do it. There is no third way to pin them, because `handle_action` takes a `Terminal<CrosstermBackend<Stdout>>` and no test can build one.

### The invariants, and why one of them is a source scan

`the_drain_bounds_are_unchanged` reads `MAX_COALESCED_EVENTS` and `COALESCE_BUDGET` themselves. `the_pre_draw_drain_still_stops_on_its_four_clauses` is a scan over `run_loop`'s drain block asserting the four clauses in evaluation order (`!app.running`, the batch cap, the wall-clock budget, a queued `suspends_terminal` action) and that `poll_pending_event` follows them, because the drain being incremental is load-bearing. `a_quit_clears_running_so_the_drain_stops` is the behavioural half of clause 1, which is what keeps a queued `e` after `q` from dispatching an editor on the way out.

A whole-drain test is not available at any price: the loop owns a Crossterm terminal on the real stdout. Extracting the stop condition into a predicate was considered and dropped, because the invariant here is *unchanged* and a scan that fails when the drain is edited is a faithful pin of exactly that.

### The residue gate

`TUI_ACTION_ENGINE_RESIDUE` is `(file, function, needle, reason)` over the production code of `src/tui/{actions,mutations}.rs`, in the shape and intent of `CLI_ENGINE_RESIDUE` and `TUI_APP_STORE_RESIDUE`, with a **needle** column those two do not have: `handle_action` is a thousand lines, and a whole-function exemption there would exempt forty arms, so what a row permits is one symbol in one function. Eleven needles, each a call and not an import (`tests/architecture_boundaries.rs` already gates the TUI's imports file by file, and what it cannot say is which function still opens a store).

Thirty-three call sites today against the eight the table names, so twenty-five have to go. The eight that stay:

| function | needle | why |
|---|---|---|
| `readonly_view_for_row` | `store_for_mutation(` | the read-only Markdown rendition (#0075, RD-06); nothing registered renders a stored message as Markdown |
| `handle_search_result_action` | `store_for_mutation(` | the same rendition under another caller |
| `handle_action` | `store_for_mutation(` | the `OpenEventSource` arm's `invite.ics` blob, the read `app/mod.rs` already keeps as `load_message_ics` |
| `store_for_mutation` | `open_store(` | the helper those three share; it dies with the last of them |
| `selected_selector` | `open_store(` | the `mp://` selector of the cursor row (RD-07): no listing carries one |
| `ingest_search_hit` | `open_store(` | ingesting a server-only hit; LST-09's `message.fetch` is not built |
| `fetch_search_hit` | `imap_client::` | the raw fetch by Message-ID behind that ingest |
| `send_one_draft` | `send_draft(` | the undo-send hold's fire path (#0090, SND-04), which the plan holds in the TUI until P6-U1/U2 |

Every one of the eight was verified to exist today, so the table's "removed" direction is not carrying a guess.

`src/tui/mutations.rs` has no caller left once the five message mutations are routed, and its four `queue_*` functions are what the three new methods need. The scan treats a missing file as a file with no residue, so deleting it is not a reason to edit this table; moving it into the daemon rather than deleting it keeps its eight unit tests, which are the only assertions anywhere that a flag change queues exactly one `ServerOp` of the right shape.

### Approved test edits

None. No existing test file was touched.

### Validation

The T-unit proof of a stub-free contract, in the shape P5-U1 and P5-U3 used: the file is a `#[cfg(test)] mod` inside the library rather than a `tests/` target, so the committed tree's `--lib` test target does not compile until P5-U6 lands.

`timeout 900 cargo test --offline --lib actions_tests --no-run`, on the tree as committed:

```
error[E0432]: unresolved import `crate::tui::commands`
   --> src/tui/actions_tests.rs:156:17
    |
156 | use crate::tui::commands::{dispatch, route, ActionRoute};
    |                 ^^^^^^^^ could not find `commands` in `tui`
error: could not compile `mailypoppins` (lib test) due to 1 previous error
```

One error, naming the one contract item that is a path: the module. The other two names are inside it, so the stub proof rather than the error text is what says the contract is complete.

The stub proof, in a throwaway `git worktree` at `~/.cache/mp-stub-p5u5` with `CARGO_TARGET_DIR=~/.cache/mp-stub-target`, never committed: a `src/tui/commands.rs` with the enum, `route` and `dispatch` as `todo!()` makes `cargo test --offline --lib actions_tests --no-run` compile, which is the evidence that the file needs nothing else. (`~/.cache/mp-stub-p5u3` was removed to make room.)

The rest of the tree is green: with the two `mod actions_tests;` lines commented out, `timeout 1200 cargo test --workspace --offline` -> **2 128 passed, 0 failed**, and `pgrep -af '[m]p daemon'` empty afterwards. That is P5-U4's 2 128 unchanged; this unit adds none to a compiling tree, because its 22 are in the target that does not compile. The lines were restored before the commit.

`rustfmt --edition 2021` on `src/tui/actions_tests.rs`, clean (`src/tui/mod.rs` was not rustfmt-clean before this unit and was left alone).

### The oracle was checked, not assumed

A plausible implementation was written in the same throwaway worktree, and discarded there, purely to run the suite: **21 passed, 1 failed**, the failure being `the_actions_that_could_be_routed_were`, which only a change to the twenty-five call sites can satisfy and which the plausible implementation deliberately did not make.

It cost, in that worktree: `route` as a copy of the table, a `dispatch` over the fifteen command-kind actions, three new `MethodSpec` entries (`message.move`, `message.set_flag`, `message.set_read`) served by the same `MessageMutationMethod`, and a `settle` parameter whose `false` branch calls `tui::mutations::queue_*` instead of `pending_ops` plus a synchronous drain. That is one plausible shape and is not proposed as P5-U6's; it exists only as the answer to "can these rows be satisfied".

**One finding is worth carrying forward**: the data-root override of #0077 is *thread-local*, and the message mutations hop to `spawn_blocking` (a `Store` is not `Sync` and the commit and the owed op are one unit), so the blocking thread resolved `store_path` against the developer's own tree and every mutation refused with `-32006`. The fixture's runtime therefore installs the override on every thread it starts (`on_thread_start`, the guard forgotten because the thread dies with the runtime). Any in-process daemon fixture that reaches a command rather than a query needs the same three lines; it is in `docs/lessons-learned.md`.

### Follow-ups

- The three unbuilt renditions behind the residue table (`RD-06`'s Markdown rendition, `LST-09`'s `message.fetch`, `RD-07`'s selector) are what stands between this gate and the zero P5-U10 needs when `src/tui/` becomes a crate that cannot link the store.
- A plural address for the batch mutations, if a selection's round trips ever show up in a measurement.
- `src/tui/mutations.rs`'s eight unit tests, which follow its four functions wherever P5-U6 puts them.

## P5-U6: actions to commands (part 1, the mutations)

`src/tui/commands.rs` (699 lines, 434 before its tests) is the module the contract names.
`src/mutations.rs` (175 lines before its tests) is `src/tui/mutations.rs` moved out of the TUI, and `src/daemon/methods/message.rs` grew the three methods and the `settle` parameter (+269 / -67).
`src/tui/actions.rs` lost 447 lines: the fifteen arms, the five mutation helpers, the four draft helpers and the two refresh helpers all moved.

**This unit is not finished.** 21 of the 22 rows of `src/tui/actions_tests.rs` pass; `the_actions_that_could_be_routed_were` does not, and the ten sites it names are part 2's. They are listed under "What is left" below.

### The three methods, and why they are not in `MESSAGE_MUTATION_METHOD_SPECS`

`message.move`, `message.set_flag` and `message.set_read` are `MESSAGE_QUEUE_METHOD_SPECS`, an array of their own.

`tests/daemon_mutation_slice.rs` holds `const _: () = assert!(MESSAGE_MUTATION_METHOD_SPECS.len() == MUTATION_METHODS.len());` over a two-name list, checked while the tree compiles.
That array is P4-U8's slice, the assertion says what that slice declares, and growing it would have edited a pinned test to say something it was not written to say.
`MESSAGE_SERVER_METHOD_SPECS` is the in-tree precedent, and its own doc gives the same reason ("that array is pinned at three").
The split is a Rust-side fact: one `MessageMutationMethod` serves all five, `register_mutations` chains the two arrays, and the wire, the capability list and the dispatcher see five methods in one family.

### Decisions

**`settle`, defaulting to `true`.** The T unit left the spelling open between a parameter, a separate durability and a client-scoped variant. A parameter is the one that leaves `mp archive` byte-identical: every existing caller sends nothing and gets the settled outcome, and the interactive contract is one key. With `settle: false` the daemon commits the row change and the owed server op in one transaction and resolves **no credential at all**, which is what lets the TUI mutate an account whose password is not in the keyring yet, exactly as the store-backed path did.

**All five mutations go through `crate::mutations::queue_*`.** The daemon had a second copy of the pairing (`pending_ops::apply_move` plus a hand-built `ServerOp`) beside the TUI's; now there is one, and `Queued {row_id, op_id}` is what a settling caller hands to `run_and_settle`. The four functions are keyed by `messages.id` rather than by `MessageRef`, so the module sits below both clients, and its eight unit tests came with it (two of them adapted: `refs` is a list of ids and the two `assert_eq!(moved, vec![MessageRef::new(id)])` compare row ids).

**The server folder a queued op names is the row's, not the open mailbox's.** `find_server_name_for_role(account, &row.mailbox)`, which is what `message.archive` has done since P4-U8; the TUI used `App::active_server_mailbox()`, which is the *open* mailbox and is wrong for a batch that spans mailboxes. For an account with no `[[mailboxes]]` mapping the role name is the folder name, so a default fixture now queues `inbox` where the TUI queued `INBOX`; a configured account is unchanged, and this is the CLI's behaviour already.

**"Already approved" is read off the row the user is looking at.** `draft.approve` answers with the status it wrote, not the status it found, so the library sentence the TUI branched on does not cross the socket. The list column is the same source the frame renders, so the line cannot disagree with the screen it is printed under; a second press before the reload lands says "Approved" twice instead of "Already approved", which is the one wording this unit moves and the reason it is recorded here.

**A refusal is a log line and a skip, and the "nothing to …" lines stay.** The store-backed path skipped a row that was gone with a log line and let the rest of the selection proceed (`mutations::message_id_of`); a daemon refusal does the same. A selection where every call refused prints the line it printed before ("Archive failed: nothing to archive" and its two neighbours) with the daemon's sentences in the log, rather than promoting one refusal to the status line and moving the wording.

**The destination check moved to the daemon.** `Action::MoveToMailbox` refused a mailbox whose sidebar row carried no `server_name`; the daemon resolves the folder from the account's mapping itself, so the client-side refusal (and its status line) is gone. It was unreachable for every configured account and refused moves the daemon can name a folder for.

### Approved test edits

- `src/daemon/session.rs`'s `the_advertised_capabilities_are_the_lifecycle_methods_and_the_table`: three names added to the literal list, in method-name order. The list is the assertion; a method served and not advertised is what it exists to catch.
- `tests/fixtures/tui-engine-imports.txt`: three rows gone, re-recorded through the documented `UPDATE_TUI_ENGINE_IMPORTS=1`, because `src/tui/mutations.rs` is not in `src/tui/` any more.
- `src/tui/actions.rs`'s own test module: five tests moved to `src/tui/commands.rs` with the code they test (the #0110 mark, the invite-rebuild predicate and the three draft flips), against an in-process daemon fixture instead of a bare store. `approve_and_mark_draft_flip_the_indexed_status` restores its row with the status the flip left, which is where "Already approved" now comes from.
- No line of `src/tui/actions_tests.rs`, `src/tui/app/queries_tests.rs` or the golden frames was touched.

### What is left (part 2)

The residue gate names ten sites, each needing a method call where a store call is:

| site | needle | what it needs |
|---|---|---|
| the `use super::helpers` line | `lib_do_sync` | drops with the three sync arms |
| `handle_action` (`Fetch`, `FetchAccount`, `Sync`) | `lib_do_sync` | `sync.quick` / `sync.full`, then `operation.status` polled off the draw thread |
| `handle_action` (`SendApproved`) | `send_draft(` | `send.approved`, the same wait |
| `handle_action` (`Rsvp`) | `send_rsvp(` | `calendar.rsvp`, the same wait |
| `handle_action` (`ServerSearch`, the local pass) | `open_store(` | `message.search` with `body: true` |
| `source_for_msg` | `store_for_mutation(` | `draft.reply` / `draft.forward`, **which need a `row_id` address** on their `source`: they take `"<mailbox>/<uid>"` or a selector, and the TUI holds a `MessageRef` |
| `forward_subject` | `open_store(` | `message.get` with `body: false` |
| `lookup_draft_path` | `Store::open(` | `draft.path` |
| `row_attachment_files` | `store_for_mutation(` | `message.get` for the part list, then `message.materialise_attachment` per part; the files land in the daemon's handle directory instead of the per-row materialisation directory |
| `html_rendition_for_row` | `store_for_mutation(` | `message.materialise_html` |

Two things part 2 has to settle that part 1 did not have to:

**An operation's answer.** `sync.*`, `send.approved` and `calendar.rsvp` answer `{operation_id}` and finish later, and the finish is a `state.event` the session thread does not read (P5-U2 left the stream connected and drained by nobody). `operation.status` is a registered query and the registry prunes nothing, so a worker thread can poll it to a terminal state and post the `BgResult` its arm already posts; P5-U8 replaces the poll with the subscription. Nothing else in the tree awaits an operation without events.

**About fifteen of `src/tui/actions.rs`'s 39 remaining unit tests** exercise those helpers directly and will need the in-process daemon fixture `src/tui/commands.rs` now has.

### Validation

`timeout 1200 cargo test --workspace --offline --no-fail-fast` -> **1 303 lib tests pass, 1 fails** (`the_actions_that_could_be_routed_were`, above), and every `tests/` suite is green, `daemon_mutation_slice` (41), `daemon_draft_slice` (34), `daemon_read_only_methods` (22), `daemon_send_slice` (50), `daemon_sync_slice` (38), `daemon_admin_slice` (44) and `architecture_boundaries` (6) among them. `pgrep -af '[m]p daemon'` empty afterwards.

`cargo test --offline --lib actions_tests` -> 21 passed, 1 failed, three times over, the same row each time.
`--lib 'ui::golden_frames::'` -> 20 and `--lib golden_frames_daemon` -> 22, unmoved, no snapshot re-approved. `--lib queries_tests` -> 18.

`cargo clippy --workspace --offline --all-targets` -> 33 warnings, none of them in `src/tui/commands.rs`, `src/mutations.rs` or any line this unit wrote. (The 39-warning baseline the earlier units quote is not comparable while the lib test target does not compile: at `17b1b65` clippy stops before it lints the test code and reports 23. **Closed in part 2**: with the lib test target compiling again the count is back to **39**, the baseline every unit before this one quoted, and the 33 was never a real reduction.)

`rustfmt --edition 2021` on `src/tui/commands.rs`, `src/daemon/methods/message.rs`, `src/daemon/session.rs` and `src/tui/session.rs`, all four rustfmt-clean at `17b1b65`; `src/lib.rs`, `src/tui/mod.rs`, `src/tui/actions.rs` and `src/mutations.rs` were not and were left alone.

Not run, because they gate the finished unit rather than this half: the help walk, `mp dump-keys --json`, the pty smoke and `cargo install --path .`.

## P5-U6: actions to commands (part 2, the ten residue sites)

`the_actions_that_could_be_routed_were` passes: eighteen call sites became eight, which are the eight rows of `TUI_ACTION_ENGINE_RESIDUE` and nothing else.
`cargo test --offline --lib actions_tests` is **22/22**.

Three commits.
`b51b3fa` is the four additive surfaces the sites needed; `723ba60` is the ten sites; `e3055e7` is the weak session handle a pty smoke found, and three tests over the machinery.

`src/tui/actions.rs` lost 468 lines and gained 309 (4 114 now).
`src/tui/commands.rs` gained 468 (1 573, 700 before its tests).
`src/tui/helpers.rs` lost 227: the store-backed sync path had no caller left.
`src/search.rs` gained 143, `src/tui/test_daemon.rs` is new at 150, and the three daemon families gained 178 between them.

### The four additive surfaces, and the one that was not needed

**`search::to_query_string`, the inverse of `search::parse`.**
The overlay holds a parsed `Query` and `message.search` takes what a user typed, so something had to bridge them.
Rendering the AST back into the grammar is the option that puts nothing new on the wire at all; the alternative was `#[derive(Serialize)]` on `Term`, `Clause` and `Query` and a second address on the method, which would have put an engine enum into the protocol that `docs/daemon-protocol.md` says a client must be able to speak without linking the engine.
Every value is rendered quoted, which is always safe (a token that begins with a quote is a literal phrase, and `field:"value"` is the field form the tokenizer reads), and the round trip is pinned over every shape the grammar can express plus a flag-built query.
The two shapes it cannot carry are the two `parse` cannot produce and are dropped rather than mangled: a field term with an empty value, which `parse` refuses outright, and a value containing a `"`, which the tokenizer consumes as a delimiter.

**`row_id` on `draft.reply` / `draft.forward`'s `source`,** in the shape P5-U4 gave `message.get`, and **`row_id` on `calendar.rsvp`** beside its selector.
Both for the same reason (#0050): the list row and the agenda row carry a `messages.id` and nothing else, and re-deriving a `"<mailbox>/<uid>"` or a selector for each would make the client hold a second identity per row.
`tests/daemon_draft_slice.rs` pins the *result* key sets and the ten method names; it pins no param set, and `draft.*` runs no `only_params`, so the addition cost no test edit.
`calendar.rsvp` does run `only_params` and its allow-list grew by one name, which is the check working.

**`headers` on `draft.reply` / `draft.forward`.**
The compose wizard's forward collects the recipients and the subject *before* the draft is written and keeps them over the ones the builder derived (`create_draft_from_source`'s `headers` argument, which the daemon was passing `None`).
Without it that one flow would have had to build the draft through the daemon and then rewrite the file behind its back, with the index refreshed twice and the daemon's answer already stale.
All four fields are required once the object is present and an empty string clears one, which is `DraftRecipientEdit`'s own contract; a partial object is `-32602`, because which fields an override covers is not a thing to leave ambiguous.

**`new_inbox_mail` on the two sync passes' result.**
`BgResult::Fetch` carries the inbox arrivals the desktop notification reads (#0009) and `SyncCompleted` does not, so without this the notification would have died silently.
It is a sibling of `outcome` rather than a member: the outcome is the payload published to *every* client as a `sync.completed` event, and a per-message list of who wrote to you belongs only in the answer to the client that asked for the pass.
A blocked pass and a pass through an account runtime's tick both report `[]`, the runtime's tick because it keeps no arrival list; that is P5-U8's to fix when the notification moves onto the event stream.

No new method was needed, so the `MESSAGE_QUEUE_METHOD_SPECS` pattern part 1 approved was not used a second time.

### The two decisions the unit was given, and what they cost

**An operation is polled.**
`sync.quick`, `sync.full`, `send.approved` and `calendar.rsvp` answer `{operation_id}` and finish later; the finish is a `state.event` the session thread does not read (P5-U2 left the stream connected and drained by nobody).
Each arm keeps the `std::thread::spawn` and the `BgResult` channel it already had, and the thread reads `operation.status` every 100 ms until the state is terminal, then posts the result its arm posted before.
100 ms is a tenth of the TUI's own idle tick, so a fast operation is reported within a frame and a minute-long sync costs six hundred round trips over a Unix socket, which is nothing beside the pass.
The wait is unbounded, because the wait the arm replaced was unbounded: `rt.block_on(lib_do_sync(…))` took as long as the mailbox did, and there was no timeout on it to inherit.
Each individual call is bounded by `Session`'s own 30 s `CALL_TIMEOUT`, which is ample for a method that answers an id at once and for a status read.
**P5-U8 replaces the poll with the `operation.finished` subscription** `mp sync` already uses.

**`draft.reply` / `draft.forward` gained `row_id` additively** and `tests/daemon_draft_slice.rs` did not have to move, so the stop-and-report condition never fired.

### The status lines, word for word

`mp_client::format::sync_status_line` is `tui::helpers::finish_sync` plus its mutation-drain suffix, clause for clause and in the same order; its doc comment has said "the TUI's one-line status message for a finished tick" since P4-U10, and this is the caller it was written for.
A blocked pass is the `SYNC_SKIPPED_MARKER` sentence the same helper returned (#0122).
`send.approved` renders `{sent} sent, {failed} failed` off the settled `ApprovedOutcome`, and an account with neither is the "No approved emails found" the empty scan printed.
`calendar.rsvp` renders `{subject} — replied to {organizer}`, and a reply that reached nobody is `Failed to send RSVP to {organizer}`, which is the `any_succeeded` check the arm made on the outcome it had in hand.

Three client-side checks stayed ahead of their call, each because its sentence belongs to the key and not to `mp`:
"IMAP not configured" on the two sync keys, "RSVP is not supported for Graph accounts yet (#0036)" and "SMTP not configured" on the RSVP key (the daemon's Graph refusal is worded for `mp invite`), and `resolve_send_transport`'s missing-transport line on `cX`, which also decides whether the progress line says "via Graph".
The RSVP's fourth check, "That message carries no invitation to reply to", is `App::load_message_ics`, which is one of the three reads `TUI_APP_STORE_RESIDUE` keeps in `app/mod.rs`; it now reads the payload and throws it away, because the daemon loads its own.

### Decisions this unit had to take

**The client-side transport fork is gone from the sync arms.**
The daemon's pass body loads the Graph or the IMAP configuration itself, so `sync.quick` covers both transports and the three arms lost their `if app.is_graph()` halves.
What is left of the fork is the progress line, which still says "(Graph)" because that is what the user reads while it runs.
`tui::helpers`' `lib_do_sync`, `lib_do_sync_graph`, `finish_sync`, `drain_queues`, `drain_pending_ops` and `SyncResultMeta` then had no caller and were deleted rather than kept as an oracle: unlike the store-backed readers of `app/store_rows.rs`, nothing compares against them.

**A sync failure reads slightly differently.**
`BgResult::Fetch`'s `Err` was `lib_do_sync`'s error rendered with `to_string()`; it is now the daemon's, rendered with `{e:#}`, so a failure whose cause chain has more than one link prints the whole chain where it printed the head.
`bg.rs` prefixes it identically (`Fetch failed{name}: {e}`) and no green line moved.

**`lookup_draft_path`'s two outcomes became one.**
The store-backed helper distinguished "not in the index" (`Ok(None)`) from "the index could not be read" (`Err`), and the TUI printed a different line for each.
`draft.path` answers `-32602` for both, so the second line is gone and its reason is in the log.
The first line is the one a user can act on; the second named a failure mode (an unreadable drafts index) that a daemon serving the same directory would have reported through every other draft method first.

**Attachments and the browser rendition land in the daemon's handle directory.**
`message.materialise_attachment` writes one directory per handle under `<data>/runtime/handles/`, where the store-backed helper wrote every part of a row into `parse::materialisation_dir(<row id>)`.
Both are 0700 and both hand the picker a file; what changed is a lifetime, ten minutes by default, where a materialised copy lived until the temp directory was swept.
Neither is released, for the reason `ATT-01` already gave for `mp open`: the viewer that was just launched is holding the file.

**A search hit that resolved to no local row is the one draft the daemon cannot build.**
`draft.reply` and `draft.forward` read a store row, and the hit has none; its content is the fetch the overlay is rendering.
`write_fetched_draft_and_edit` is that branch, and it is the only remaining caller of `create_draft_from_source` in the TUI.
It reaches no engine module, so the residue gate does not name it, and `LST-09`'s `message.fetch` is what would delete it.

**`DFT-11`'s recipient rewrite stayed client-side.**
`ce` resolves its file through `draft.path` now and then writes the frontmatter itself, because `draft.set_recipients` is not built and building it was not this unit's brief.
It reaches no engine module either; what it costs is a second drafts-index refresh the daemon could have done in one.

### The weak session handle

The pty smoke found it: `q` painted and the process never left.
`Session::close` drops its sender and then **joins** the session thread, whose loop ends when the last sender goes, and `Session::handle()` handed out a strong clone.
A mailbox load holds one for milliseconds; a sync arm polling `operation.status` holds one for as long as the pass takes, so quitting waited for the sync.
`QueryHandle` holds a `WeakUnboundedSender` now and upgrades it per call, so quitting closes the channel under every worker and their next call fails with the closed-session error the door already had.
It is also what the method's own doc has claimed since P5-U4: *"it keeps no session alive (a call on a handle whose session has closed fails like any other)"*.

### The in-process daemon fixture, and the ten tests carried onto it

`src/tui/test_daemon.rs` (150 lines) is the fixture P5-U1 established, extracted the moment a second module needed it, and `Session::serving` puts it behind a **real session**: a thread of its own answering over the same call channel, so an `App` reaches it through `app.session` and a `QueryHandle` exactly as it reaches a daemon over a socket.
That is what let ten `actions.rs` tests be carried rather than rewritten: the routed helpers read their door off `app.session`, so a fixture that hands them one tests the whole path, status lines included, instead of testing the layer function underneath.

Its runtime is **multi-threaded**, which part 1's was not: an operation's work is a `tokio::spawn`ed task, a current-thread runtime only drives its tasks while something is inside `block_on`, and a caller polling `operation.status` never is, so the first operation a test drove sat in `running` for ever. It is in `docs/lessons-learned.md`.

The ten, all of them still in `src/tui/actions.rs` and all of them now asserting against a daemon instead of a store:

| test | what it needed |
|---|---|
| `the_cursor_row_materialises_its_blobs_into_daemon_handles` | renamed from `…_where_mp_open_puts_them`; the directory moved, so the assertion did |
| `a_row_without_attachments_resolves_to_an_empty_list` | a session |
| `saving_the_same_attachment_twice_keeps_both_copies` | a session |
| `the_browser_gets_the_html_blob_written_to_a_file` | a session |
| `the_browser_rendition_inlines_cid_images_as_data_uris` | a session |
| `a_draft_answers_the_attachment_key_from_its_own_frontmatter` | a session, for the `draft.path` under `cursor_draft` |
| `a_draft_without_attachments_resolves_to_an_empty_list` | the same |
| `a_missing_draft_attachment_is_named_on_the_status_line` | the same |
| `edit_current_resolves_the_cursor_draft_through_the_index` | the same |
| `edit_recipients_finds_the_draft_through_the_index` | the same |

None was deleted, because none of them was pinned elsewhere: `actions_tests.rs` pins which method an action issues, not what the helper under it does with the answer.
Two more that were already passing are stronger for the fixture and were given one anyway: `a_message_without_html_says_so_instead_of_opening_an_empty_page` was passing because an `App` with no session refuses exactly as a row with no markup does, which is a false pass, and `an_unresolved_search_hit_is_served_from_the_fetch` shares the module's builder.

Three tests were added to `src/tui/commands.rs`, over the machinery that has no `actions.rs` caller a test can reach:
`an_operation_is_polled_to_the_state_it_settled_in` (a real `calendar.rebuild` from `{operation_id}` to its settled result),
`a_refused_sync_is_the_sentence_the_daemon_gave` (a local-only account, whose refusal is what lands on the status line),
and `the_local_pass_finds_the_row_the_index_holds` (the overlay's AST rendered, sent, re-parsed daemon-side and run against the FTS index, with the body travelling on the hit).

### The final residue

Eight rows, unchanged from the table P5-U5 wrote, and the reason each is still there is unchanged too:

| function | needle | why |
|---|---|---|
| `readonly_view_for_row` | `store_for_mutation(` | the read-only Markdown rendition (#0075, `RD-06`); nothing registered renders a stored message as Markdown |
| `handle_search_result_action` | `store_for_mutation(` | the same rendition under another caller |
| `handle_action` | `store_for_mutation(` | the `OpenEventSource` arm's `invite.ics` blob; no `message.*` method hands out an attachment blob inline |
| `store_for_mutation` | `open_store(` | the helper those three share; it dies with the last of them |
| `selected_selector` | `open_store(` | the `mp://` selector of the cursor row (`RD-07`): no listing carries one |
| `ingest_search_hit` | `open_store(` | ingesting a server-only hit; `LST-09`'s `message.fetch` is not built |
| `fetch_search_hit` | `imap_client::` | the raw fetch by Message-ID behind that ingest |
| `send_one_draft` | `send_draft(` | the undo-send hold's fire path (#0090, `SND-04`), held in the TUI until P6-U1/U2 |

Three surfaces and one plan decision stand between this and the zero P5-U10 needs.

### Approved test edits

- **`src/tui/actions.rs`'s own test modules**: the ten rows above, plus `app_on_row` and `app_on_draft` taking the fixture so they can give the `App` a session. No assertion was weakened; the one that changed (`the_cursor_row_materialises_its_blobs_into_daemon_handles`) asserts a stronger property than the path it replaced, since it also pins one directory per handle.
- **`src/tui/commands.rs`'s test module**: its local `Daemon` now delegates to `TestDaemon` rather than building a second dispatcher, and three tests were added.
- No line of `src/tui/actions_tests.rs`, `src/tui/app/queries_tests.rs`, the golden frames or anything under `tests/` was touched: `git diff --stat 3dfb184..HEAD -- src/tui/actions_tests.rs src/tui/app/queries_tests.rs src/tui/ui/golden_frames*.rs tests/` is empty.

### Follow-ups

- **P5-U8 replaces the `operation.status` poll** with the `operation.finished` subscription, and with it `new_inbox_mail`'s place in the answer: an arrival list belongs on the event stream, and the account runtime's tick reports none today.
- The server leg of `LST-08` is still `lib_do_multi_search` on a background thread. It reaches no engine *needle* the gate scans (it takes `app.imap_config`, already loaded), so the gate does not name it, and `message.list_server` is the method it becomes.
- The handle lifetime the attachment and browser keys now inherit: ten minutes, where a materialised copy used to live until the temp directory was swept.
- `DFT-11`'s client-side recipient rewrite, and the second index refresh it costs, for `draft.set_recipients`.
- `src/tui/app/store_rows.rs` is now the last store-backed reader in the TUI and its only caller is a wedged session. P5-U10 is where it dies.

### Validation

`timeout 1200 cargo test --workspace --offline` -> **2 156 passed, 0 failed, 1 ignored**, `pgrep -af '[m]p daemon'` empty afterwards.
That is 1 310 lib tests (part 1's 1 303 passing plus the row that was failing, plus three query-string round trips and three command-layer rows) and every `tests/` suite green.

`cargo test --offline --lib actions_tests` -> **22 passed**, three times over.
`--lib 'ui::golden_frames::'` -> 20 and `--lib golden_frames_daemon` -> 22, unmoved, no snapshot re-approved. `--lib queries_tests` -> 18 (plus the `#[ignore]`d timing row).
Every `daemon_*_slice` suite green: read 22, draft 34, mutation 35, send 50, sync 38, admin 44.

`git diff --stat 3dfb184..HEAD -- src/tui/actions_tests.rs src/tui/app/queries_tests.rs src/tui/ui/golden_frames*.rs tests/` is empty.

`mp --help` recursive and `mp dump-keys --json` byte-identical to the Phase 0 captures, from a binary rebuilt in the same run.
`cargo clippy --workspace --offline --all-targets` -> **39 warnings**, the baseline every unit before part 1 quoted, none of them on a line this unit wrote. Part 1's 33 is closed: it was clippy stopping short of a test target that did not compile.

`cargo install --path . --offline` green.

A pty smoke against an `examples/mkfixture` root (`script -qec "stty rows 40 cols 120; mp"`, 120x40, `HOME`, `MAILYPOPPINS_DATA_DIR` and `MAILYPOPPINS_CONFIG_DIR` all inside the fixture):
the shell painted with `··` in every count column, the bootstrap filled the sidebar (`Inbox 201`, `Drafts 0`, `Sent 50`, `Archive 100`, `Bulk 500`), `a` opened the archive confirm and `y` archived the row with `Email archived` on the activity line and the row gone from the list, `ss` parked behind the startup auto-fetch with the `Quick sync queued (waiting for the current sync)` line and then ran, printing `Quick sync (alpha)...` and settling on `Synced: 0 new, 0 existing`, and `q` left with exit 0.
The sync leg went through `sync.quick` and `operation.status` end to end.
It did not *fail*, which is the one thing the smoke could not produce: the fixture's account has no IMAP block, so the pre-daemon check `app.imap_config.is_none()` short-circuits before the call (the `IMAP not configured` line, verbatim), and an account given a bogus host plus a stored password reaches the server, fails per mailbox and still settles a clean pass, which is `sync_mailboxes`' behaviour before and after.
The refusal path is pinned by `a_refused_sync_is_the_sentence_the_daemon_gave` instead, over a local-only account, which is the branch whose sentence reaches the status line.

The first smoke is also what found the weak-handle bug: `q` painted and the process sat there until the pty's own timeout, because the sync worker's strong `QueryHandle` was keeping the session thread that `Session::close` joins alive.
