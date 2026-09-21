---
id: 0125
title: Phase 6 of the daemon migration, hardening and service integration
type: feature
priority: now
status: open
created: 2026-09-21
---

Status: open. P6-U1 has landed its contract tests; nothing else of the phase has started.

Eighth ticket of the daemon-first architecture plan (`.agents/workflow/native-gui-daemon/plan.md` section 3.8), after #0118, #0119, #0120, #0121, #0122, #0123 and #0124.

Phase 5 left the TUI daemon-backed and the parity gate green.
Phase 6 makes the daemon something that can be left running: the undo-send hold moves out of the last client that owns a piece of domain behaviour, shutdown becomes graceful, the daemon starts at login through the platform's own service manager, and it can be asked how it is doing.

## The unit table

| unit | kind | commit | subject | status |
|---|---|---|---|---|
| P6-U1 | T | this commit | daemon-owned undo-send hold, the contract | done (tests) |
| P6-U2 | I | - | daemon-owned undo-send hold (SND-04) | not started |
| P6-U3 | T | - | graceful shutdown, the contract | not started |
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
