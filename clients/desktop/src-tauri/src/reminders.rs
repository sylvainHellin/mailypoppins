//! The calendar reminders (CAL-15, INT-07, PERSO-255).
//!
//! The daemon owns the reminders: it fires each at its due minute as
//! `calendar.reminder`, keeps what was dismissed or snoozed, and names the
//! presenter, the one connection that raises the system notification (this
//! app's whenever it is connected, as its bootstrap named it). This layer
//! raises that notification from
//! Rust through `tauri-plugin-notification`, so the webview holds no
//! notification permission; delivery is a best-effort view of the daemon's
//! state.
//!
//! The notification carries no action buttons: on macOS the plugin delivers
//! through `NSUserNotificationCenter`, where two buttons collapse into an
//! "Options" menu and a click is lost once the app restarts. A click on its
//! body arrives as the action `tap` ([`install`]), which shows, unminimizes
//! and focuses the main window and asks the frontend to open the event
//! ([`OPEN_EVENT`]), except for the palette's test notification, which names
//! no account and only brings the window forward. Snooze and Dismiss live in the app's reminder banner,
//! which calls `calendar.snooze_reminder` and `calendar.dismiss_reminder`
//! through [`reminder_snooze`] and [`reminder_dismiss`]. macOS shows no
//! banner for the frontmost app, which is why the in-app banner lists every
//! waiting reminder whatever the notification did.
//!
//! At launch the reminders already waiting are notified once
//! ([`notify_waiting`]), since they fired before this app could hear them.
//! [`reminder_notify_test`] raises a notification of a made-up reminder, the
//! palette's way to check that the system shows this app's notifications.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_notification::NotificationExt;

use mp_client::queries;
use mp_protocol::events::KIND_CALENDAR_REMINDER;
use mp_protocol::reminder::{CalendarReminder, PRESENTER_GUI};
use mp_protocol::EventEnvelope;

use crate::calendar::daemon_sentence;
use crate::commands::with_door;
use crate::error::{Addressing, GuiError};
use crate::session::{Budgeted, Door, SessionHandle};

/// The app event a click on a reminder notification emits, with a
/// [`ReminderTarget`]: the frontend opens the event in the Calendar view.
pub const OPEN_EVENT: &str = "reminder:open";

/// One `calendar.reminders` read or decision: the daemon's store, no server.
const BUDGET: Duration = Duration::from_secs(10);

/// How long the launch waits for the session before it gives up notifying
/// the reminders already waiting; the banner still lists them once it reads.
const LAUNCH_WAIT: Duration = Duration::from_secs(30);

/// The event a notification stands for, carried in its extra payload and in
/// the [`OPEN_EVENT`] it emits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ReminderTarget {
    pub account: String,
    pub event: String,
    /// RFC 3339, the event's start, which picks the day to show.
    pub start: String,
}

impl ReminderTarget {
    fn of(reminder: &CalendarReminder) -> ReminderTarget {
        ReminderTarget {
            account: reminder.account.clone(),
            event: reminder.event.clone(),
            start: reminder.start.clone(),
        }
    }
}

/// Raise the system notification of `reminder`.
pub fn notify<R: Runtime>(app: &AppHandle<R>, reminder: &CalendarReminder) {
    let (title, body) = mp_client::format::reminder_notification(reminder);
    let shown = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .extra("reminder", ReminderTarget::of(reminder))
        .show();
    match shown {
        Ok(()) => tracing::info!(
            "[reminders] notified {}/{}",
            reminder.account,
            reminder.event
        ),
        Err(e) => tracing::warn!(
            "[reminders] the notification of {} failed: {e}",
            reminder.event
        ),
    }
}

/// Whether `reminder` is this app's to notify: the daemon names
/// `connection`, this app's, as its presenter.
fn presents(reminder: &CalendarReminder, connection: Option<u64>) -> bool {
    connection.is_some() && reminder.presenter_connection == connection
}

/// A `calendar.reminder` this app's `connection` presents is notified; every
/// other event, and a reminder another connection presents, is not.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, connection: Option<u64>, event: &EventEnvelope) {
    if event.kind != KIND_CALENDAR_REMINDER {
        return;
    }
    match serde_json::from_value::<CalendarReminder>(event.payload.clone()) {
        Ok(reminder) if presents(&reminder, connection) => notify(app, &reminder),
        Ok(_) => {}
        Err(e) => tracing::warn!("[reminders] a calendar.reminder did not decode: {e}"),
    }
}

/// The target a click on a notification names, from its extra payload.
fn clicked(performed: &tauri_plugin_notification::ActionPerformed) -> Option<ReminderTarget> {
    if performed.action_id() != "tap" {
        return None;
    }
    let extra = performed.notification()?.extra().get("reminder")?.clone();
    serde_json::from_value(extra).ok()
}

/// Bring the main window forward: shown, unminimized and focused.
fn bring_forward<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The event a click on `target`'s notification opens: none for the
/// palette's test notification, whose made-up event names no account.
fn to_open(target: ReminderTarget) -> Option<ReminderTarget> {
    (!target.account.is_empty()).then_some(target)
}

/// Handle the clicks on this app's notifications: the window comes forward
/// on the reminder's event, and on its own for new mail
/// ([`crate::new_mail`]). Registered once, before the first notification,
/// since the plugin reports nothing about one shown before a handler existed.
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    let registered = app.notification().on_action(move |performed| {
        if crate::new_mail::is_new_mail(performed) {
            bring_forward(&handle);
            return tracing::info!("[mail] a new-mail notification brought the window forward");
        }
        let Some(target) = clicked(performed) else {
            return;
        };
        bring_forward(&handle);
        let Some(target) = to_open(target) else {
            tracing::info!("[reminders] the test notification brought the window forward");
            return;
        };
        tracing::info!(
            "[reminders] opened {}/{} from its notification",
            target.account,
            target.event
        );
        if let Err(e) = handle.emit(OPEN_EVENT, target) {
            tracing::warn!("[reminders] could not ask the window to open the event: {e}");
        }
    });
    if let Err(e) = registered {
        tracing::warn!("[reminders] notification clicks will not open the event: {e}");
    }
}

/// Notify, once the session is up, the reminders that fired before this app
/// started; off the setup path, on a thread of its own.
pub fn notify_waiting<R: Runtime>(app: &AppHandle<R>, session: SessionHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("mp-desktop-reminders".into())
        .spawn(move || {
            let Ok(door) = session.door(LAUNCH_WAIT) else {
                return;
            };
            match calendar_reminders_on(&door) {
                Ok(waiting) => waiting.iter().for_each(|reminder| notify(&app, reminder)),
                Err(e) => {
                    tracing::warn!("[reminders] the waiting reminders could not be read: {e}")
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("[reminders] could not start the launch notifications: {e}");
    }
}

/// The reminders that fired and wait, every account's.
pub fn calendar_reminders_on(door: &Door) -> Result<Vec<CalendarReminder>, GuiError> {
    let q = Budgeted {
        door,
        budget: BUDGET,
    };
    queries::calendar_reminders(&q, None).map_err(|e| GuiError::from_call(&e, Addressing::Resource))
}

/// Dismiss one reminder; the daemon's `calendar.reminders_changed` reads the
/// banner again in every window.
pub fn reminder_dismiss_on(door: &Door, account: &str, event: &str) -> Result<(), GuiError> {
    let q = Budgeted {
        door,
        budget: BUDGET,
    };
    queries::calendar_dismiss_reminder(&q, account, event)
        .map_err(|e| GuiError::from_call(&e, Addressing::Params))
        .map_err(daemon_sentence)
}

/// Snooze one reminder for `minutes`.
pub fn reminder_snooze_on(
    door: &Door,
    account: &str,
    event: &str,
    minutes: u32,
) -> Result<(), GuiError> {
    let q = Budgeted {
        door,
        budget: BUDGET,
    };
    queries::calendar_snooze_reminder(&q, account, event, minutes)
        .map(|_| ())
        .map_err(|e| GuiError::from_call(&e, Addressing::Params))
        .map_err(daemon_sentence)
}

/// A made-up reminder, five minutes out, for [`reminder_notify_test`].
fn test_reminder() -> CalendarReminder {
    let start = time::OffsetDateTime::now_utc() + time::Duration::minutes(5);
    CalendarReminder {
        account: String::new(),
        event: "test".into(),
        summary: "mailypoppins test reminder".into(),
        start: start
            .replace_nanosecond(0)
            .ok()
            .and_then(|start| {
                start
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_default(),
        all_day: false,
        location: Some("a click brings this window forward".into()),
        online_meeting_url: None,
        minutes_before: 5,
        presenter: Some(PRESENTER_GUI.into()),
        presenter_connection: None,
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// The reminders that fired and wait.
#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_reminders(
    session: State<'_, SessionHandle>,
) -> Result<Vec<CalendarReminder>, GuiError> {
    with_door(&session, |_, door| calendar_reminders_on(door)).await
}

/// Dismiss one reminder.
#[tauri::command(rename_all = "snake_case")]
pub async fn reminder_dismiss(
    session: State<'_, SessionHandle>,
    account: String,
    event: String,
) -> Result<(), GuiError> {
    with_door(&session, move |_, door| {
        reminder_dismiss_on(door, &account, &event)
    })
    .await
}

/// Snooze one reminder for `minutes`.
#[tauri::command(rename_all = "snake_case")]
pub async fn reminder_snooze(
    session: State<'_, SessionHandle>,
    account: String,
    event: String,
    minutes: u32,
) -> Result<(), GuiError> {
    with_door(&session, move |_, door| {
        reminder_snooze_on(door, &account, &event, minutes)
    })
    .await
}

/// Raise the notification of a made-up reminder, to check that the system
/// shows this app's notifications; nothing reaches the daemon.
#[tauri::command(rename_all = "snake_case")]
pub fn reminder_notify_test(app: AppHandle) {
    notify(&app, &test_reminder());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reminder() -> CalendarReminder {
        CalendarReminder {
            account: "evoqs".into(),
            event: "AAMkADev2".into(),
            summary: "Standup".into(),
            start: "2026-11-04T09:00:00Z".into(),
            all_day: false,
            location: Some("Room 2.14".into()),
            online_meeting_url: None,
            minutes_before: 15,
            presenter: Some("gui".into()),
            presenter_connection: Some(4),
        }
    }

    /// Only the reminder naming this app's connection is notified: one a TUI
    /// or another connection presents is not, nor any before a bootstrap.
    #[test]
    fn only_this_apps_connection_presents() {
        assert!(presents(&reminder(), Some(4)));
        assert!(!presents(&reminder(), Some(5)));
        assert!(!presents(&reminder(), None));
        let unpresented = CalendarReminder {
            presenter: None,
            presenter_connection: None,
            ..reminder()
        };
        assert!(!presents(&unpresented, None));
    }

    /// A click on a reminder's notification opens its event; one on the
    /// palette's test notification, which names no account, opens nothing.
    #[test]
    fn the_test_notification_opens_no_event() {
        let target = ReminderTarget::of(&reminder());
        assert_eq!(to_open(target.clone()), Some(target));
        assert_eq!(to_open(ReminderTarget::of(&test_reminder())), None);
    }

    /// The extra payload a click hands back names the event to open.
    #[test]
    fn a_click_names_the_reminders_event() {
        let target = ReminderTarget::of(&reminder());
        let performed: tauri_plugin_notification::ActionPerformed =
            serde_json::from_value(serde_json::json!({
                "actionId": "tap",
                "inputValue": null,
                "notification": {"id": 1, "extra": {"reminder": target}},
            }))
            .unwrap();
        assert_eq!(clicked(&performed), Some(ReminderTarget::of(&reminder())));
        let other: tauri_plugin_notification::ActionPerformed =
            serde_json::from_value(serde_json::json!({
                "actionId": "dismiss",
                "notification": {"id": 1, "extra": {"reminder": ReminderTarget::of(&reminder())}},
            }))
            .unwrap();
        assert_eq!(clicked(&other), None);
    }

    /// The test reminder is due in five minutes, presented by this app, and
    /// its start reads as an RFC 3339 instant.
    #[test]
    fn the_test_reminder_is_presented_here() {
        let test = test_reminder();
        assert_eq!(test.presenter.as_deref(), Some(PRESENTER_GUI));
        let (_, body) = mp_client::format::reminder_notification(&test);
        assert!(body.starts_with("in "), "{body}");
    }
}
