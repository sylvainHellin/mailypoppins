---
id: 0124
title: Phase 5 of the daemon migration, the TUI cutover
type: feature
priority: now
status: in-progress
created: 2026-09-10
---

Status: in-progress. P5-U1 to P5-U4 have landed; P5-U5 is next.

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
