//! The calendar reminders in the TUI (PERSO-255).
//!
//! The daemon fires a reminder at its due minute as `calendar.reminder`; the
//! TUI puts it on the status line, keeps it in [`App::reminders`] (which the
//! hint bar counts), and raises a system notification through
//! [`mp_core::notify`] when `notifications = true` and the daemon names the
//! TUI as the presenter, that is when no desktop app is connected. `R` from
//! any view opens the reminders overlay: `s` snoozes the one under the cursor
//! for [`SNOOZE_MINUTES`], `d` dismisses it, `Enter` opens its event in the
//! Calendar view. A dismissal or a snooze in any client publishes
//! `calendar.reminders_changed`, which reads the list again here.

use chrono::{DateTime, Utc};
use mp_protocol::reminder::{CalendarReminder, PRESENTER_TUI};

use super::{App, Overlay, RemindersOverlay, StatusLevel, View};

/// How long `s` snoozes a reminder, the desktop app's Snooze too.
pub const SNOOZE_MINUTES: u32 = 5;

impl App {
    /// Read the waiting reminders from the daemon: at a bootstrap, and at
    /// every `calendar.reminders_changed`. An `App` with no session keeps
    /// what it has; a refusal is logged and empties nothing.
    pub fn reload_reminders(&mut self) {
        let Some(queries) = self.queries() else {
            return;
        };
        match super::super::queries::calendar_reminders(queries) {
            Ok(reminders) => self.set_reminders(reminders),
            Err(e) => log::warn!("[queries] calendar.reminders: {e:#}"),
        }
    }

    /// Replace the list, keeping an open overlay's cursor in range and
    /// closing an overlay left with nothing to show.
    fn set_reminders(&mut self, reminders: Vec<CalendarReminder>) {
        self.reminders = reminders;
        if let Overlay::Reminders(overlay) = &mut self.overlay {
            if self.reminders.is_empty() {
                self.overlay = Overlay::None;
            } else {
                overlay.selected = overlay.selected.min(self.reminders.len() - 1);
            }
        }
    }

    /// `calendar.reminder`: a reminder fell due. It joins the list (a snooze
    /// that ended replaces its earlier entry), goes on the status line, and
    /// raises a notification when this TUI is the presenter.
    pub fn on_reminder_fired(&mut self, payload: &serde_json::Value) {
        let reminder: CalendarReminder = match serde_json::from_value(payload.clone()) {
            Ok(reminder) => reminder,
            Err(e) => {
                log::warn!("[events] a calendar.reminder did not decode: {e}");
                return;
            }
        };
        let now = Utc::now();
        let zone = self.calendar_view.zone;
        let line = mp_client::format::reminder_line(&reminder, now, &zone);
        if self.global_config.notifications && reminder.presenter.as_deref() == Some(PRESENTER_TUI)
        {
            let mut when = mp_client::format::reminder_when(&reminder, now, &zone);
            if let Some(location) = reminder
                .location
                .as_deref()
                .filter(|l| !l.trim().is_empty())
            {
                when.push_str(&format!(", {location}"));
            }
            mp_core::notify::notify_reminder(&reminder.summary, &when);
        }
        self.reminders
            .retain(|r| !(r.account == reminder.account && r.event == reminder.event));
        self.reminders.push(reminder);
        self.reminders.sort_by(|a, b| a.start.cmp(&b.start));
        self.set_status_level(
            format!("\u{25f7} Reminder: {line} (R to snooze or dismiss)"),
            StatusLevel::Warning,
        );
    }

    /// `R`: the reminders overlay over the list the daemon holds now.
    pub fn open_reminders_overlay(&mut self) {
        self.reload_reminders();
        if self.reminders.is_empty() {
            self.set_status("No reminders waiting".into());
            return;
        }
        self.overlay = Overlay::Reminders(RemindersOverlay { selected: 0 });
    }

    /// The reminder under the overlay's cursor.
    fn selected_reminder(&self) -> Option<CalendarReminder> {
        let Overlay::Reminders(overlay) = &self.overlay else {
            return None;
        };
        self.reminders.get(overlay.selected).cloned()
    }

    /// `s` in the overlay: snooze the reminder under the cursor.
    pub fn snooze_selected_reminder(&mut self) {
        let Some(reminder) = self.selected_reminder() else {
            return;
        };
        let Some(queries) = self.queries() else {
            self.set_status_level("No daemon session".into(), StatusLevel::Error);
            return;
        };
        match super::super::queries::calendar_snooze_reminder(
            queries,
            &reminder.account,
            &reminder.event,
            SNOOZE_MINUTES,
        ) {
            Ok(()) => {
                self.reload_reminders();
                self.set_status(format!(
                    "Snoozed {} for {SNOOZE_MINUTES} min",
                    reminder.summary
                ));
            }
            Err(e) => self.set_status_level(
                format!("Cannot snooze {}: {e:#}", reminder.summary),
                StatusLevel::Error,
            ),
        }
    }

    /// `d` in the overlay: dismiss the reminder under the cursor.
    pub fn dismiss_selected_reminder(&mut self) {
        let Some(reminder) = self.selected_reminder() else {
            return;
        };
        let Some(queries) = self.queries() else {
            self.set_status_level("No daemon session".into(), StatusLevel::Error);
            return;
        };
        match super::super::queries::calendar_dismiss_reminder(
            queries,
            &reminder.account,
            &reminder.event,
        ) {
            Ok(()) => {
                self.reload_reminders();
                self.set_status(format!("Dismissed {}", reminder.summary));
            }
            Err(e) => self.set_status_level(
                format!("Cannot dismiss {}: {e:#}", reminder.summary),
                StatusLevel::Error,
            ),
        }
    }

    /// `Enter` in the overlay: the event in the Calendar view, its card
    /// open, on the day it starts.
    pub fn open_selected_reminder_event(&mut self) {
        let Some(reminder) = self.selected_reminder() else {
            return;
        };
        self.close_overlay();
        self.switch_view(View::Calendar);
        let zone = self.calendar_view.zone;
        if let Ok(start) = DateTime::parse_from_rfc3339(&reminder.start) {
            self.calendar_go_to(start.with_timezone(&zone).date_naive());
        }
        let found = self.calendar_view.entries.iter().position(|entry| {
            self.calendar_view
                .events
                .get(entry.event)
                .is_some_and(|event| {
                    event.account == reminder.account && event.id == reminder.event
                })
        });
        match found {
            Some(index) => {
                self.calendar_view.list_index = index;
                self.open_event_card();
            }
            None => self.set_status(format!(
                "{} is not in the Calendar view's days",
                reminder.summary
            )),
        }
    }
}
