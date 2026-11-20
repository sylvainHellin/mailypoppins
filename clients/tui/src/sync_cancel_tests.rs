//! Cancelling a running sync from the TUI (PERSO-321).
//!
//! `sx` sends `operation.cancel` for every sync this window started and is
//! still waiting for, the call the CLI's `^C` and the desktop's Cancel make.
//! The daemon settles the pass `cancelled` at once; its finish ends the
//! status line with what stays, and is not a failed sync. A cancel the daemon
//! answers too late says nothing was cancelled, and with no sync of this
//! window running the key says so.
//!
//! The daemon is a door that starts each pass under a fixed id and answers
//! the cancel the way `src/daemon/operations.rs` does.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::app::{App, StatusLevel};
use crate::commands::dispatch;
use crate::queries::Queries;

const INSTANCE: &str = "sync-cancel";
const ACCOUNT: &str = "work";
const FULL: &str = "op-full";

/// How the door answers `operation.cancel`.
#[derive(Clone, Copy)]
enum CancelAnswer {
    /// The pass was running: `{operation_id, state: "cancelled"}`.
    Cancelled,
    /// The pass had already ended: `-32602` naming it, as the registry
    /// refuses a terminal operation.
    TooLate,
}

/// A door that starts a full sync as [`FULL`], answers the cancel as told,
/// and records every call.
struct Daemon {
    cancel: CancelAnswer,
    calls: RefCell<Vec<(String, Value)>>,
}

impl Daemon {
    fn answering(cancel: CancelAnswer) -> Daemon {
        Daemon {
            cancel,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn methods(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(m, _)| m.clone()).collect()
    }
}

impl Queries for Daemon {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls
            .borrow_mut()
            .push((method.to_string(), params.clone()));
        match (method, self.cancel) {
            ("sync.full", _) => Ok(json!({"operation_id": FULL})),
            ("operation.cancel", CancelAnswer::Cancelled) => {
                Ok(json!({"operation_id": params["operation_id"], "state": "cancelled"}))
            }
            ("operation.cancel", CancelAnswer::TooLate) => Err(mp_client::session::Refused {
                method: method.to_string(),
                error: mp_protocol::RpcError {
                    code: -32602,
                    message: format!("operation {FULL} has already succeeded"),
                    data: Some(json!({"operation_id": FULL, "state": "succeeded"})),
                },
            }
            .into()),
            (other, _) => Err(anyhow::anyhow!("unexpected {other}")),
        }
    }
}

/// An app on [`ACCOUNT`], bootstrapped at revision 1.
fn app() -> App {
    let mut app = App::default_for_tests();
    app.account_config.name = ACCOUNT.to_string();
    app.accounts = vec![crate::app::AccountState::for_tests(ACCOUNT)];
    app.active_account = 0;
    app.apply_bootstrap(&Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision: 1,
        ..Bootstrap::default()
    });
    app.pending_actions.clear();
    app
}

/// Press `keys` and run what they queued through `daemon`.
fn press(app: &mut App, daemon: &Daemon, keys: &str) {
    for c in keys.chars() {
        app.handle_key(KeyEvent::from(KeyCode::Char(c)));
    }
    while let Some(action) = app.pending_actions.pop_front() {
        dispatch(app, daemon, &action);
    }
}

/// The finish the registry publishes for a cancelled operation.
fn cancelled_finish(revision: u64) -> EventEnvelope {
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision,
        kind: "operation.finished".to_string(),
        payload: json!({
            "operation_id": FULL,
            "state": "cancelled",
            "error": {
                "code": -32008,
                "message": format!("operation {FULL} was cancelled"),
                "data": {"operation_id": FULL},
            },
        }),
    }
}

/// The level of the newest line in the activity log.
fn last_level(app: &App) -> &StatusLevel {
    &app.status_log.back().expect("a status line").level
}

/// `sS` then `sx`: the cancel names the full sync's id, and the cancelled
/// finish ends the line with what stays, as a warning and not as a failed
/// sync, which leaves the account's sync health alone.
#[test]
fn sx_cancels_the_full_sync_this_window_started_and_its_end_says_what_stays() {
    let daemon = Daemon::answering(CancelAnswer::Cancelled);
    let mut app = app();
    let health = format!("{:?}", app.accounts[0].sync_health);

    press(&mut app, &daemon, "sS");
    assert_eq!(daemon.methods(), ["sync.full"]);
    press(&mut app, &daemon, "sx");
    let calls = daemon.calls.borrow().clone();
    assert_eq!(
        calls.last(),
        Some(&(
            "operation.cancel".to_string(),
            json!({"operation_id": FULL})
        )),
        "{calls:?}"
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Cancelling the sync of work...")
    );

    app.apply_event(&cancelled_finish(2));
    assert_eq!(
        app.status_message.as_deref(),
        Some(
            "Sync of work cancelled: what it already ingested stays, \
             and the next sync goes on from there"
        )
    );
    assert!(
        matches!(last_level(&app), StatusLevel::Warning),
        "{:?}",
        last_level(&app)
    );
    assert_eq!(app.bg_count, 0, "the pass is no longer awaited");
    assert_eq!(
        format!("{:?}", app.accounts[0].sync_health),
        health,
        "a cancelled sync is not a failed one"
    );

    // Nothing of this window runs any more.
    press(&mut app, &daemon, "sx");
    assert_eq!(
        app.status_message.as_deref(),
        Some("No sync to cancel: none started from this window is running")
    );
}

/// `sx` with nothing running says so and calls nothing, whatever another
/// client may be syncing: this window cancels only what it started.
#[test]
fn sx_with_no_sync_of_this_window_running_says_so_and_calls_nothing() {
    let daemon = Daemon::answering(CancelAnswer::Cancelled);
    let mut app = app();
    press(&mut app, &daemon, "sx");
    assert_eq!(daemon.methods(), Vec::<String>::new());
    assert_eq!(
        app.status_message.as_deref(),
        Some("No sync to cancel: none started from this window is running")
    );
}

/// A cancel the daemon refuses because the pass already ended says nothing
/// was cancelled, and is no error.
#[test]
fn a_cancel_that_came_too_late_says_nothing_was_cancelled() {
    let daemon = Daemon::answering(CancelAnswer::TooLate);
    let mut app = app();
    press(&mut app, &daemon, "sS");
    press(&mut app, &daemon, "sx");
    assert_eq!(
        app.status_message.as_deref(),
        Some("The sync of work had already ended: nothing was cancelled")
    );
    assert!(
        matches!(last_level(&app), StatusLevel::Info),
        "{:?}",
        last_level(&app)
    );
}
