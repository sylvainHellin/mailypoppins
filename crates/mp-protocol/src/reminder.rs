//! The calendar reminders (PERSO-255, calendar phase 2).
//!
//! The daemon computes each event's reminder from its calendar cache and
//! fires it at its due minute as a `calendar.reminder` lifecycle event
//! ([`CalendarReminder`]); `calendar.reminders` answers the fired reminders
//! not yet dismissed, for a client that connects later.
//! `calendar.dismiss_reminder` and `calendar.snooze_reminder` are commands
//! that publish `calendar.reminders_changed` ([`CalendarRemindersChanged`]),
//! so every client clears or moves the reminder together.

use serde::{Deserialize, Serialize};

/// `presenter` when a desktop app is connected: the most recently
/// bootstrapped one raises the system notification.
pub const PRESENTER_GUI: &str = "gui";

/// `presenter` when a TUI is connected and no desktop app is: the most
/// recently bootstrapped TUI raises the system notification.
pub const PRESENTER_TUI: &str = "tui";

/// One reminder that fell due, as `calendar.reminder` carries it and
/// `calendar.reminders` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarReminder {
    /// The configured account name.
    pub account: String,
    /// The server id of the event, the one `event.get` takes.
    pub event: String,
    pub summary: String,
    /// When the event starts, an RFC 3339 instant in UTC; an all-day event's
    /// first midnight in the daemon's zone.
    pub start: String,
    /// Whether the event lasts whole days.
    #[serde(default)]
    pub all_day: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub location: Option<String>,
    /// The join link of an online meeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub online_meeting_url: Option<String>,
    /// The event's reminder setting, in minutes before the start.
    pub minutes_before: u32,
    /// The kind of the connection that raises the system notification:
    /// [`PRESENTER_GUI`] when a desktop app is connected, else
    /// [`PRESENTER_TUI`] when a TUI is, else none; every client shows the
    /// reminder in its own window.
    pub presenter: Option<String>,
    /// That connection itself, as its `state.bootstrap` named it
    /// (`connection`): the one client whose connection this is notifies, so
    /// two clients of one kind raise one notification.
    #[serde(default)]
    pub presenter_connection: Option<u64>,
}

/// The `params` of `calendar.reminders`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarRemindersParams {
    /// The accounts to list, every configured one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub accounts: Option<Vec<String>>,
}

/// The `result` of `calendar.reminders`: the fired reminders not dismissed
/// and not snoozed, the earliest start first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarRemindersResult {
    pub reminders: Vec<CalendarReminder>,
}

/// The `result` of `calendar.dismiss_reminder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReminderDismissed {
    pub account: String,
    pub event: String,
}

/// The `result` of `calendar.snooze_reminder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReminderSnoozed {
    pub account: String,
    pub event: String,
    /// When the reminder fires again, an RFC 3339 instant in UTC.
    pub until: String,
}

/// The payload of a `calendar.reminders_changed` event: a reminder of the
/// account was dismissed, snoozed or cleared (its event cancelled, declined,
/// ended or its reminder turned off), and a client that shows reminders reads
/// `calendar.reminders` again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarRemindersChanged {
    pub account: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed reminder fixtures decode into their wire types whole.
    #[test]
    fn the_reminder_fixtures_decode() {
        let frame = |raw: &str| -> serde_json::Value { serde_json::from_str(raw).expect("JSON") };
        let listed: CalendarRemindersResult = serde_json::from_value(
            frame(include_str!("../fixtures/calendar.reminders.response.json"))["result"].clone(),
        )
        .expect("calendar.reminders");
        assert_eq!(
            listed.reminders[0].presenter.as_deref(),
            Some(PRESENTER_GUI)
        );
        let fired: CalendarReminder = serde_json::from_value(
            frame(include_str!(
                "../fixtures/notification.calendar_reminder.json"
            ))["params"]["payload"]
                .clone(),
        )
        .expect("calendar.reminder");
        assert_eq!(fired.minutes_before, 15);
        let snoozed: ReminderSnoozed = serde_json::from_value(
            frame(include_str!(
                "../fixtures/calendar.snooze_reminder.response.json"
            ))["result"]
                .clone(),
        )
        .expect("calendar.snooze_reminder");
        assert_eq!(snoozed.event, fired.event);
        let _: ReminderDismissed = serde_json::from_value(
            frame(include_str!(
                "../fixtures/calendar.dismiss_reminder.response.json"
            ))["result"]
                .clone(),
        )
        .expect("calendar.dismiss_reminder");
        let _: CalendarRemindersChanged = serde_json::from_value(
            frame(include_str!(
                "../fixtures/notification.calendar_reminders_changed.json"
            ))["params"]["payload"]
                .clone(),
        )
        .expect("calendar.reminders_changed");
    }
}
