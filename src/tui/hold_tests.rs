//! The daemon-owned undo-send hold: the contract (P6-U1, ticket #0125).
//!
//! This file is a **contract test**: it is written before the hold leaves
//! `src/tui/actions.rs`, against the surface P6-U2 has to supply
//! (`.agents/workflow/native-gui-daemon/plan.md` section 3.8, P6-U1/P6-U2),
//! the row `docs/parity-matrix.md` classifies as `SND-04`, and the `send.*`
//! family of `docs/daemon-protocol.md`. It does not compile against today's
//! tree, which has no `send.hold_status`, no `Applied::Hold` and no
//! `Action::CancelHeldSend`; that failure *is* the proof the contract has no
//! stub behind it. The implementer does not edit this file, they make it pass.
//!
//! The socket-level half is `tests/daemon_send_hold.rs`: two clients, one
//! countdown, a cancel from the window that did not send, and the CLI still
//! bypassing the hold. Everything a `&mut App` can be asked in-process is
//! here, because that is where the status line, the `u` key and the residue
//! are.
//!
//! # Contract
//!
//! ```text
//! mp_protocol::send::HoldStatus {                 // Clone + Debug + PartialEq + Eq + Serde
//!     operation_id: String,   // the id `send.draft` / `send.approved` answered with
//!     account: String,
//!     draft_id: String,       // the `id:` of the draft that is waiting
//!     subject: String,        // its subject, empty when it has none
//!     hold_secs: u64,         // the window it was armed with
//!     remaining_secs: u64,    // what is left of that window, 0 once it fired or was cancelled
//!     fires_at: String,       // RFC3339 UTC, so a late client can render a deadline
//!     origin: String,         // the kind word of the client that asked: "tui", "cli", "gui"
//! }
//! mp_protocol::send::HoldListing { holds: Vec<HoldStatus> }   // `send.hold_status`'s result
//!
//! mp_protocol::events::KIND_SEND_HOLD_STARTED   == "send.hold_started"
//! mp_protocol::events::KIND_SEND_HOLD_TICK      == "send.hold_tick"
//! mp_protocol::events::KIND_SEND_HOLD_FIRED     == "send.hold_fired"
//! mp_protocol::events::KIND_SEND_HOLD_CANCELLED == "send.hold_cancelled"
//!     // all four are `state.event` notifications whose payload is one `HoldStatus`
//!
//! daemon::methods::send::SEND_METHOD_SPECS gains, in method-name order:
//!     MethodSpec::new("send.cancel_hold", MethodKind::Command, 1)
//!     MethodSpec::new("send.hold_status", MethodKind::Query,   1)
//!
//! tui::app::Action::CancelHeldSend                    // a new variant, non-suspending
//! tui::app::App::hold: Option<mp_protocol::send::HoldStatus>
//! tui::events::Applied::Hold(String)                  // the operation_id the event concerned
//! ```
//!
//! Nine names, and nothing else. Every other type below is the TUI's own, the
//! daemon's own or the protocol's own and exists today.
//!
//! ## Why `hold` is a boolean and not a number of seconds
//!
//! The plan moves the hold *into the daemon*, and a client that computed a
//! window from a config file it read itself would leave the policy where it is
//! today. So `send.draft` and `send.approved` take `hold: bool`, defaulting to
//! `false`, and the daemon resolves `email.send_hold_secs` from the
//! configuration it already owns and already serves through `config.get`. Two
//! consequences, and both are wanted:
//!
//! - **The CLI bypasses the hold by construction.** `mp send` and
//!   `mp send-approved` pass no `hold` at all, so `ANO-7` stays true without a
//!   line of client code, and so does every other caller that exists today -
//!   `tests/daemon_send_slice.rs` calls both methods directly and must not
//!   start waiting out a twenty-second window because a parameter's default
//!   changed under it.
//! - **`send_hold_secs = 0` is a daemon-side rule.** A `hold: true` against a
//!   zero window fires at once, publishes no `send.hold_*` event and leaves
//!   `send.hold_status` empty, which is the opt-out #0090 shipped.
//!
//! ## Why the status line is pinned word for word
//!
//! `SND-04` says the migration preserves the presentation, and the
//! presentation is three sentences that exist in exactly one place today
//! (`src/tui/actions.rs` and `src/tui/app/keys.rs`). They are asserted here as
//! literals so the move cannot quietly reword them. There is no golden frame
//! of the countdown to compare against: `rg 'press u to undo' src/tui/ui/`
//! finds nothing, the hold has never been captured in
//! `src/tui/ui/golden_frames*.rs`, and this file does not mint one - a frame
//! whose status line is a literal three tests already assert would pin the
//! same string twice and cost a snapshot review.
//!
//! The one thing that *does* change is the number: today the line is written
//! once at arm time and shows the whole window for its whole life, because
//! nothing in the TUI counts down. With a per-second event the same sentence
//! carries the real remainder, and the first one a client sees (`hold_secs ==
//! remaining_secs`) is byte-identical to today's.
//!
//! ## Why quitting mid-hold stops refusing
//!
//! `Message::Quit` refuses today, and says why: *"A send is holding"*, because
//! the hold lives in this process and quitting would drop a send the user
//! confirmed. Once the daemon owns it that reason is gone - the hold outlives
//! the window - and the plan states the replacement rule in its own words:
//! *"When the last client exits mid-hold the daemon cancels the hold and
//! leaves the draft approved."* That is P6-U3's to enforce daemon-side; what
//! P6-U2 owes is that the TUI stops standing in the way, which is the row in
//! section (e).
//!
//! ## Determinism
//!
//! No row sleeps, no row polls and no row opens a socket: an event is built as
//! a value and handed to `App::apply_event`, and a command is dispatched
//! against a recorder. Rows that build an `App` own a per-thread data root
//! ([`crate::config::test_env::TestDataDir`], #0077) held alive for as long as
//! the `App`, so no row can reach the developer's tree.

use std::cell::RefCell;
use std::path::Path;

use serde_json::{json, Value};

use mp_protocol::events::{
    KIND_SEND_HOLD_CANCELLED, KIND_SEND_HOLD_FIRED, KIND_SEND_HOLD_STARTED, KIND_SEND_HOLD_TICK,
};
use mp_protocol::send::{HoldListing, HoldStatus};
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::config::{AccountConfig, SmtpConfig};
use crate::daemon::dispatch::{CancelScope, MethodKind};
use crate::daemon::methods::send::SEND_METHOD_SPECS;
use crate::tui::app::{build_mailboxes, Action, App, MailboxKind, Message, StatusLevel};
use crate::tui::commands::{dispatch, route, ActionRoute};
use crate::tui::events::Applied;
use crate::tui::queries::Queries;

/// The account every row names.
const ACCOUNT: &str = "alice";

/// The instance every event travels under, and the one every fixture
/// bootstraps against.
const INSTANCE: &str = "p6u1-hold";

/// The revision a fixture's bootstrap watermarks at.
const WATERMARK: u64 = 10;

/// The operation id the hold under test travels under. A literal, so a failure
/// names the hold it is about.
const OPERATION: &str = "op-7f3a";

/// The window every fixture arms, which is also `email.send_hold_secs`'s
/// default (`src/config.rs`) and therefore the number today's status line
/// shows.
const HOLD_SECS: u64 = 20;

// ---------------------------------------------------------------------------
// (a) The wire vocabulary
// ---------------------------------------------------------------------------

/// The four kinds spell what `docs/daemon-protocol.md` says they spell.
///
/// Constants rather than literals everywhere else in this file and in the
/// daemon, and asserted here once: a rename that misses one place fails here
/// rather than as a countdown that silently never starts.
#[test]
fn the_four_hold_kinds_spell_their_wire_names() {
    assert_eq!(KIND_SEND_HOLD_STARTED, "send.hold_started");
    assert_eq!(KIND_SEND_HOLD_TICK, "send.hold_tick");
    assert_eq!(KIND_SEND_HOLD_FIRED, "send.hold_fired");
    assert_eq!(KIND_SEND_HOLD_CANCELLED, "send.hold_cancelled");
}

/// One shape for all four kinds and for `send.hold_status`, and it round-trips.
///
/// Four kinds sharing one payload is the point: a client decodes once and
/// renders a countdown from whichever of the four it happened to receive, and
/// a GUI that joins mid-hold gets the same object out of `send.hold_status`
/// that the stream is carrying. `remaining_secs` is 0 on a fired or cancelled
/// hold, which is what makes the terminal events decodable as the same struct.
#[test]
fn a_hold_status_round_trips_through_the_wire() {
    let status = hold_status(HOLD_SECS);
    let wire = serde_json::to_value(&status).expect("a HoldStatus serialises");
    assert_eq!(
        wire,
        json!({
            "operation_id": OPERATION,
            "account": ACCOUNT,
            "draft_id": "freigabe",
            "subject": "Angebot",
            "hold_secs": HOLD_SECS,
            "remaining_secs": HOLD_SECS,
            "fires_at": "2026-09-11T08:00:20Z",
            "origin": "tui",
        }),
        "the field names are the wire's, so a GUI reads them without a mapping"
    );
    let back: HoldStatus = serde_json::from_value(wire).expect("and deserialises");
    assert_eq!(back, status);

    let listing: HoldListing = serde_json::from_value(json!({"holds": [&status]}))
        .expect("`send.hold_status` answers a listing");
    assert_eq!(listing.holds, vec![status], "which carries the same shape");
}

// ---------------------------------------------------------------------------
// (b) The two methods the daemon gains
// ---------------------------------------------------------------------------

/// The family serves eight methods, and the two new ones are a query and a
/// command.
///
/// `send.hold_status` reads a scheduler that is already there, so it is a
/// [`MethodKind::Query`]. `send.cancel_hold` is one decision committed at once
/// - the hold is dropped and the draft is left approved - so it is a
/// [`MethodKind::Command`] and reports a revision, not an operation that
/// finishes later. Neither is a second operation: the send's own operation id
/// is what they address, which is why `send.cancel_hold` takes an
/// `operation_id` and nothing else.
#[test]
fn the_family_declares_the_two_hold_methods() {
    let names: Vec<&str> = SEND_METHOD_SPECS.iter().map(|spec| spec.name).collect();
    assert_eq!(
        names,
        [
            "send.approved",
            "send.cancel_hold",
            "send.draft",
            "send.hold_status",
            "send.invite",
            "send.outbox_discard",
            "send.outbox_list",
            "send.outbox_retry",
        ],
        "the family serves exactly these eight, still in method-name order"
    );

    let spec = |name: &str| {
        *SEND_METHOD_SPECS
            .iter()
            .find(|spec| spec.name == name)
            .unwrap_or_else(|| panic!("{name} is declared"))
    };
    assert_eq!(spec("send.hold_status").kind, MethodKind::Query);
    assert_eq!(spec("send.cancel_hold").kind, MethodKind::Command);
    for name in ["send.hold_status", "send.cancel_hold"] {
        assert_eq!(spec(name).since, 1, "{name} is served from protocol 1");
        // The whole family is durable and these two are no exception: a hold
        // that died because the window that armed it closed is the one
        // behaviour `tests/phase5_undo_send_hold.rs` forbids, and cancelling
        // is a decision a client makes rather than one a disconnect makes for
        // it. The daemon cancelling a hold when the *last* client leaves is
        // P6-U3's rule and is not a cancel scope.
        assert_eq!(
            spec(name).cancel_scope,
            CancelScope::Durable,
            "{name} must not be abandoned when its client goes away"
        );
    }
}

// ---------------------------------------------------------------------------
// (c) The countdown, applied to the model
// ---------------------------------------------------------------------------

/// A `send.hold_started` puts the countdown on the status line and the hold in
/// the model.
///
/// The sentence is `src/tui/actions.rs`'s, to the byte, and the first event of
/// a hold carries the whole window, so the line a user sees when the hold
/// starts is the line #0090 shipped.
#[test]
fn a_started_hold_renders_the_countdown_the_tui_renders_today() {
    let mut fixture = Fixture::new();

    let applied = fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));

    assert_eq!(applied, Applied::Hold(OPERATION.to_string()));
    assert_eq!(
        fixture.app.status_message.as_deref(),
        Some("Sending in 20s (press u to undo)"),
        "the wording is the one the TUI has shown since #0090"
    );
    assert!(
        matches!(
            fixture.app.status_log.back().map(|entry| &entry.level),
            Some(StatusLevel::Progress)
        ),
        "and it is progress, not information: something is about to happen"
    );
    assert_eq!(
        fixture
            .app
            .hold
            .as_ref()
            .map(|hold| hold.operation_id.as_str()),
        Some(OPERATION),
        "the model holds the countdown it is rendering, and nothing else"
    );
}

/// Every tick rewrites the same sentence with the remainder the daemon
/// reports.
///
/// The client never subtracts: a countdown computed from a local clock drifts
/// against the daemon that owns the timer, and two windows would then disagree
/// about how long is left of one hold.
#[test]
fn every_tick_carries_the_remainder_and_the_client_computes_nothing() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));

    for remaining in [19u64, 5, 1] {
        let applied = fixture
            .app
            .apply_event(&event(KIND_SEND_HOLD_TICK, &hold_status(remaining)));
        assert_eq!(applied, Applied::Hold(OPERATION.to_string()));
        assert_eq!(
            fixture.app.status_message.as_deref(),
            Some(format!("Sending in {remaining}s (press u to undo)").as_str()),
            "the line shows the daemon's remainder"
        );
        assert_eq!(
            fixture.app.hold.as_ref().map(|hold| hold.remaining_secs),
            Some(remaining),
            "and so does the model"
        );
    }
}

/// A hold that fired is the send leaving, which is the line the send key has
/// always shown as the draft goes.
#[test]
fn a_fired_hold_says_the_send_is_on_its_way_and_clears_the_countdown() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));

    let applied = fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_FIRED, &hold_status(0)));

    assert_eq!(applied, Applied::Hold(OPERATION.to_string()));
    assert_eq!(
        fixture.app.status_message.as_deref(),
        Some("Sending..."),
        "`fire_held_send`'s own line, which is what the user reads today"
    );
    assert!(
        fixture.app.hold.is_none(),
        "a fired hold is no longer a countdown anyone can cancel"
    );
}

/// A cancelled hold says the draft is untouched, whoever cancelled it.
///
/// The event reaches every bootstrapped connection, so the window that pressed
/// `u` and the window that merely watched show the same sentence: that is what
/// "one countdown" means, and it is why the line is written from the event
/// rather than optimistically from the key.
#[test]
fn a_cancelled_hold_says_the_draft_is_untouched_in_every_window() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));

    let applied = fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_CANCELLED, &hold_status(0)));

    assert_eq!(applied, Applied::Hold(OPERATION.to_string()));
    assert_eq!(
        fixture.app.status_message.as_deref(),
        Some("Send cancelled; the draft is untouched"),
        "`dispatch_normal_mode`'s own line, moved onto the event"
    );
    assert!(
        matches!(
            fixture.app.status_log.back().map(|entry| &entry.level),
            Some(StatusLevel::Info)
        ),
        "nothing went wrong, so it is not a warning"
    );
    assert!(fixture.app.hold.is_none(), "and there is nothing to cancel");
}

/// A hold this client is not showing is applied all the same.
///
/// Another window's send is this window's countdown too, because either window
/// may cancel it. A client that ignored a hold it did not start would show an
/// empty status line beside a `u` that cancels something invisible.
#[test]
fn a_hold_another_client_armed_is_shown_here_too() {
    let mut fixture = Fixture::new();
    let elsewhere = HoldStatus {
        operation_id: "op-from-the-other-window".to_string(),
        origin: "gui".to_string(),
        ..hold_status(HOLD_SECS)
    };

    let applied = fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &elsewhere));

    assert_eq!(
        applied,
        Applied::Hold("op-from-the-other-window".to_string())
    );
    assert_eq!(
        fixture.app.hold.as_ref().map(|hold| hold.origin.as_str()),
        Some("gui"),
        "the payload says which client asked, and the model keeps it"
    );
}

// ---------------------------------------------------------------------------
// (d) The `u` key, and the method it reaches
// ---------------------------------------------------------------------------

/// `u` while a hold is showing asks the daemon to cancel it.
///
/// It stays hand-dispatched ahead of the KEYMAP table, exactly as #0090 wrote
/// it, so it neither collides with the Message-context `u` (toggle read) once
/// the window has fired nor needs a website key-table entry. What changes is
/// the body: clearing a local slot becomes an action, because cancelling is
/// now a round trip and `dispatch_normal_mode` has no door.
#[test]
fn u_while_a_hold_is_showing_queues_the_cancel() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));

    let message = fixture.app.handle_key(key('u'));

    assert!(message.is_none(), "`u` is not a message, it is an action");
    assert!(
        fixture
            .app
            .pending_actions
            .iter()
            .any(|action| matches!(action, Action::CancelHeldSend)),
        "the cancel is queued for the dispatcher, not performed on the spot"
    );
    assert!(
        fixture.app.hold.is_some(),
        "and the countdown stays up until the daemon says it is cancelled: a client that \
         cleared it optimistically would hide a hold whose cancel was refused"
    );
}

/// `u` with no hold is the key it has always been.
///
/// The guard is `app.hold.is_some()` and nothing else, so a window that never
/// sent anything keeps its Message-context `u`.
#[test]
fn u_with_no_hold_is_not_a_cancel() {
    let mut fixture = Fixture::new();

    fixture.app.handle_key(key('u'));

    assert!(
        !fixture
            .app
            .pending_actions
            .iter()
            .any(|action| matches!(action, Action::CancelHeldSend)),
        "nothing is holding, so nothing is cancelled"
    );
}

/// The new action is daemon-routed, and it suspends nothing.
#[test]
fn cancel_held_send_routes_to_the_daemon() {
    assert_eq!(
        route(&Action::CancelHeldSend),
        ActionRoute::Daemon(&["send.cancel_hold"]),
        "the `u` key is a method call now"
    );
    assert!(
        !Action::CancelHeldSend.suspends_terminal(),
        "cancelling a hold opens no editor"
    );
}

/// Dispatching it calls `send.cancel_hold` with the operation id the countdown
/// carries, and nothing else.
///
/// The id rather than the account: a hold is addressed by the operation the
/// send answered with, which is what makes cancellation from a window that did
/// not send it possible at all.
#[test]
fn dispatching_the_cancel_names_the_operation_the_countdown_carries() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));
    let recorder = Recorder::answering(json!({"cancelled": true, "operation_id": OPERATION}));

    let handled = dispatch(&mut fixture.app, &recorder, &Action::CancelHeldSend);

    assert!(handled, "the dispatcher owns this action end to end");
    assert_eq!(
        recorder.only_call("send.cancel_hold"),
        json!({"operation_id": OPERATION}),
        "one parameter, and it is the hold's own id"
    );
}

/// A cancel with nothing to cancel calls nothing.
#[test]
fn dispatching_the_cancel_with_no_hold_calls_nothing() {
    let mut fixture = Fixture::new();
    let recorder = Recorder::answering(json!({}));

    dispatch(&mut fixture.app, &recorder, &Action::CancelHeldSend);

    assert!(
        recorder.calls().is_empty(),
        "there is no operation to name, so there is no call to make"
    );
}

// ---------------------------------------------------------------------------
// (e) Quitting, and the send that asks for a hold
// ---------------------------------------------------------------------------

/// Quitting mid-hold is allowed, because the hold is not this process's to
/// lose.
///
/// The refusal of #0090 existed because `app.held_send` died with the window.
/// The daemon owns the timer now, and the plan fixes what happens when the
/// last window goes: *"the daemon cancels the hold and leaves the draft
/// approved"* (P6-U3), which `tests/phase5_undo_send_hold.rs` already asserts
/// from the outside. A TUI that still refused would make that rule
/// unreachable.
#[test]
fn quitting_mid_hold_is_allowed_now_that_the_daemon_owns_the_timer() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .apply_event(&event(KIND_SEND_HOLD_STARTED, &hold_status(HOLD_SECS)));

    fixture.app.update(Message::Quit);

    assert!(
        !fixture.app.running,
        "the window closes; the hold is the daemon's problem and it has a rule for it"
    );
}

/// The TUI's send asks for the hold; nothing else does.
///
/// `hold: true` and not a number of seconds: the window is
/// `email.send_hold_secs`, the daemon reads it, and a client that computed it
/// would be the hold living in the client again under a longer name.
#[test]
fn send_approved_from_the_tui_asks_for_the_hold() {
    let mut fixture = Fixture::new();
    fixture.on_drafts();
    let recorder = Recorder::answering(json!({"operation_id": OPERATION}));

    dispatch(&mut fixture.app, &recorder, &Action::SendApproved);

    assert_eq!(
        recorder.only_call("send.approved"),
        json!({"account": ACCOUNT, "hold": true}),
        "the account it always named, and the hold the daemon now owns"
    );
}

// ---------------------------------------------------------------------------
// (f) The residue
// ---------------------------------------------------------------------------

/// The five source files the hold lives in today, and the symbols that mean it
/// is still there.
///
/// `held_send` is the slot, `HeldSend` its type, `fire_held_send` the tail that
/// hands a parked send to a thread, and `send_one_draft` the blocking call
/// around `crate::send::send_draft` - the enclosing function of the one row
/// `src/tui/actions_tests.rs`'s `TUI_ACTION_ENGINE_RESIDUE` still permits, and
/// the last engine call the TUI's action layer makes. When the hold moves,
/// every one of them goes with it, and the TUI keeps only what it renders.
///
/// `send_draft(` itself is deliberately not a needle here. It is
/// `TUI_ACTION_ENGINE_RESIDUE`'s, whose scan attributes a call to its
/// *enclosing function* and drops `#[cfg(test)]` modules, so it tells the
/// fire path apart from the four engine tests that call
/// `crate::send::send_draft` directly inside `src/tui/actions.rs`'s own test
/// module. Those four are about the outbox and the two refusals, they are not
/// the hold, and this unit does not ask for them. Striking the
/// `send_one_draft -> send_draft(` row from that table is what makes them the
/// only ones left.
const HOLD_RESIDUE: [(&str, &[&str]); 5] = [
    (
        "src/tui/actions.rs",
        &["HeldSend", "held_send", "fire_held_send", "send_one_draft"],
    ),
    ("src/tui/mod.rs", &["held_send", "fire_held_send"]),
    ("src/tui/app/mod.rs", &["HeldSend", "held_send"]),
    ("src/tui/app/types.rs", &["HeldSend", "held_send"]),
    ("src/tui/app/keys.rs", &["held_send"]),
];

/// The hold's machinery is gone from the TUI, which keeps only the rendered
/// countdown.
///
/// A scan and not a compile error, because the compiler is happy with a TUI
/// that keeps a second, private hold beside the daemon's: two timers for one
/// send is exactly the failure this unit exists to prevent, and the second one
/// would only show up as a message sent twice. `src/tui/hold_tests.rs` is not
/// scanned - it is this file, and it names every symbol on purpose.
#[test]
fn the_hold_machinery_is_gone_from_the_tui() {
    let mut found: Vec<String> = Vec::new();
    for (file, needles) in HOLD_RESIDUE {
        let path = Path::new(file);
        assert!(
            path.exists(),
            "{file} is named by HOLD_RESIDUE and is not in the tree; strike the row or fix the \
             path in the same commit"
        );
        let source = std::fs::read_to_string(path).expect("a source file this crate owns");
        for (number, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in needles {
                if mentions(code, needle) {
                    found.push(format!("{file}:{}: {needle}", number + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "the undo-send hold still has machinery in the TUI:\n{}\n\nThe hold is the daemon's: \
         the TUI keeps `App::hold`, the status line and the `u` key, and nothing that counts, \
         parks or sends. Strike the `send_draft(` row from TUI_ACTION_ENGINE_RESIDUE in the \
         same commit.",
        found.join("\n")
    );
}

/// Whether `code` mentions `needle` as a symbol of its own.
///
/// A plain `contains` would report `Action::CancelHeldSend` as the `HeldSend`
/// type and `fire_held_send` as the `held_send` slot, so an occurrence counts
/// only when the character before it cannot be part of a Rust identifier.
/// That is why `fire_held_send` and `send_one_draft` are needles in their own
/// right rather than suffixes of another one.
fn mentions(code: &str, needle: &str) -> bool {
    code.match_indices(needle).any(|(at, _)| {
        at == 0
            || !code[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// An `App` that has bootstrapped, over a data root of its own.
///
/// The field order is the drop order: the `App` goes before the directory its
/// paths resolve under.
struct Fixture {
    app: App,
    _data: crate::config::test_env::TestDataDir,
}

impl Fixture {
    /// A watermarked `App` for [`ACCOUNT`], with the four mailbox roles and
    /// nothing in them.
    fn new() -> Fixture {
        let data = crate::config::test_env::TestDataDir::new();
        let mut app = App::default_for_tests();
        app.account_config = AccountConfig {
            name: ACCOUNT.to_string(),
            ..Default::default()
        };
        app.mailboxes = build_mailboxes(&app.account_config);
        app.mailbox_counts = vec![0; app.mailboxes.len()];
        app.email_cache = vec![None; app.mailboxes.len()];
        app.apply_bootstrap(&Bootstrap {
            instance_id: INSTANCE.to_string(),
            revision: WATERMARK,
            ..Bootstrap::default()
        });
        Fixture { app, _data: data }
    }

    /// Open the Drafts mailbox and give the account a transport, which is what
    /// `send_approved` refuses without.
    fn on_drafts(&mut self) {
        self.app.active_mailbox = self
            .app
            .find_mailbox_by_kind(MailboxKind::Drafts)
            .expect("the Drafts mailbox is one of the four roles");
        // `SmtpConfig` holds a password and deliberately has no `Debug`, so it
        // is built by hand rather than defaulted.
        self.app.smtp_config = Some(SmtpConfig {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: "me@example.com".to_string(),
            password: "secret".to_string(),
            default_from: "me@example.com".to_string(),
            accept_invalid_certs: false,
            auth_method: crate::config::AuthMethod::Password,
        });
    }
}

/// A door that records what it was asked and answers one canned value.
///
/// No dispatcher: every row in sections (d) and (e) is about the call the TUI
/// makes, and the daemon's own answer to it is `tests/daemon_send_hold.rs`'s
/// business, over a socket, against a real scheduler.
struct Recorder {
    answer: Value,
    calls: RefCell<Vec<(String, Value)>>,
}

impl Recorder {
    fn answering(answer: Value) -> Recorder {
        Recorder {
            answer,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.borrow().clone()
    }

    /// The parameters of the one call to `method`, asserting it was the only
    /// call of any kind.
    fn only_call(&self, method: &str) -> Value {
        let calls = self.calls();
        assert_eq!(
            calls.len(),
            1,
            "expected one call to {method}, got {:?}",
            calls
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(calls[0].0, method, "the action called the wrong method");
        calls[0].1.clone()
    }
}

impl Queries for Recorder {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls.borrow_mut().push((method.to_string(), params));
        Ok(self.answer.clone())
    }
}

/// The hold every row is about, with `remaining` seconds left of it.
fn hold_status(remaining: u64) -> HoldStatus {
    HoldStatus {
        operation_id: OPERATION.to_string(),
        account: ACCOUNT.to_string(),
        draft_id: "freigabe".to_string(),
        subject: "Angebot".to_string(),
        hold_secs: HOLD_SECS,
        remaining_secs: remaining,
        fires_at: "2026-09-11T08:00:20Z".to_string(),
        origin: "tui".to_string(),
    }
}

/// One hold event, as the daemon frames it: the payload *is* the status.
///
/// The revision walks forward by itself, because a row applies several events
/// in sequence and one at or below the watermark would be dropped as a
/// duplicate.
fn event(kind: &str, status: &HoldStatus) -> EventEnvelope {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(WATERMARK + 1);
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision: NEXT.fetch_add(1, Ordering::Relaxed),
        kind: kind.to_string(),
        payload: serde_json::to_value(status).expect("a HoldStatus serialises"),
    }
}

/// One unmodified character key.
fn key(c: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(c))
}
