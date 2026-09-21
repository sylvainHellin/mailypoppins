---
id: 0125
title: Phase 6 of the daemon migration, hardening and service integration
type: feature
priority: now
status: open
created: 2026-09-21
---

Status: open. P6-U1 to P6-U6 have landed the daemon-owned hold, the graceful shutdown and the login-start service units; the rest of the phase has not started.

Eighth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.8), after #0118, #0119, #0120, #0121, #0122, #0123 and #0124.

Phase 5 left the TUI daemon-backed and the parity gate green.
Phase 6 makes the daemon something that can be left running: the undo-send hold moves out of the last client that owns a piece of domain behaviour, shutdown becomes graceful, the daemon starts at login through the platform's own service manager, and it can be asked how it is doing.

## The unit table

| unit | kind | commit | subject | status |
|---|---|---|---|---|
| P6-U1 | T | this commit | daemon-owned undo-send hold, the contract | done (tests) |
| P6-U2 | I | this commit | daemon-owned undo-send hold (SND-04) | done |
| P6-U3 | T | this commit | graceful shutdown, the contract | done (tests) |
| P6-U4 | I | this commit | graceful shutdown | done |
| P6-U5 | T | this commit | login-start service units (LIF-06), the contract | done (tests) |
| P6-U6 | I | this commit | login-start service units | done |
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

## P6-U4: graceful shutdown

`tests/daemon_shutdown.rs` is green, twelve of twelve, three runs.
The shutdown is `src/daemon/shutdown.rs`, 474 lines of which 61 are its own tests and 178 of the rest are comment or blank; the eight steps the contract fixes are one function each and the order is the module's subject.

### The sequence, and where each step runs

Steps 1 to 4 run **inside the connection task that answered**, synchronously, so the answer cannot describe a daemon that has already moved past them: mark, cancel the holds, publish `daemon.shutting_down`, then build `{stopping, grace_secs, pending}` from what the registry has left.
Steps 5 and 6 run in a driver task `shutdown::request` spawns, because the grace may be ten seconds and the stop's answer may not wait for it.
Step 7 is split: the driver publishes the report and the asking connection writes it, which is the only arrangement in which `daemon.stopped` is both the daemon's own word and the last frame on that socket.
Step 8 is `lifecycle::run`'s, unchanged: `serve` returns, `cleanup` unlinks.

### Four decisions the contract did not settle

- **The asking connection keeps serving while it waits.**
  The first design closed the read loop the moment the stop was answered, which reads well and cannot pass `a_shutting_down_daemon_refuses_every_command_but_the_two_lifecycle_ones`: that row sends a *second* `daemon.stop` on the same connection during the grace, and a connection that stopped reading could never answer it.
  So the stop sets a flag rather than a terminal state, the connection goes on answering `daemon.status` and a repeated `daemon.stop`, and a fourth `select!` arm on the settled signal is what turns it into the report writer.
  A connection that stops twice is one reporter and not two, or the driver would sit out its ceiling waiting for a frame nobody is going to write.
- **Step 7 closes the connections rather than letting the process exit under them.**
  The contract says "close every connection", and the cheap reading of that is "the kernel does it when we exit".
  It is cheap in the wrong place: a client whose `daemon.shutting_down` was still in its outbound queue would learn about an orderly stop as an EOF mid-stream, which is exactly what a crash looks like, and the SIGTERM row is the one that would flake on it, having no reporter to wait for.
  Every connection therefore wakes on the same settled signal, writes out what it was queued (`AfterFlush::Drain`), and closes; the driver waits, bounded at two seconds, for `subscriber_count()` to reach zero.
  The subscription is released when a connection task ends, so the count the state already keeps is what says they are gone.
- **The `-32009` refusal reads one flag and the two lifecycle names.**
  It sits ahead of `Session::gate` as P6-U3 required, and it asks `session::is_lifecycle_method` rather than matching two strings of its own: `LIFECYCLE_METHODS` is the single list of what answers before a handshake, and a second copy of it in the dispatcher would be the thing that drifts.
- **The grace is validated, not coerced.**
  `grace_secs` that is not a whole number of seconds is `-32602` naming what it got.
  A stop that silently ignored a malformed grace would wait for a length nobody asked for, and the parameter exists precisely because the caller has an opinion about that length.

### Two watch channels and a counter, and why `send_replace`

`Shutdown` holds a `settled` channel (step 6 is done, the report exists), a `watchers` channel (step 6 has begun, the draft watcher stops), and a reporter counter with a `Notify`.
Both channels are written with `send_replace` and never with `send`: `watch::Sender::send` fails and **leaves the value unchanged** when no receiver is live, and a daemon nobody has connected to is exactly that case.
The first version used `send`, and the symptom was a unit test that hung forever on a shutdown that had already finished.

### The two follow-ups P6-U3 left

`src/tui/events.rs` gained an arm for `daemon.shutting_down`: one status line, `Daemon stopping`, at `Warning`, and then the existing reconnect path in `src/tui/session.rs` takes over on the EOF with the sentence it already shows.
It answers `Applied::Ignored` rather than a sixth variant, because the five that enum has are P5-U7's contract and a new one would be a contract change for a line the drain does not branch on; nothing is reloaded and nothing is refetched, which is what `Ignored` promises a caller.
No golden frame moved: the frames are rendered from constructed `App` states and none of them is built from an event.

`lifecycle::cleanup` now guards the socket with the same instance check `daemon.pid` and `daemon.json` already had.
It was the one path by which a slow shutdown could unlink a *successor's* socket: a daemon that started after us wrote its own `daemon.json` over ours, so the file that named us names it, and the check that already protected its metadata now protects its socket too.

### `snapshot.holds`

`CanonicalState::attach_holds` is `attach_operations`' twin, and `bootstrap` fills the array from `HoldScheduler::listing(None)`.
The projection is taken in `bootstrap` rather than reduced into `Inner` for the reason the operations one is: the hold table is the daemon's, not the mirrored state's.
`mp_protocol::state::Snapshot::holds` stays `Vec<Value>`; the TUI reads a hold off the typed event and off `send.hold_status`, and typing it in the snapshot before a client reads it *there* would pin the shape from the wrong end.

### Test edits made

**None.** `git diff --stat -- tests/ src/tui/*_tests.rs` is empty, which is what P6-U3 asked for: no method spec moved, no `LIFECYCLE_METHODS` entry was added, and `daemon.stop` stayed lifecycle surface rather than becoming a dispatcher method.

Outside `tests/`, the five pre-approved doc edits were made (`docs/daemon-protocol.md`, `docs/daemon-operations.md`, `server.rs::serve`, `snapshot.rs::to_json`, `lifecycle.rs`'s startup header) and three files this unit owns gained tests of their own: `src/daemon/shutdown.rs` (3), `src/daemon/server.rs` (3 new rows on the dispatcher's refusal and the stop's answer shape, and the existing rows retargeted at a `dispatch` helper because `dispatch_request` now takes the state by `Arc` and an exit channel).
The optional tidy-up P6-U3 offered - replacing the `"daemon.shutting_down"` literal in the queue tests with the new constant - was **not** taken: those rows are about the queue, they stay true, and the edit would have touched `tests/daemon_events.rs` for nothing.

### Five new protocol fixtures

`daemon.stop.request.json`, `daemon.stop.response.json`, `notification.daemon_shutting_down.json`, `notification.daemon_stopped.json` and `error.shutting_down.json`, discovered by `tests/daemon_protocol_fixtures.rs` through its own directory walk, so the file it lives in did not move either.

### The CLI help walk

`MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt` is **empty**, not the diff hunk this unit was told to expect.
The `daemon` subcommand is hidden (`hide = true`, plan section 3.0), the walk recurses only into the names clap lists under `Commands:`, and the baseline has never carried a `daemon` line at all: `rg -c daemon docs/baselines/pre-daemon/cli-help.txt` is 0.
`--grace-secs` is therefore invisible to the snapshot by the same construction that has kept `mp daemon` out of it since P2-U7, and `mp daemon stop --help` shows it:

```
Options:
      --timeout-secs <TIMEOUT_SECS>  Seconds to wait for the daemon to go away, counted after the grace [default: 10]
      --grace-secs <GRACE_SECS>      Seconds the daemon may spend settling work in flight (0 waits none)
```

### Size

865 production lines added, 157 of test: over the ~700 the unit was budgeted, and the overrun is `src/daemon/shutdown.rs`, whose 413 non-test lines are 178 of comment.
The eight steps are a sequence whose *order* is the contract, and the file is where that order is written down.

### Validation

`TMPDIR=/var/tmp cargo test --offline --test daemon_shutdown` -> **12 passed**, three runs, 1.1 s each.
`TMPDIR=/var/tmp cargo test --workspace --offline` -> **2248 passed, 0 failed**, 5 ignored, which is 2242 at `7c96613` plus the six unit tests this unit added.
`--test daemon_send_hold` 7; `--test phase5_undo_send_hold` 2; `--test tui_daemon_recovery` 3; `--test daemon_lifecycle` 10; `--test daemon_runtime_paths` 17; `--test daemon_bootstrap` 28; `--test daemon_events` 29; `--test daemon_protocol_fixtures` 17; `--lib events_tests` 20; `--lib hold_tests` 16; the two golden-frame suites 20 and 22 with no snapshot re-approved.
`git diff --stat -- tests/ src/tui/*_tests.rs` empty.
`cargo clippy --workspace --offline --all-targets` -> 38 warnings, the count at `7c96613`, none in a file this unit touched.
`diff <(./target/debug/mp dump-keys --json) docs/baselines/pre-daemon/tui-keys.json` empty.

The smoke run, over an `examples/mkfixture` root in a sandbox `HOME`: `mp daemon start`, `mp daemon status` (running), `mp daemon stop` -> `✓ daemon stopped`, exit 0, and `<data_dir>/runtime` left holding only `daemon.start.lock`, which is never unlinked; then a second start and `kill -TERM <pid>` -> the same three files gone and the process with them.
`pgrep -af '[m]p daemon'` after every run: one line, pid 3667325, which is not this tree's.

## P6-U5: login-start service units (LIF-06), the contract

One test file and two fixtures, no production code.

- `tests/daemon_service.rs`, 23 rows, every one of them socket-free: the two commands write a file and call a service manager, and neither needs a daemon to exist.
- `tests/fixtures/service/mailypoppins.service` and `tests/fixtures/service/dev.mailypoppins.daemon.plist`, the byte-exact generated files with three placeholders (`{{MP}}`, `{{DATA_DIR}}`, `{{CONFIG_DIR}}`) the rows substitute.

There is no stub proof: the file compiles against the tree as committed, importing only `mailypoppins::daemon::client::needs_daemon` and `mailypoppins::daemon::shutdown::DEFAULT_GRACE_SECS`, both of which exist.
The 22 contract rows fail at runtime on clap's `unrecognized subcommand`, which is the arrangement `tests/daemon_shutdown.rs` used.

### The surface

```text
mp daemon install-service   [--force] [--check]
mp daemon uninstall-service
```

Both are local commands under the already-hidden `daemon` subtree, so `mp --help` and `docs/baselines/pre-daemon/cli-help.txt` do not move.

**No wire method.** The parity matrix's `LIF-06` row named `daemon.install_service` and `daemon.remove_service`; a method would mean asking a running daemon to arrange for a daemon to run, and the file being installed is the thing that starts one. The row now names the two commands instead, and `LIFECYCLE_METHODS` and every `*_METHOD_SPECS` array stay as they are, with their `const _: () = assert!(…)` count tripwires untouched.

`needs_daemon` already answers `false` for `(Some("daemon"), _)`, so neither command auto-starts anything; the last row asserts it, passes today, and is there because P6-U6 adds two names to that family.

### The two environment hooks

Both in the `MAILYPOPPINS_DAEMON_*` family the other ten hooks use, both without a flag.

- `MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN=1` renders, writes and removes the file exactly as usual and **runs no `systemctl` and no `launchctl`**, printing the lines it would have run plus one line saying nothing was run.
  This is what keeps the suite off the developer's own user session.
- `MAILYPOPPINS_DAEMON_SERVICE_OS=linux|darwin` selects the half. Unset means this build's target OS; an unrecognised value is an error naming the variable and the two values it takes.
  It exists because the macOS half cannot be smoke-tested on this host - the plan says so and carries the live launchd check as an escalation - so without it the plist would be pinned nowhere at all.

Three rows run with the dry run **off** and `PATH` pointing at a sandbox directory holding a fake `systemctl` that records its argv and exits with a chosen code.
Containment there is by `PATH`, not by the hook: the real `systemctl` is unreachable from those children, and recording the argv is the only way to pin *which* commands run and in what order rather than only which are printed.

### The files

| OS | path |
|---|---|
| linux | `$XDG_CONFIG_HOME/systemd/user/mailypoppins.service`, falling back to `$HOME/.config` |
| darwin | `$HOME/Library/LaunchAgents/dev.mailypoppins.daemon.plist` |

Both are written 0644, with their parent directories created as needed, and neither holds a secret.
`{{MP}}` is `std::env::current_exe()`; `{{DATA_DIR}}` and `{{CONFIG_DIR}}` are the two directories the installing `mp` resolved, canonicalised, i.e. the exact strings `mp daemon status --json` reports.
The rows render `{{MP}}` canonicalised because the test binary is a real file under `target/` where both spellings are one string; they are not one string for a Homebrew `mp`, whose `bin/mp` is a symlink into a version-stamped Cellar directory, so the contract names `current_exe()` and leaves the resolution to it.

```ini
# Written by `mp daemon install-service`.
# Hand edits are replaced by `mp daemon install-service --force`.

[Unit]
Description=mailypoppins daemon
Documentation=https://mailypoppins.dev

[Service]
Type=simple
ExecStart={{MP}} daemon run
Environment=MAILYPOPPINS_DATA_DIR={{DATA_DIR}}
Environment=MAILYPOPPINS_CONFIG_DIR={{CONFIG_DIR}}
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=15

[Install]
WantedBy=default.target
```

The plist carries `Label`, `ProgramArguments` `[{{MP}}, daemon, run]`, `EnvironmentVariables` with the same two directories, `RunAtLoad`, `KeepAlive` `{SuccessfulExit: false}`, `ExitTimeOut 15`, and `StandardOutPath` / `StandardErrorPath` both at `{{DATA_DIR}}/logs/daemon.log`, which is `lifecycle::daemon_log_path()` and the file `mp daemon start` already points a detached daemon's stdio at.
The keys are pinned in that order, tab-indented, Apple's doctype and XML declaration first.
The darwin install also creates `<data_dir>/logs`, because launchd refuses a job whose `StandardOutPath` names a directory that does not exist.

Four decisions inside those files, all asserted:

- **`ExecStart` is `daemon run`, never `daemon start`.** A `Type=simple` unit whose `ExecStart` forked and returned would be restarted forever by `Restart=on-failure`, and the daemon left behind would be one systemd does not own. The rows also assert `--foreground-logs` is absent: the journal gets what the daemon logs anyway.
- **`Environment=` is exactly the data and config directories, and nothing else.** A login-started daemon inherits the session manager's environment, not the shell's, so a user whose `MAILYPOPPINS_DATA_DIR` or `XDG_DATA_HOME` is exported from a shell rc file would otherwise get a daemon serving a different tree than the one his `mp` talks to. No `PATH`: the binary is invoked by absolute path and the daemon spawns nothing that needs one.
- **`TimeoutStopSec` and `ExitTimeOut` are `DEFAULT_GRACE_SECS + 5`.** The tie is asserted against the constant rather than assumed, so a grace raised in `src/daemon/shutdown.rs` without the unit following it fails here instead of having the service manager `SIGKILL` a daemon in the middle of the eight steps.
- **`KillSignal=SIGTERM`** is the signal those eight steps answer (P6-U4), and `KeepAlive {SuccessfulExit: false}` is its launchd equivalent: a crash is restarted, a `mp daemon stop` (exit 0) is not.

The plist is checked structurally - declaration, doctype, balanced `dict` and `array`, the eight keys in order, the argv array verbatim - and linted by `plutil -lint` when it is on `PATH`, with an `eprintln` skip otherwise, which is what this Linux host takes. No `plist` crate: it is not in `Cargo.lock` and one file does not buy a dependency.

### Stdout, pinned line for line

In the style of `mp daemon stop`: a `✓`/`✗` first line, then indented detail lines.

| case | stdout | exit |
|---|---|---|
| fresh install | `✓ wrote <path>`, then the command lines | 0 |
| identical unit already there | `✓ <path> is already installed`, then the same command lines | 0 |
| uninstall | `✓ removed <path>`, then the command lines | 0 |
| uninstall with nothing installed | `✓ no service installed`, and nothing is run | 0 |
| `--check`, installed | `✓ service installed at <path>` | 0 |
| `--check`, absent | `✗ no service installed` / `  install one: mp daemon install-service` | 1 |

The command lines, in order, are `  systemctl --user daemon-reload` then `  systemctl --user enable --now mailypoppins.service` on install, and `  systemctl --user disable --now mailypoppins.service` then `  systemctl --user daemon-reload` on uninstall - disable while the unit file is still readable, reload once it is gone.
The darwin half prints one line each: `  launchctl bootstrap gui/<uid> <plist>` and `  launchctl bootout gui/<uid>/dev.mailypoppins.daemon`, the bootout targeting the label rather than the path.

Two lines are appended to that block, never both:

- `  dry run: MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN is set, nothing was run`
- `  systemctl is not on PATH; run the lines above to enable the service`

A missing service manager is **not** a failure of the write: that is a container, a minimal image, or a session that is not systemd's, and a command that refused to write the file there would be useless exactly where a user would copy the unit somewhere else himself. Exit 0.
A `systemctl` that runs and *fails* is exit 1 with a `✗` on stderr naming it, and the unit stays on disk: the file is what the command owns, enabling is what it asked the session manager for, and removing the file because the enable failed would throw away the half that worked.

An install over a unit whose content differs is refused: exit 1, a `✗` line on stderr naming the path, the word `differs` and `--force`, and the file left byte-identical.
A user may have edited it, and an upgrade that silently overwrote that edit would lose it without saying so. `--force` replaces it and reports a write.
An identical unit is not rewritten at all - the row asserts the mtime does not move - but the enable still runs, because a user who disabled the unit by hand expects `install-service` to put it back.

### Which rows fail today

`TMPDIR=/var/tmp cargo test --offline --test daemon_service` on the tree as committed: **1 passed, 22 failed**, three runs, identical every time.

All 22 fail the same way and for the same reason, which is what a contract test with no implementation behind it looks like:

```
error: unrecognized subcommand 'install-service'
error: unrecognized subcommand 'uninstall-service'

Usage: mp daemon [OPTIONS] <COMMAND>
```

clap exits 2, so every row's first assertion - the exit code, or a `stdout_lines` comparison against an empty stdout - is what trips.
The one that passes is `neither_service_command_is_on_the_daemon_list`, a regression row over `needs_daemon`, which is vacuously true until the two subcommands exist and load-bearing afterwards.

### Pre-approved edits for P6-U6

Nothing in `tests/` moves, and neither fixture moves.
No method-spec array and no count tripwire is touched, because this unit adds no wire method.

Outside `tests/`, these are forced and pre-approved:

- **`src/daemon/lifecycle.rs`**: two `DaemonAction` variants and their `dispatch` arms, plus the module header, whose first line lists the five lifecycle commands.
- **`docs/daemon-operations.md`**: the *Login mode* section, which today says "Not implemented" and names this unit, and the environment-hook list, which says "all ten environment hooks" in the `DaemonFixture` paragraph and becomes twelve. `DaemonFixture::start` clears the hooks it lists, and the two new ones belong in that clearing.
- **`docs/parity-matrix.md`**: `LIF-06` - P6-U5 already points it at this ticket and corrects its daemon-surface line; P6-U6 moves its Status.
- **`docs/release-process.md`**: a note that a binary reinstalled at a different path leaves a unit whose `ExecStart` points at the old one, which `mp daemon install-service --force` repairs. Installing from source keeps `~/.cargo/bin/mp` and does not move; a Homebrew upgrade does, if `ExecStart` ends up holding the resolved Cellar path rather than the `bin/mp` symlink, which is the one case P6-U6 has to check on macOS and no test on this host can see.
- **`CHANGELOG.md`** and the unit table above.

The website pages under `website/src/pages/` are hand-derived from `mp --help`, and the `daemon` subtree is hidden from it, so they stay as they are.

### Escalation

The launchd half is written and pinned as a fixture and cannot be smoke-tested here: `plutil` is absent, `launchctl` is absent, and `MAILYPOPPINS_DAEMON_SERVICE_OS=darwin` on Linux proves only that the right bytes are produced.
The live check - `mp daemon install-service` on macOS, log out and back in, `mp daemon status` reporting a running daemon, `mp daemon uninstall-service` - is owner action on Sylvain's Mac, as plan risk 8 already anticipated.

### Validation

`TMPDIR=/var/tmp cargo test --offline --test daemon_service` -> 1 passed, 22 failed, three runs, identical each time.
`TMPDIR=/var/tmp cargo test --workspace --offline` with the file moved aside -> **2248 passed**, 0 failed, 5 ignored, the count at `29691c0`; with it present and `--no-fail-fast` -> 2249 passed, 22 failed, which is 2248 plus the one regression row and the twenty-two contract rows, nothing else disturbed.
`cargo clippy --offline --test daemon_service` reports nothing in the new file.
`rustfmt --edition 2021 tests/daemon_service.rs` leaves it unchanged.
`pgrep -af '[m]p daemon'` after every run: one line, pid 3667325, which is not this tree's.

## P6-U6: login-start service units

`tests/daemon_service.rs` is green, twenty-three of twenty-three, three runs.
The commands are `src/daemon/service.rs`, 469 production lines of which 56 are the module header, plus 150 of unit test, and two template files that are byte-for-byte copies of the committed fixtures.

### The templates live in `src/`, not in `tests/`

The contract offered `include_str!` of the fixtures themselves.
It was not taken: a library that only builds when its test fixtures are present has a second source tree, and `tests/` is excluded from what a published crate has to be able to compile.
`src/daemon/templates/mailypoppins.service` and `src/daemon/templates/dev.mailypoppins.daemon.plist` are the compiled-in copies, and `the_templates_are_the_committed_fixtures` reads both fixtures at test time and asserts equality, so a fixture edit that does not reach `src/` fails in the module rather than twenty-three rows later.

`TimeoutStopSec` and `ExitTimeOut` stay literals in those templates, as they are in the fixtures, and `the_stop_timeout_follows_the_shutdown_grace` ties both to `DEFAULT_GRACE_SECS + STOP_MARGIN_SECS`.
Rendering them from the constant instead would have made the in-crate copy stop being a byte-for-byte copy, for no gain: a raised grace fails the same two tests either way, and the fix is one line in each template.

### Five decisions the contract did not settle

- **The stdout block is built before it is printed.**
  An uninstall prints `✓ removed <path>` above the two `systemctl` lines, but it has to *run* the disable before the removal and the reload after it, so printing as it goes would either print the header before the removal it claims or print the commands out of the order the contract fixes. The lines are assembled once the mode is known, the work runs, and the block is printed at the end - including on the failure path, so a failed enable still shows what was written and what was attempted before the `✗` on stderr.
- **`--check` prints its `✗` to stdout.**
  Every other refusal in these two commands goes to stderr through `dispatch`'s error arm. `--check` is a report the user asked for rather than a failure of the command, the row compares `stdout_lines` against both lines, and the exit code is what a script branches on.
- **A missing service manager is decided by `PATH` and nothing else.**
  `on_path` looks for an executable file named `systemctl` or `launchctl` on the process's own `PATH`, which is what the three contained rows manipulate. No `which` crate, and no probe of whether the session is actually systemd's: a user session with `systemctl` present and no user bus gets the command's real exit code, which is more informative than a guess made before running it.
- **The refusals travel as `anyhow` errors.**
  An unrecognised `MAILYPOPPINS_DAEMON_SERVICE_OS`, a file whose content differs, and a service-manager command that exits nonzero are all `bail!`, so `lifecycle::dispatch`'s existing arm prints `✗ {e:#}` on stderr and returns 1. Three refusal paths with one spelling, and the `✗` is the one every other lifecycle command already prints.
- **A re-install does not `bootout` before `bootstrap` on darwin.**
  The contract pins exactly one `launchctl` line per operation and the row compares stdout exactly, so a defensive `bootout` would be a fourth line nobody asked for. Whether `bootstrap` refuses a label it has already loaded is one of the two questions the live check answers.

### Test edits made

**None.** `git diff --stat 33d209e..HEAD -- tests/` is empty: no row moved, neither fixture moved, and no method-spec array or count tripwire was touched, because this unit adds no wire method.

One pre-approved edit was deliberately **not** made. P6-U5 asked for the two new hooks to join `DaemonFixture::start`'s clearing list, and that list is `HOOKS` in `tests/support/parity.rs`, inside the frozen tree.
No fixture installs a login service, and `tests/daemon_service.rs` sets both hooks explicitly on every child it runs, so the clearing buys nothing; the doc paragraph says so instead of the code lying about it.
That paragraph also said "all ten environment hooks" while the list has carried twelve since P3b, and is corrected to twelve.

The other four pre-approved edits were made: `src/daemon/lifecycle.rs` (two `DaemonAction` variants, their `dispatch` arms and the module header), `docs/daemon-operations.md`, `docs/parity-matrix.md` and `docs/release-process.md`.

Two corrections outside that list, both of a document that no longer described the tree:

- `docs/daemon-operations.md`'s hook section counted thirteen variables and now counts fifteen.
- `docs/parity-matrix.md`'s lifecycle group opened with "no daemon, serve, or IPC surface exists in the tree", and `LIF-01` to `LIF-05` and `LIF-08` all still read "Status: not started" although P2-U7 (#0120) and P4-U2 (#0123) shipped them. Each now names its unit, its ticket and the suite that covers it; `LIF-04` also names P6-U4, which is what made it graceful.

No CHANGELOG entry. The phase's entry is P6-U10's, which is how P6-U2 and P6-U4 left it: there is no `#0125` section to append a unit to, and one written now would be rewritten at the exit sweep.

### Escalation: the live launchd check is NOT TAKEN

Unchanged from what P6-U5 recorded, and now with a second question on it.
`launchctl` and `plutil` are both absent from this host, so `MAILYPOPPINS_DAEMON_SERVICE_OS=darwin` proves the bytes and nothing else.
Owner action on the Mac: install the service, log out and back in, `mp daemon status` reporting a running daemon, then remove it again.

1. Does `bootstrap` refuse a label it has already loaded? If it does, a re-install over an identical plist wants a `bootout` first and the contract's one-line stdout has to grow.
2. What does `current_exe()` resolve to under a Homebrew `mp`? If it is the version-stamped Cellar path rather than the `bin/mp` symlink, the agent breaks at the next `brew upgrade` and the repair is a forced re-install. `docs/release-process.md` carries that as an open item.

### Validation

`TMPDIR=/var/tmp cargo test --offline --test daemon_service` -> **23 passed**, three runs, 0.05 s each.
`TMPDIR=/var/tmp cargo test --workspace --offline` -> **2279 passed, 0 failed**, 5 ignored, which is 2248 at `29691c0` plus the twenty-three contract rows and the eight unit tests this unit added.
`--test daemon_lifecycle` 10; `--test daemon_shutdown` 12; `--lib daemon::service` 8.
`git diff --stat 33d209e..HEAD -- tests/` empty.
`cargo clippy --workspace --offline --all-targets` -> 38 warnings, the count at `7c96613`, none in a file this unit touched.
`MP=./target/debug/mp scripts/capture-cli-help.sh | diff - docs/baselines/pre-daemon/cli-help.txt` and `diff <(./target/debug/mp dump-keys --json) docs/baselines/pre-daemon/tui-keys.json` both empty: the `daemon` subtree is hidden, so two more subcommands are invisible to the walk by the same construction that has kept `mp daemon` out of it since P2-U7.

The smoke run, in a sandbox `HOME` with `MAILYPOPPINS_DAEMON_SERVICE_DRY_RUN=1`: install, install again, `--check`, uninstall, then the same four with `MAILYPOPPINS_DAEMON_SERVICE_OS=darwin`, each printing the block the table above fixes and each exiting 0 except the `--check` with nothing installed.
No real `systemctl --user enable` was run on this host.
`pgrep -af '[m]p daemon'` after every run: one line, pid 3667325, which is not this tree's.
