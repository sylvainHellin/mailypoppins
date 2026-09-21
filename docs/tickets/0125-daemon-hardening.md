---
id: 0125
title: Phase 6 of the daemon migration, hardening and service integration
type: feature
priority: now
status: open
created: 2026-09-21
---

Status: open. P6-U1 and P6-U2 have landed the daemon-owned hold and P6-U3 has pinned the graceful-shutdown contract; P6-U4 and the rest of the phase have not started.

Eighth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.8), after #0118, #0119, #0120, #0121, #0122, #0123 and #0124.

Phase 5 left the TUI daemon-backed and the parity gate green.
Phase 6 makes the daemon something that can be left running: the undo-send hold moves out of the last client that owns a piece of domain behaviour, shutdown becomes graceful, the daemon starts at login through the platform's own service manager, and it can be asked how it is doing.

## The unit table

| unit | kind | commit | subject | status |
|---|---|---|---|---|
| P6-U1 | T | this commit | daemon-owned undo-send hold, the contract | done (tests) |
| P6-U2 | I | this commit | daemon-owned undo-send hold (SND-04) | done |
| P6-U3 | T | this commit | graceful shutdown, the contract | done (tests) |
| P6-U4 | I | - | graceful shutdown | not started |
| P6-U5 | T | - | login-start service units (LIF-06), the contract | not started |
| P6-U6 | I | - | login-start service units | not started |
| P6-U7 | T | - | diagnostics, the contract | not started |
| P6-U8 | I | - | `diagnostic.health`, `diagnostic.logs`, `diagnostic.support_bundle` | not started |
| P6-U9 | I | - | soak tests | not started |
| P6-U10 | I | - | benchmarks and docs | not started |

The phase exit gate for the hold, verbatim from the plan: *"The daemon-owned hold reproduces the behaviour the parity gate recorded, and the last client exiting mid-hold cancels the hold and leaves the draft approved."*
The second half of that sentence is P6-U3/P6-U4's and is already asserted from the outside by `tests/phase5_undo_send_hold.rs`; the first half is P6-U1/P6-U2's and is the two files below.

## P6-U1: the daemon-owned undo-send hold, the contract

Two test files, no production code.

- `src/tui/hold_tests.rs` (16 tests), a `#[cfg(test)] mod` of `src/tui/mod.rs`, in the crate because the status line, the `u` key, `App` and the residue are all in-crate.
- `tests/daemon_send_hold.rs` (7 tests), over a real `mp daemon run` and a real socket, because "two clients see one countdown" and "the window that did not send cancels it" cannot be said in one process.

The plan names `tests/daemon_hold.rs` in the Phase 6 gate table; the file is `tests/daemon_send_hold.rs`, which is the name the send family's other socket-level file already uses (`tests/daemon_send_slice.rs`, `tests/daemon_send_attachments.rs`).

### The contract

Nine names, and nothing else.
P6-U2 supplies them; the stub proof below is the evidence that no tenth is missing.

```text
mp_protocol::send::HoldStatus {                 // Clone + Debug + PartialEq + Eq + Serde
    operation_id: String,   // the id `send.draft` / `send.approved` answered with
    account: String,
    draft_id: String,       // the `id:` of the draft that is waiting
    subject: String,        // its subject, empty when it has none
    hold_secs: u64,         // the window it was armed with
    remaining_secs: u64,    // what is left of it, 0 once the hold fired or was cancelled
    fires_at: String,       // RFC3339 UTC, so a late client renders a deadline
    origin: String,         // the kind word of the client that asked: "tui", "cli", "gui"
}
mp_protocol::send::HoldListing { holds: Vec<HoldStatus> }   // `send.hold_status`'s result

mp_protocol::events::KIND_SEND_HOLD_STARTED   == "send.hold_started"
mp_protocol::events::KIND_SEND_HOLD_TICK      == "send.hold_tick"
mp_protocol::events::KIND_SEND_HOLD_FIRED     == "send.hold_fired"
mp_protocol::events::KIND_SEND_HOLD_CANCELLED == "send.hold_cancelled"
    // all four are `state.event` notifications whose payload is one `HoldStatus`

daemon::methods::send::SEND_METHOD_SPECS gains, in method-name order:
    MethodSpec::new("send.cancel_hold", MethodKind::Command, 1)
    MethodSpec::new("send.hold_status", MethodKind::Query,   1)

tui::app::Action::CancelHeldSend                    // a new variant, non-suspending
tui::app::App::hold: Option<mp_protocol::send::HoldStatus>
tui::events::Applied::Hold(String)                  // the operation_id the event concerned
```

On the wire, and not a Rust name: `send.draft` and `send.approved` gain an optional `hold: bool` parameter, defaulting to `false`.

- `send.hold_status` `{account?}` answers `{holds: [HoldStatus, …]}`, so a client that joined after the countdown started renders the same object the event carries.
- `send.cancel_hold` `{operation_id}` answers `{cancelled: true, operation_id}`, from **any** connection, and leaves the draft `approved` with its file on disk.
- A send that armed a hold says so in its own answer: `{operation_id, held: true, …}`. A send that armed none does not carry `held: true`, which is what the zero-window row asserts.
- An `operation_id` that names no live hold is `-32602`, and so is one whose hold has already fired: a cancel is not a recall.

### Why `hold` is a boolean and not a number of seconds

The plan moves the hold *into the daemon*, so the daemon resolves `email.send_hold_secs` from the configuration it already owns and already serves through `config.get`.
A client that read the file itself and passed a number would leave the policy where it is today, under a longer name.

Two consequences, and both are wanted.

The CLI bypasses the hold by construction: `mp send` and `mp send-approved` pass no `hold` at all, so `ANO-7` stays true without a line of client code.
So does every other caller that exists: `tests/daemon_send_slice.rs` calls both methods directly over a socket, and a default of "hold for twenty seconds" would have made that suite wait out a window nobody asked for.
`send_hold_secs = 0` stays the opt-out, resolved daemon-side: `hold: true` against a zero window fires at once, publishes no `send.hold_*` event and leaves `send.hold_status` empty.

### The presentation, pinned word for word

`SND-04` says the migration preserves the status-line presentation, and that presentation is three sentences living in exactly one place each today.
They are asserted as literals so the move cannot quietly reword them:

| event | line | level |
|---|---|---|
| `send.hold_started`, `send.hold_tick` | `Sending in {remaining_secs}s (press u to undo)` | Progress |
| `send.hold_fired` | `Sending...` | Progress |
| `send.hold_cancelled` | `Send cancelled; the draft is untouched` | Info |

The one thing that changes is the number.
Today the line is written once at arm time and shows the whole window for its whole life, because nothing in the TUI counts down; with a per-second event the same sentence carries the daemon's remainder, and the first line a client sees (`hold_secs == remaining_secs`) is byte-identical to today's.
The client never subtracts: a countdown computed from a local clock drifts against the daemon that owns the timer, and two windows would then disagree about one hold.

There is **no golden frame of the countdown** to compare against and this unit mints none: `rg 'press u to undo' src/tui/ui/` finds nothing, the hold has never been captured in `src/tui/ui/golden_frames*.rs`, and a frame whose status line is a literal three tests already assert would pin the same string twice at the cost of a snapshot review.

### The `u` key

It stays hand-dispatched ahead of the KEYMAP table, exactly as #0090 wrote it, so it neither collides with the Message-context `u` (toggle read) once the window has fired nor needs a website key-table entry.
What changes is its body: clearing a local slot becomes `push_action_dedup(Action::CancelHeldSend)`, because cancelling is a round trip now and `dispatch_normal_mode` has no door.
The countdown stays on screen until the daemon says it is cancelled; a client that cleared it optimistically would hide a hold whose cancel was refused.

Quitting mid-hold stops being refused.
`Message::Quit` refuses today and says why - *"A send is holding"* - because `app.held_send` dies with the window.
Once the daemon owns the timer that reason is gone, and the plan states the replacement rule: *"When the last client exits mid-hold the daemon cancels the hold and leaves the draft approved."*
A TUI that still refused to quit would make that rule unreachable.

### How the Phase 5 oracle stays green

`tests/phase5_undo_send_hold.rs` (P5-U9, oracle (e)) approves a draft over a real connection, drops the connection, waits past the configured window and then asks a fresh connection what the daemon did: the draft must still be `approved`, the outbox must hold no new row and the fake transport's ledger must be empty.

It **arms no hold**: it calls `draft.approve` and never `send.draft`, so it stays green through P6-U2 whatever the scheduler does, and it stays green for the right reason afterwards.
Its own header says so in advance: *"Today the answer is 'it never had a hold to fire'; after P6-U2 it will be 'it had one and cancelled it'."*
The reading this unit takes, and the one the plan's own words fix, is that the daemon cancels a hold when the **last** client exits, which is P6-U3/P6-U4's rule and needs no reconciliation of the P5 suite: nothing in it has to change.

What P6-U2 must not do is cancel a hold because *the client that armed it* went away.
That would make `send.*`'s `CancelScope::Durable` a lie and lose a send the user confirmed, and it is pinned here by `the_sender_may_close_its_window_while_another_client_watches`: A sends with a hold, A's connection drops, B is still connected, and the hold fires.

### Pre-approved edits to other test files

A T unit's file is the implementer's to pass, not to edit, and these are the edits P6-U2 is allowed to make elsewhere.
Each is mechanical and each is forced by a name in the contract above.

- **`src/tui/actions_tests.rs`, three edits.**
  1. `variant_name` gains `Action::CancelHeldSend => "CancelHeldSend",`. The match is exhaustive with no wildcard arm by design, so a new variant does not compile until it is named.
  2. `ACTION_ROUTING` gains the row `("CancelHeldSend", ActionRoute::Daemon(&["send.cancel_hold"]), false)`, in the table's existing order. `every_action_variant_is_in_the_routing_table` is exhaustive by construction and fails without it.
  3. `TUI_ACTION_ENGINE_RESIDUE` **loses** the `("src/tui/actions.rs", "send_one_draft", "send_draft(", …)` row, and only that row. `the_actions_that_could_be_routed_were` asserts equality in both directions, so a site that went away belongs struck from the table in the same commit. The table shrinks from eight entries to seven.
- **`tests/daemon_send_slice.rs`, two edits.**
  1. `SEND_METHODS` grows from six names to eight, keeping method-name order, and the `const _: () = assert!(SEND_METHOD_SPECS.len() == SEND_METHODS.len())` tripwire follows it.
  2. `the_cli_send_paths_take_no_hold` currently asserts that `hold`, `hold_secs` and `countdown` are all refused on all three sends. `hold` becomes a real parameter of `send.draft` and `send.approved`, so the row keeps `hold_secs` and `countdown` on all three and keeps `hold` refused on `send.invite` only. Its point is unchanged and is asserted elsewhere too: what the CLI must not do is *pass* one, which `a_routed_send_delivers_without_waiting_out_a_hold` and this unit's `the_cli_send_approved_still_bypasses_the_hold` both measure against the clock.
- **`src/daemon/methods/send.rs`'s module header**, whose "The hold is not here" paragraph says none of these methods takes a `hold` and a caller that sends one is refused. That paragraph is the thing this unit changes; it has to be rewritten rather than left as a false statement about its own file.
- **`docs/daemon-protocol.md`**: the `send.*` paragraph (eight methods, not six), the event-kind list, and a changelog entry. The protocol changelog is what a second implementation reads, so a new kind that is not in it is not in the protocol.

Nothing else in `tests/` or in another unit's test file may move.

### Validation

`src/tui/hold_tests.rs` is a `#[cfg(test)] mod` inside the library rather than a `tests/` target, so, exactly as P5-U1 recorded, **the committed tree's `--lib` test target does not compile until P6-U2 lands** and `cargo test --workspace` therefore fails in its build phase. That is the cost the sibling-module arrangement was accepted for, and it is why the rest of the tree is verified with the `mod hold_tests;` line commented out.

`TMPDIR=/var/tmp cargo test --offline --lib hold_tests --no-run`, on the tree as committed: 19 errors, in five kinds, naming the nine contract items and nothing else.

```
error[E0432]: unresolved imports `mp_protocol::events::KIND_SEND_HOLD_CANCELLED`,
  `mp_protocol::events::KIND_SEND_HOLD_FIRED`, `mp_protocol::events::KIND_SEND_HOLD_STARTED`,
  `mp_protocol::events::KIND_SEND_HOLD_TICK`
error[E0432]: unresolved imports `mp_protocol::send::HoldListing`, `mp_protocol::send::HoldStatus`
error[E0599]: no variant, associated function, or constant named `CancelHeldSend` found for enum
  `app::types::Action` in the current scope
error[E0599]: no variant, associated function, or constant named `Hold` found for enum `Applied`
  in the current scope
error[E0609]: no field `hold` on type `app::App`
```

`tests/daemon_send_hold.rs` compiles today - it names the two new methods as strings on the wire - and fails at runtime, seven of seven, naming the same contract and nothing else:

```
the hold's query and its cancel are part of the send family: ["send.approved", "send.draft",
  "send.invite", "send.outbox_discard", "send.outbox_list", "send.outbox_retry"]
send.draft accepts a hold: Rpc(RpcError { code: -32602, message: "send.draft has no hold
  parameter; it takes account, id, selector" })
send.hold_status answers: Rpc(RpcError { code: -32601, message: "unknown method send.hold_status" })
assertion `left == right` failed: the caller named something that is not there
  left: -32601    right: -32602        // `send.cancel_hold` is not registered either
```

The stub proof, in a throwaway `git worktree` at `~/.cache/mp-stub-p6u1` with `CARGO_TARGET_DIR=~/.cache/mp-stub-target`, never committed: the two protocol types, the four kind constants, `Applied::Hold`, `App::hold` and `Action::CancelHeldSend` as empty declarations - plus the three mechanical exhaustiveness arms the new variant forces (`handle_action`, `variant_name`, `ACTION_ROUTING`) - make `cargo test --offline --lib hold_tests` **compile**, which is the evidence that the contract is complete and the file needs nothing else.
It then runs 16 tests: **6 passed, 10 failed**, the six being the ones a declaration can satisfy (the wire vocabulary, the route table, and the three rows that assert nothing happens when there is no hold) and the ten being every row that needs behaviour. A stub that passed more than six would mean a vacuous row.

The rest of the tree, with `mod hold_tests;` commented out and `tests/daemon_send_hold.rs` moved aside: `TMPDIR=/var/tmp cargo test --workspace --offline` -> **2204 passed**, the count at `55305f6`, both lines restored before the commit.

## P6-U2: the daemon-owned undo-send hold

The hold left `src/tui/actions.rs` and became `src/daemon/hold.rs`, 330 lines including four unit tests.
`src/tui/hold_tests.rs` is green, sixteen of sixteen; `tests/phase5_undo_send_hold.rs` is green for the reason P6-U1 predicted, and the reading it took is the one implemented.

### The scheduler

A held send is an operation with a deadline.
`send.draft` and `send.approved` answer `{operation_id, held: true}` at once, `HoldScheduler::arm` publishes `send.hold_started`, a tokio task publishes one `send.hold_tick` a second, and at the deadline it publishes `send.hold_fired` and awaits the very future the unheld path would have spawned.
So there is one send path and not two: `run(work, handle)` is the same value either way, passed to `hold::run_held` instead of to `tokio::spawn`.

Three decisions the contract did not settle.

- **The countdown is driven off the deadline, not off a repeating one-second sleep.**
  Each wake computes what is left and sleeps until the instant the remainder drops, so a loaded machine cannot accumulate drift between the last tick and the fire.
  The remainder is rounded *up*, so the last fraction of a window reads as `1s` rather than `0s`.
- **One hold ends exactly once, and the table is what decides it.**
  `fire` and `cancel` both *remove* the row under one mutex, so the cancel that arrives while the timer is waking either wins (and the timer finds nothing and returns) or loses (and refuses with `-32602`).
  That is why the timer task carries no cancellation token: there is nothing a token would say that the table does not.
- **A cancel settles the operation as `cancelled`.**
  `operation.status` then agrees with the `send.hold_cancelled` event, and a GUI watching the operation is not left waiting for work that will never run.
  The TUI drops its own `Awaited` entry when the cancel event lands, so the `operation.finished` behind it is ignored rather than posting an error line over "Send cancelled; the draft is untouched".

### The last-client rule, landed here rather than in P6-U3

`handle_connection` calls `HoldScheduler::cancel_all` once the unsubscribe has brought `CanonicalState::subscriber_count()` to zero.
P6-U3 and P6-U4 refine what a graceful shutdown does around it; this is the rule that keeps `tests/phase5_undo_send_hold.rs` meaning the same thing after the move, so it lands now.
It is not a cancel scope, and `the_sender_may_close_its_window_while_another_client_watches` is the row that keeps it from becoming one.

### The TUI, which now only renders

`App::hold` replaces `App::held_send`; `HeldSend`, `fire_held_send`, `send_one_draft` and `send_status_line` are gone, and with them the last `use crate::send::…` in the action layer, so `tests/fixtures/tui-engine-imports.txt` lost its `actions.rs send` row (10 pairs over 8 files now) and `TUI_ACTION_ENGINE_RESIDUE` lost its `send_one_draft -> send_draft(` row (seven entries now).
`u` pushes `Action::CancelHeldSend` and leaves the countdown on screen; `Message::Quit` stops refusing.

Two consequences worth naming.

- **`Action::Send` is `send.draft` now**, which is what its `ACTION_ROUTING` row always claimed.
  The approve and the transport refusal stay in the key (they are its sentences), and the account is still the draft's own `from:` through `helpers::resolve_send_account`.
  A draft whose `from:` names an account *other* than the one whose drafts directory holds the file is therefore refused by `send.draft` (`-32602`, "no draft …") where the in-process path used to send it through the other account's SMTP.
  That cross-account case was already inconsistent - the file stayed here and the outbox row went there - and `mp send <selector>` has never supported it; it is now a visible refusal rather than a silent split.
- **The status line of a finished send moved to `commands::sent_line`**, rendered from the `SendOutcome` the daemon settles with.
  The three sentences are unchanged to the byte and are pinned by `the_send_lines_are_the_ones_the_send_key_has_always_shown`.
- **`bg_count` rises when the hold is armed**, not when it fires, so the spinner runs for the length of the window. No golden frame covers the countdown, so nothing moved.

### `send.approved` and the batch

The batch's hold names the first approved draft in listing order, because that is the one a countdown is about, and an account with *no* approved draft arms no window at all: "No approved emails found" is not a sentence worth waiting twenty seconds for.

### Test edits made

The five P6-U1 pre-approved them, and all five were made and nothing else in another unit's file:
`src/tui/actions_tests.rs` (the `variant_name` arm, the `ACTION_ROUTING` row, the struck residue row) and `tests/daemon_send_slice.rs` (`SEND_METHODS` six to eight, `the_cli_send_paths_take_no_hold` keeping `hold` refused on `send.invite` alone).

Four edits outside that list, each forced and each in a file this unit owns:

- `src/daemon/session.rs`'s capability-list test gains the two method names, which is the derivation working.
- `src/daemon/methods/send.rs`'s own module test is renamed from `six` to `eight`.
- `src/tui/app/types.rs` loses two tests about `HeldSend`: `a_held_send_is_ready_only_once_its_window_has_elapsed` (the type is gone) and `quit_refuses_while_a_send_is_holding` (the refusal is gone, and `hold_tests.rs` asserts its replacement).
- `src/tui/actions.rs`'s all-refused send test asserted through `send_status_line`; it asserts the same fact on the report directly, and the wording moved to the new `commands.rs` test.

### The reviewed edit in the T unit's own file

Three rows of `tests/daemon_send_hold.rs` asserted `transport_events(&log).len() == 1` after a successful send of `send_fixture::APPROVED`, and no hold design can satisfy that: `a_hold_nobody_cancels_fires_and_retires_the_draft`, `the_sender_may_close_its_window_while_another_client_watches` and `a_zero_window_sends_at_once_and_publishes_no_hold_event`.
The count is a property of the fixture and not of the hold: `send_fixture::widen_approved_draft` puts `carol@example.com` on the draft's `cc:`, the fake transport writes one `Submit` line **per recipient**, the send files its own Sent copy, and the send's outbox drain finishes the seeded `APPENDING_ROW`'s APPEND on its way out.
One send is therefore four ledger lines, which is exactly what `tests/daemon_send_slice.rs` says in prose where it counts by Message-ID instead:

> SND-09 is about *this* message's copy, counted by its Message-ID rather than by the ledger's length: the send drains the account's outbox on its way out, so the seeded row that was waiting on its APPEND (`APPENDING_ROW`) files its copy in the same run and a total of two is the drain doing its job.

The zero-window row is the control: it takes the ordinary unheld path and produces the same four lines.
So the edit, reviewed and approved for exactly these three rows, counts the draft's own submissions the same way: the new `draft_submissions` helper takes the Message-ID of the first `Submit` line and returns the addresses submitted under it, sorted, and each row asserts that they are `APPROVED_RECIPIENTS` (the draft's `to:` and the widened `cc:`).
"Sent exactly once" is one submission per recipient with nothing repeated, which is the meaning the length assertion was reaching for.
The file's header bullet said "exactly one submission" and now says one per recipient; nothing else in the file moved.

### Validation

`TMPDIR=/var/tmp cargo test --workspace --offline` -> **2230 passed, 0 failed**, 2 ignored.
`--lib hold_tests` 16, three runs; `--test daemon_send_hold` 7 of 7, three runs; `--test phase5_undo_send_hold` 2; `--lib actions_tests` 22; `--lib events_tests` 20; `--test phase5_parity_gate` 11; `--test daemon_send_slice` green; the two golden-frame suites 20 and 22 with no snapshot re-approved; `--test architecture_boundaries` and `--test test_selection_guard` green.
`cargo clippy --workspace --offline --all-targets` -> 34 warnings, none new.
The CLI help walk and `dump-keys --json` both diff empty against `docs/baselines/pre-daemon/`.

## P6-U3: graceful shutdown, the contract

One test file, no production code: `tests/daemon_shutdown.rs`, 12 rows, every one of them a real `mp daemon run` over a real socket.

There is no in-process half and no new Rust name, so there is no stub proof either: the file **compiles against the tree as committed** and fails at runtime, which is the arrangement `tests/daemon_send_hold.rs` already uses.
Every clause of the plan's sentence is about a process ending - what its last frames were, what it left on disk, what it let go of - and none of that can be said in one process.

### The contract

On the wire and on stdout, and nowhere else.

```text
daemon.stop {grace_secs?: u64}  ->  {stopping: true, grace_secs, pending: [operation…]}

state.event {kind: "daemon.shutting_down", payload: {grace_secs, pending: [operation…]}}

daemon.stopped {instance_id, clean: bool, unsettled: [operation…]}   // a notification

-32009 shutting_down, "the daemon is shutting down"
```

- **`grace_secs`** is optional. Absent means the default, **10 seconds**; `0` means no waiting at all.
  A parameter is explicit where an environment hook is ambient, so `0` is honoured here rather than read as "unset" the way the daemon's numeric env hooks read it.
- **`pending`** and **`unsettled`** carry the `operation.status` object, the one `snapshot.operations` already carries (`operation_id`, `method`, `state`, `scope`, `progress`, `result`, `error`), in start order.
  A GUI that lists what is still running at shutdown renders it with the code it already has.
- **`daemon.shutting_down`** is a `state.event` like any other, so no client needs a second code path to receive it, and the name is the one `src/daemon/state/events.rs` and `tests/daemon_events.rs` have used as their lifecycle-event placeholder since P3a. It is also `-32009`'s own name.
- **`daemon.stopped`** is a third notification method beside `state.event` and `state.resync_required`, sent only on the connection that asked and only as the last frame before that connection closes.
  It exists because the asking connection is pre-handshake and therefore not subscribed: the report has to reach `mp daemon stop`, and the stop's own answer is flushed long before the daemon knows how the shutdown went.

### The order the daemon does it in

The rows are decidable only because the sequence is fixed:

1. mark shutting down - from here every other method is `-32009`;
2. cancel every armed hold, publishing `send.hold_cancelled`, drafts left `approved`;
3. publish `daemon.shutting_down` to every bootstrapped connection;
4. answer the `daemon.stop` with the effective grace and what is still live;
5. wait up to the grace for those operations to settle, cancelling whatever is left;
6. stop the watchers and the account runtimes, releasing the engine locks;
7. send `daemon.stopped` on the asking connection and close every connection;
8. unlink its own socket, `daemon.pid` and `daemon.json`, and exit 0.

Two consequences the rows assert directly.

A hold is **never** in `pending` or in `unsettled`, because step 2 happens before step 4.
That is the plan's "settled or cancelled, never silently dropped" for a window whose whole point is that the user may still stop it: a daemon that sat out the remaining seconds and then sent would send mail nobody could stop any more, since the client that would have pressed `u` is losing its socket in the same second.

With nothing live at step 4 the daemon does not enter the grace at all.
The grace is a ceiling, never a sleep, which is what keeps every existing suite's `mp daemon stop` as fast as it is today - `DaemonFixture`, `SandboxRoot::drop` and `stop_daemon` all stop idle daemons.

### `mp daemon stop`

The command gains `--grace-secs <N>`, passed through as the `grace_secs` parameter; absent, it sends `{}` and the daemon applies its own default.
Its existing `--timeout-secs` (default 10) is the wait for the process to disappear and is counted **after** the grace, so a long grace cannot make the command give up on a daemon that is doing what it was asked.
The daemon subcommand is hidden, so neither flag moves `tests/cli_help_snapshot.rs`.

Stdout, pinned word for word:

| case | stdout |
|---|---|
| clean | `✓ daemon stopped` |
| unclean | `✗ daemon stopped, {n} operation did not settle within {g}s`, then one `  {method} ({operation_id})` per operation |

`operation` becomes `operations` when `n != 1`, and the operations are listed in the order `unsettled` carried them.
Exit code **0** either way: the daemon stopped, which is what was asked.
A nonzero code would make `mp daemon restart` refuse to start the replacement, and would turn "a sync was still running" into a failure of the command that succeeded.
A stop that fell back to `SIGTERM` on a wedged daemon prints the clean line, because nothing told it otherwise.

### Which rows fail today, and which do not

`TMPDIR=/var/tmp cargo test --offline --test daemon_shutdown` on the tree as committed: **8 failed, 4 passed**, three runs, the same twelve every time.

The eight name the contract and nothing else:

```
the stop answer names the grace it will honour and the work it has to settle: {"stopping":true}
  left: ["stopping"]   right: ["grace_secs", "pending", "stopping"]
pending is an array: {"stopping":true}
the daemon closed the connection before any daemon.shutting_down
the daemon closed the connection before any send.hold_cancelled       // twice: the stop, and SIGTERM
a domain call during the grace was answered rather than refused: {"capabilities":[],…}
error: unexpected argument '--grace-secs' found
a client that joined mid-hold is told about the window it cannot otherwise know about
  left: 0   right: 1                                                  // snapshot.holds is still []
```

The last of them closes the follow-up P6-U2 recorded: `snapshot.holds` is hard-coded to `[]` in `src/daemon/state/snapshot.rs`, so a client that connects while a countdown is running cannot render it.

The four that pass are regression rows rather than new contract, and they are in the file deliberately: `serve`, `cleanup` and the signal watch are all rewritten by P6-U4, and each of these is a property today's shutdown has that the new one could quietly lose.

- `a_clean_stop_leaves_no_runtime_file_and_no_process` - all three runtime files, not only the socket, and no `mp daemon` left in the process list.
- `a_clean_stop_prints_the_line_it_has_always_printed` - the unclean wording must not leak into the clean case.
- `a_graceful_stop_removes_its_own_runtime_files_and_no_others` - a second daemon under a second root still answers afterwards.
- `a_graceful_stop_releases_the_engine_locks_the_runtimes_held`.

### What the file does not pin, and who does

- The **last client exiting mid-hold**: `tests/phase5_undo_send_hold.rs`, and P6-U2 landed the rule in `server.rs::handle_connection`. A shutdown is a different event and has its own rows.
- A **stale socket or pidfile after a SIGKILL**: `tests/daemon_runtime_paths.rs` owns the probe and `tests/daemon_lifecycle.rs` the sweep on the next start. A killed daemon runs none of the eight steps.
- A **socket owned by another uid**: `tests/daemon_runtime_paths.rs::a_socket_owned_by_another_uid_probes_unsafe_and_is_never_removed`. It is a property of the probe, which shutdown does not run; what shutdown owes is that it unlinks three paths under its own runtime directory and nothing else.
- The **engine lock across a kill**: `tests/phase5_parity_gate.rs::the_engine_the_legacy_lock_suite_assumes_is_the_daemon`. No assertion can tell "released at step 6" from "released when the process died", because the process ends either way; what the row here owes is that the graceful path ends with the lock free all the same.

### Two implementation notes the rows force

The `-32009` refusal has to sit **ahead of** `Session::gate`, not inside it.
The handshake gate exempts `initialize` by construction, so a refusal placed behind it would let a client that arrives during the grace negotiate its way into a daemon that is leaving; the row asserts the `initialize` itself is refused.

`snapshot.holds` needs the scheduler where the snapshot is built.
`CanonicalState::bootstrap` already reaches the operation registry for `snapshot.operations` and reaches nothing else that is not canonical state; the hold table is the second such projection, and `HoldScheduler::listing(None)` is the whole of it.

### Pre-approved edits for P6-U4

Nothing in `tests/` moves.
No method-spec array changes, because `daemon.stop` is lifecycle surface and not a dispatcher method: `LIFECYCLE_METHODS` stays two names and no `*_METHOD_SPECS` count or its `const _: () = assert!(…)` tripwire is touched.

Outside `tests/`, these are forced and pre-approved:

- **`docs/daemon-protocol.md`**: the `daemon.stop` paragraph (which today says it takes `{}` and that open connections are not drained), a `daemon.stopped` entry beside the two existing notification methods, `daemon.shutting_down` in the event-kind list, and a protocol-changelog entry. A second implementation reads the changelog, so a kind that is not in it is not in the protocol.
- **`docs/daemon-operations.md`**: the `mp daemon stop` paragraph, `--grace-secs`, and the recovery paragraph about a wedged daemon.
- **`src/daemon/server.rs`**: `serve`'s own doc comment says "Open connections are not drained" and names Phase 5 as where draining would arrive. That paragraph is what this unit changes and has to be rewritten rather than left as a false statement about its own file.
- **`src/daemon/state/snapshot.rs`**: `Snapshot::to_json`'s comment claims `holds` is empty because "nothing in this build produces one", which stopped being true at P6-U2.
- **`src/daemon/lifecycle.rs`**: the startup-sequence header, whose step 9 is "serve until `daemon.stop`, SIGTERM or SIGINT, then unlink all three".

Optional and equally mechanical: `src/daemon/state/events.rs`, `src/daemon/state/mod.rs` and `tests/daemon_events.rs` use the literal `"daemon.shutting_down"` as a placeholder lifecycle kind in queue tests. Those rows are about the queue and stay true; replacing the literal with the new constant is a tidy-up, not a requirement.

### Validation

`TMPDIR=/var/tmp cargo test --offline --test daemon_shutdown` -> 4 passed, 8 failed, three runs, identical each time.
`TMPDIR=/var/tmp cargo test --workspace --offline` with the file moved aside -> **2230 passed**, the count at `e8bf7b0`; with it present and `--no-fail-fast` -> 2234 passed, 8 failed, which is 2230 + the twelve new rows and nothing else disturbed.
`cargo clippy --offline --test daemon_shutdown` reports nothing in the new file.
`pgrep -af '[m]p daemon'` after every run: one line, the pid that was there before.
