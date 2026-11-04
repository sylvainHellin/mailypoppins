//! The calendar reminders in the TUI (PERSO-255): the hint bar's count, the
//! reminders overlay and its snooze and dismiss, and a fired reminder on the
//! status line and as a notification.
//!
//! The daemon half is a canned [`Queries`] that holds the waiting list and
//! answers the three reminder calls the way the daemon does; the daemon's own
//! behaviour is `tests/daemon_reminder_slice.rs`'s. The frame is built from
//! [`calendar_fixture`]'s pinned zone with all-day reminders, so no line reads
//! the clock.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent};
use insta::assert_snapshot;
use serde_json::{json, Value};

use crate::tui::app::{App, Overlay, View};
use crate::tui::events::Applied;
use crate::tui::queries::Queries;
use crate::tui::session::Session;
use crate::tui::ui::golden_frames::{calendar_fixture, frame_snapshot, HEIGHT, WIDTH};

use super::daemon::CallLog;

fn reminder(event: &str, summary: &str, start: &str, all_day: bool, presenter: Value) -> Value {
    json!({
        "account": "evoqs",
        "event": event,
        "summary": summary,
        "start": start,
        "all_day": all_day,
        "location": "Room 2.14",
        "minutes_before": 15,
        "presenter": presenter,
    })
}

/// The daemon's reminder calls over a waiting list: a snooze or a dismissal
/// takes the reminder off it.
struct Reminding {
    log: CallLog,
    waiting: Arc<Mutex<Vec<Value>>>,
}

impl Queries for Reminding {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.log
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        let mut waiting = self.waiting.lock().unwrap();
        match method {
            "calendar.reminders" => Ok(json!({"reminders": *waiting})),
            "calendar.dismiss_reminder" | "calendar.snooze_reminder" => {
                waiting.retain(|r| r["event"] != params["event"]);
                Ok(
                    json!({"account": params["account"], "event": params["event"],
                          "until": "2026-08-04T09:05:00Z"}),
                )
            }
            other => anyhow::bail!("no {other} here"),
        }
    }
}

fn app_with(waiting: Vec<Value>) -> (App, CallLog) {
    let log = CallLog::default();
    let mut app = calendar_fixture();
    let (log_in, waiting) = (log.clone(), Arc::new(Mutex::new(waiting)));
    app.session = Some(Session::serving(move || Reminding {
        log: log_in,
        waiting,
    }));
    app.apply_bootstrap(&mp_protocol::state::Bootstrap {
        instance_id: "tui-test".to_string(),
        revision: 1,
        ..Default::default()
    });
    (app, log)
}

fn methods(log: &CallLog) -> Vec<(String, Value)> {
    log.lock().unwrap().clone()
}

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

fn fired(app: &mut App, revision: u64, payload: Value) -> Applied {
    app.apply_event(&mp_protocol::EventEnvelope {
        instance_id: "tui-test".to_string(),
        revision,
        kind: mp_protocol::events::KIND_CALENDAR_REMINDER.to_string(),
        payload,
    })
}

/// The bootstrap reads the waiting reminders, the hint bar counts them, and
/// `R` opens the overlay over them, where the hint bar gives way to the
/// overlay's own footer.
#[test]
fn golden_reminders_overlay() {
    let (mut app, log) = app_with(vec![
        reminder(
            "ev-loc",
            "LOC Day",
            "2026-08-05T22:00:00Z",
            true,
            json!(null),
        ),
        reminder(
            "ev-hol",
            "Holiday",
            "2026-08-06T22:00:00Z",
            true,
            json!(null),
        ),
    ]);
    assert_eq!(methods(&log)[0], ("calendar.reminders".into(), json!({})));
    assert_eq!(app.reminders.len(), 2);
    let frame = frame_snapshot(&mut app, WIDTH, HEIGHT);
    assert!(frame.contains("\u{25f7} 2 reminders (R)"), "{frame}");
    app.handle_key(key('R'));
    assert!(matches!(app.overlay, Overlay::Reminders(_)));
    assert_snapshot!(frame_snapshot(&mut app, WIDTH, HEIGHT));
}

/// `s` snoozes the reminder under the cursor for five minutes, `d`
/// dismisses the next, each through the daemon and the list read again, and
/// the overlay closes once nothing waits; `Enter` opens the event.
#[test]
fn the_overlay_snoozes_dismisses_and_opens() {
    let (mut app, log) = app_with(vec![
        reminder(
            "ev-a",
            "Standup",
            "2026-08-04T07:00:00Z",
            false,
            json!(null),
        ),
        reminder("ev-b", "Review", "2026-08-04T08:00:00Z", false, json!(null)),
    ]);
    app.handle_key(key('R'));
    app.handle_key(key('s'));
    assert!(methods(&log).contains(&(
        "calendar.snooze_reminder".into(),
        json!({"account": "evoqs", "event": "ev-a", "minutes": 5})
    )));
    assert_eq!(app.reminders.len(), 1);
    assert_eq!(
        app.status_message.as_deref(),
        Some("Snoozed Standup for 5 min")
    );
    app.handle_key(key('d'));
    assert!(methods(&log).contains(&(
        "calendar.dismiss_reminder".into(),
        json!({"account": "evoqs", "event": "ev-b"})
    )));
    assert!(app.reminders.is_empty());
    assert!(matches!(app.overlay, Overlay::None), "nothing left to show");
    app.handle_key(key('R'));
    assert_eq!(app.status_message.as_deref(), Some("No reminders waiting"));

    // Enter goes to the event's day in the Calendar view, which reads it.
    let (mut app, log) = app_with(vec![reminder(
        "ev-jour-1",
        "Jour fixe",
        "2026-08-04T08:00:00Z",
        false,
        json!(null),
    )]);
    app.view = View::Mail;
    app.handle_key(key('R'));
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.view, View::Calendar);
    assert_eq!(
        app.calendar_view.anchor,
        chrono::NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()
    );
    assert!(methods(&log)
        .iter()
        .any(|(method, _)| method == "calendar.view"));
}

/// A fired reminder joins the list and the status line; it raises a
/// notification only when notifications are on and this TUI presents.
#[test]
fn a_fired_reminder_notifies_only_as_the_presenter() {
    let (mut app, _) = app_with(Vec::new());
    app.global_config.notifications = true;
    let capture = mp_core::notify::test_sink::capture();
    let applied = fired(
        &mut app,
        2,
        reminder(
            "ev-a",
            "Standup",
            "2026-08-04T07:00:00Z",
            false,
            json!("gui"),
        ),
    );
    assert_eq!(applied, Applied::Ignored);
    assert_eq!(app.reminders.len(), 1);
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|line| line.contains("Reminder: Standup")),
        "{:?}",
        app.status_message
    );
    assert!(capture.shown().is_empty(), "the desktop app presents");

    fired(
        &mut app,
        3,
        reminder(
            "ev-a",
            "Standup",
            "2026-08-04T07:00:00Z",
            false,
            json!("tui"),
        ),
    );
    assert_eq!(app.reminders.len(), 1, "a snooze that ended replaces it");
    let shown = capture.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].0, "Standup");
    assert!(shown[0].1.ends_with(", Room 2.14"), "{shown:?}");

    app.global_config.notifications = false;
    fired(
        &mut app,
        4,
        reminder(
            "ev-b",
            "Review",
            "2026-08-04T08:00:00Z",
            false,
            json!("tui"),
        ),
    );
    assert_eq!(capture.shown().len(), 1, "notifications are off");
    assert_eq!(app.reminders.len(), 2);
}
