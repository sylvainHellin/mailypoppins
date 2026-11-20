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
const QUICK: &str = "op-quick";

/// How the door answers `operation.cancel`.
#[derive(Clone, Copy)]
enum CancelAnswer {
    /// The pass was running: `{operation_id, state: "cancelled"}`.
    Cancelled,
    /// The pass had already ended: `-32602` naming it, as the registry
    /// refuses a terminal operation.
    TooLate,
}

/// A door that starts a full sync as [`FULL`] and a quick one as [`QUICK`],
/// answers the cancel as told, and records every call.
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
            ("sync.quick", _) => Ok(json!({"operation_id": QUICK})),
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

/// `sS` and `ss` name the account on their progress line, as the startup
/// fetch's `Quick sync (<account>)...` does.
#[test]
fn both_sync_keys_name_the_account_they_sync() {
    let daemon = Daemon::answering(CancelAnswer::Cancelled);
    let mut app = app();
    press(&mut app, &daemon, "sS");
    assert_eq!(app.status_message.as_deref(), Some("Full sync (work)..."));

    let mut app = self::app();
    press(&mut app, &daemon, "ss");
    assert_eq!(app.status_message.as_deref(), Some("Quick sync (work)..."));
}

/// `ss` while `sS` runs is parked behind it; `sx` then cancels the full sync
/// and drops the parked quick one, which never starts, since the user asked
/// the syncing to stop.
#[test]
fn sx_cancels_the_running_sync_and_drops_the_one_parked_behind_it() {
    let daemon = Daemon::answering(CancelAnswer::Cancelled);
    let mut app = app();
    press(&mut app, &daemon, "sS");
    press(&mut app, &daemon, "ss");
    assert_eq!(
        app.status_message.as_deref(),
        Some("Quick sync (work) queued (waiting for the current sync)")
    );
    assert!(app.queued_action.is_some(), "the quick sync is parked");

    press(&mut app, &daemon, "sx");
    assert!(app.queued_action.is_none(), "the parked sync is dropped");
    assert_eq!(
        app.status_message.as_deref(),
        Some("Cancelling the sync of work...")
    );
    app.apply_event(&cancelled_finish(2));
    assert_eq!(app.bg_count, 0);
    assert!(
        app.queued_action.is_none(),
        "nothing is left to start once the cancelled sync is over"
    );
    assert_eq!(daemon.methods(), ["sync.full", "operation.cancel"]);
}

/// On an account whose one server is its calendar's (PERSO-318), `sS` runs
/// its calendar sync, and the cancelled end says the client stopped waiting
/// and the calendar sync finishes in the daemon, since it has no point to
/// stop at.
#[test]
fn a_cancelled_calendar_only_sync_says_the_calendar_sync_finishes() {
    let daemon = Daemon::answering(CancelAnswer::Cancelled);
    let mut app = app();
    app.account_config.calendar_backend = Some(mp_core::config::CalendarBackendKind::Ews);
    app.accounts[0].account_config = app.account_config.clone();
    assert!(app.account_config.syncs_only_a_calendar());

    press(&mut app, &daemon, "sS");
    press(&mut app, &daemon, "sx");
    app.apply_event(&cancelled_finish(2));
    assert_eq!(
        app.status_message.as_deref(),
        Some(
            "Stopped waiting for the calendar sync of work: a calendar sync is not cut short, \
             so it finishes in the daemon"
        )
    );
}

/// `sx` with nothing running says so and calls nothing, whatever another
/// client may be syncing: this window names only what it started.
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

/// A sync the daemon cancelled because it was stopping ends on the line that
/// names the stop, after the "Daemon stopping" warning the announcement
/// left: nobody in this window asked for it to stop.
#[test]
fn a_sync_the_daemons_stop_cancelled_says_the_stop_cut_it_short() {
    let daemon = Daemon::answering(CancelAnswer::Cancelled);
    let mut app = app();
    press(&mut app, &daemon, "sS");
    app.apply_event(&EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision: 2,
        kind: "daemon.shutting_down".to_string(),
        payload: json!({"grace_secs": 0, "pending": []}),
    });
    assert_eq!(app.status_message.as_deref(), Some("Daemon stopping"));
    app.apply_event(&cancelled_finish(3));
    assert_eq!(
        app.status_message.as_deref(),
        Some(
            "Sync of work cut short by the daemon stopping: what it already ingested stays, \
             and the next sync goes on from there"
        )
    );
    assert!(
        matches!(last_level(&app), StatusLevel::Warning),
        "{:?}",
        last_level(&app)
    );
}
