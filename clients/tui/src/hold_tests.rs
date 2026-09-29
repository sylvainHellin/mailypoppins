//! Two undo-send holds armed at once.
//!
//! Nothing refuses a second send inside the first one's window, so the model
//! keeps every armed hold by its operation id: an event touches only its own
//! entry, `u` cancels the most recently armed one, and a hold that fires
//! leaves the other one cancellable. `src/tui_tests/hold.rs` is the
//! single-hold contract; this is the row it does not cover.

use std::cell::RefCell;

use serde_json::{json, Value};

use mp_protocol::events::{KIND_SEND_HOLD_FIRED, KIND_SEND_HOLD_STARTED, KIND_SEND_HOLD_TICK};
use mp_protocol::send::HoldStatus;
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::app::{Action, App};
use crate::commands::dispatch;
use crate::queries::Queries;

const INSTANCE: &str = "two-holds";

/// A door that records every call and answers each with an empty object.
#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<(String, Value)>>,
}

impl Queries for Recorder {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls.borrow_mut().push((method.to_string(), params));
        Ok(json!({}))
    }
}

fn hold(operation: &str, remaining: u64, fires_at: &str) -> HoldStatus {
    HoldStatus {
        operation_id: operation.to_string(),
        account: "alice".to_string(),
        draft_id: operation.to_string(),
        subject: operation.to_string(),
        hold_secs: 20,
        remaining_secs: remaining,
        fires_at: fires_at.to_string(),
        origin: "tui".to_string(),
    }
}

/// One hold event; `revision` walks forward so none is dropped as a duplicate.
fn event(revision: u64, kind: &str, status: &HoldStatus) -> EventEnvelope {
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision,
        kind: kind.to_string(),
        payload: serde_json::to_value(status).expect("a HoldStatus serialises"),
    }
}

/// The operation id `u` would cancel, read off the call the dispatcher makes.
fn cancelled_by_u(app: &mut App) -> Value {
    app.pending_actions.clear();
    app.handle_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Char('u'),
    ));
    assert!(
        app.pending_actions
            .iter()
            .any(|action| matches!(action, Action::CancelHeldSend)),
        "`u` is caught as a cancel while a hold is armed"
    );
    let recorder = Recorder::default();
    dispatch(app, &recorder, &Action::CancelHeldSend);
    let calls = recorder.calls.borrow();
    assert_eq!(calls.len(), 1, "one cancel call, got {calls:?}");
    assert_eq!(calls[0].0, "send.cancel_hold");
    calls[0].1["operation_id"].clone()
}

#[test]
fn u_cancels_the_latest_hold_and_the_other_survives_when_the_first_fires() {
    let _data = mp_core::config::test_env::TestDataDir::new();
    let mut app = App::default_for_tests();
    app.apply_bootstrap(&Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision: 1,
        ..Bootstrap::default()
    });

    let a = hold("op-a", 20, "2026-09-11T08:00:20Z");
    let b = hold("op-b", 20, "2026-09-11T08:00:25Z");
    app.apply_event(&event(2, KIND_SEND_HOLD_STARTED, &a));
    app.apply_event(&event(3, KIND_SEND_HOLD_STARTED, &b));
    // A ticks after B started: it must neither take the slot nor the line.
    app.apply_event(&event(4, KIND_SEND_HOLD_TICK, &hold("op-a", 15, &a.fires_at)));

    assert_eq!(app.holds.len(), 2, "both holds are armed");
    assert_eq!(
        app.holds[0].remaining_secs, 15,
        "A's tick updated A's own entry"
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Sending in 20s (press u to undo)"),
        "the line keeps B's countdown, the hold `u` cancels"
    );
    assert_eq!(cancelled_by_u(&mut app), json!("op-b"), "`u` cancels B");

    app.apply_event(&event(5, KIND_SEND_HOLD_FIRED, &hold("op-a", 0, &a.fires_at)));

    assert_eq!(
        app.hold.as_ref().map(|hold| hold.operation_id.as_str()),
        Some("op-b"),
        "A firing leaves B armed"
    );
    assert_eq!(cancelled_by_u(&mut app), json!("op-b"), "and still cancellable");
}
