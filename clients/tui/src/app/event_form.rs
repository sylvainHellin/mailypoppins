//! The event form (PERSO-255, `n` in the Calendar view): an overlay built
//! like the compose wizard and the respond overlay, one row per field, the
//! description in `$EDITOR`, which `event.create` takes as typed.
//!
//! The form checks its fields as they are typed ([`EventForm::problem`]) and
//! the daemon checks them again, so a refusal it words lands on the form's
//! error line with the content kept. A create that failed after it started,
//! or whose daemon restarted under it, reopens the form with the transaction
//! id it was sent with, so `Enter` retries it and the server makes one event.

use chrono::{NaiveDate, NaiveTime};
use mp_protocol::calendar::{
    CalendarInfo, EventAttendeeInput, EventCreateParams, EventInputParams,
};

/// One calendar a new event can go to: a server calendar the user may write
/// to on an account whose backend serves `write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCalendarChoice {
    pub account: String,
    pub id: String,
    pub name: String,
    /// Whether the backend serves `online-meeting`, which the Teams row needs.
    pub teams: bool,
}

impl EventCalendarChoice {
    /// The calendars `calendar.list` named that take a new event, the default
    /// calendar of each account first.
    pub fn writable(calendars: &[CalendarInfo]) -> Vec<EventCalendarChoice> {
        let mut choices: Vec<(bool, EventCalendarChoice)> = calendars
            .iter()
            .filter(|c| c.can_edit && c.features.iter().any(|f| f == "write"))
            .map(|c| {
                (
                    c.is_default,
                    EventCalendarChoice {
                        account: c.account.clone(),
                        id: c.id.clone(),
                        name: c.name.clone(),
                        teams: c.features.iter().any(|f| f == "online-meeting"),
                    },
                )
            })
            .collect();
        // Stable: accounts keep their order, each one's default first.
        choices.sort_by_key(|(default, choice)| {
            (
                calendars
                    .iter()
                    .position(|c| c.account == choice.account)
                    .unwrap_or(usize::MAX),
                !*default,
            )
        });
        choices.into_iter().map(|(_, choice)| choice).collect()
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.account, self.name)
    }
}

/// The rows of the event form, in the order `Tab` walks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventFormRow {
    Calendar,
    Title,
    AllDay,
    Date,
    Start,
    End,
    Location,
    To,
    Optional,
    Rooms,
    Teams,
    Reminder,
    Description,
}

impl EventFormRow {
    /// Whether the row takes typed text.
    pub fn is_text(self) -> bool {
        !matches!(
            self,
            EventFormRow::Calendar
                | EventFormRow::AllDay
                | EventFormRow::Teams
                | EventFormRow::Description
        )
    }
}

/// The event form's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventForm {
    /// Where the event can go; never empty.
    pub calendars: Vec<EventCalendarChoice>,
    /// Index into `calendars`.
    pub calendar: usize,
    pub row: EventFormRow,
    pub title: String,
    pub all_day: bool,
    /// `YYYY-MM-DD`, the first day.
    pub date: String,
    /// `HH:MM` on a timed event.
    pub start: String,
    /// `HH:MM` on the same day, or `YYYY-MM-DDTHH:MM`; the last day
    /// (`YYYY-MM-DD`, empty for one day) on an all-day event.
    pub end: String,
    pub location: String,
    /// Required attendees, comma separated.
    pub to: String,
    pub optional: String,
    pub rooms: String,
    pub teams: bool,
    /// Minutes before the start; empty for the mailbox default, `none` for
    /// no reminder.
    pub reminder: String,
    /// The description, edited in `$EDITOR`.
    pub body: String,
    /// The IANA zone the times are read in: the Calendar view's.
    pub zone: String,
    /// The id the create was last sent with, which a retry sends again.
    pub transaction_id: Option<String>,
    /// Why the last attempt failed, shown under the rows.
    pub error: Option<String>,
}

impl EventForm {
    /// A fresh form on `day` from 09:00 to 10:00, in the calendar of
    /// `account` when it has one, else in the first writable calendar.
    pub fn new(
        calendars: Vec<EventCalendarChoice>,
        account: &str,
        day: NaiveDate,
        zone: &str,
    ) -> Self {
        let calendar = calendars
            .iter()
            .position(|c| c.account == account)
            .unwrap_or(0);
        EventForm {
            calendars,
            calendar,
            row: EventFormRow::Title,
            title: String::new(),
            all_day: false,
            date: day.to_string(),
            start: "09:00".into(),
            end: "10:00".into(),
            location: String::new(),
            to: String::new(),
            optional: String::new(),
            rooms: String::new(),
            teams: false,
            reminder: String::new(),
            body: String::new(),
            zone: zone.to_string(),
            transaction_id: None,
            error: None,
        }
    }

    /// The calendar the event goes to.
    pub fn chosen(&self) -> &EventCalendarChoice {
        &self.calendars[self.calendar.min(self.calendars.len() - 1)]
    }

    /// The rows `Tab` walks: no start time on an all-day event, no Teams row
    /// where the backend has no online meetings.
    pub fn rows(&self) -> Vec<EventFormRow> {
        use EventFormRow::*;
        [
            Calendar,
            Title,
            AllDay,
            Date,
            Start,
            End,
            Location,
            To,
            Optional,
            Rooms,
            Teams,
            Reminder,
            Description,
        ]
        .into_iter()
        .filter(|row| match row {
            Start => !self.all_day,
            Teams => self.chosen().teams,
            _ => true,
        })
        .collect()
    }

    /// Move the cursor `by` rows, clamped.
    pub fn step(&mut self, by: isize) {
        let rows = self.rows();
        let at = rows.iter().position(|r| *r == self.row).unwrap_or(0) as isize;
        let to = (at + by).clamp(0, rows.len() as isize - 1) as usize;
        self.row = rows[to];
    }

    /// The text the cursor row types into.
    pub fn text_mut(&mut self) -> Option<&mut String> {
        Some(match self.row {
            EventFormRow::Title => &mut self.title,
            EventFormRow::Date => &mut self.date,
            EventFormRow::Start => &mut self.start,
            EventFormRow::End => &mut self.end,
            EventFormRow::Location => &mut self.location,
            EventFormRow::To => &mut self.to,
            EventFormRow::Optional => &mut self.optional,
            EventFormRow::Rooms => &mut self.rooms,
            EventFormRow::Reminder => &mut self.reminder,
            _ => return None,
        })
    }

    /// Flip the all-day switch: the end becomes the last day (empty for one
    /// day) or a time again, and the cursor leaves the start row it hides.
    pub fn toggle_all_day(&mut self) {
        self.all_day = !self.all_day;
        self.end = if self.all_day {
            String::new()
        } else {
            "10:00".into()
        };
        if self.all_day && self.row == EventFormRow::Start {
            self.row = EventFormRow::Date;
        }
    }

    /// Move to the next or the previous calendar; a calendar without online
    /// meetings turns the Teams switch off.
    pub fn cycle_calendar(&mut self, forward: bool) {
        let len = self.calendars.len();
        self.calendar = if forward {
            (self.calendar + 1) % len
        } else {
            (self.calendar + len - 1) % len
        };
        if !self.chosen().teams {
            self.teams = false;
        }
    }

    /// What is wrong with the fields as typed, the first thing only; `None`
    /// when the form can be sent. The daemon checks everything again.
    pub fn problem(&self) -> Option<String> {
        if self.title.trim().is_empty() {
            return Some("The event needs a title".into());
        }
        let Ok(date) = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d") else {
            return Some("The date is YYYY-MM-DD".into());
        };
        if self.all_day {
            let end = self.end.trim();
            if !end.is_empty() {
                match NaiveDate::parse_from_str(end, "%Y-%m-%d") {
                    Ok(last) if last >= date => {}
                    Ok(_) => return Some("The last day comes before the first".into()),
                    Err(_) => return Some("The last day is YYYY-MM-DD, or empty for one".into()),
                }
            }
        } else {
            let Some(start) = time(&self.start) else {
                return Some("The start is HH:MM".into());
            };
            match self.end_at(date) {
                Some(end) if end > date.and_time(start) => {}
                Some(_) => return Some("The end comes before the start".into()),
                None => return Some("The end is HH:MM, or YYYY-MM-DDTHH:MM".into()),
            }
        }
        for (what, list) in [
            ("attendee", &self.to),
            ("optional attendee", &self.optional),
            ("room", &self.rooms),
        ] {
            if let Some(bad) = addresses(list).find(|a| !a.contains('@')) {
                return Some(format!("The {what} {bad:?} is not an email address"));
            }
        }
        if self.reminder_minutes().is_err() {
            return Some("The reminder is minutes, none, or empty for the default".into());
        }
        None
    }

    /// The end of a timed event: `HH:MM` on `date`, or a full date-time.
    fn end_at(&self, date: NaiveDate) -> Option<chrono::NaiveDateTime> {
        let end = self.end.trim();
        if let Some(at) = time(end) {
            return Some(date.and_time(at));
        }
        chrono::NaiveDateTime::parse_from_str(end, "%Y-%m-%dT%H:%M").ok()
    }

    /// The reminder as `event.create` takes it: absent, `null`, or minutes.
    fn reminder_minutes(&self) -> Result<Option<Option<u32>>, ()> {
        match self.reminder.trim() {
            "" => Ok(None),
            "none" | "off" => Ok(Some(None)),
            minutes => minutes.parse().map(|m| Some(Some(m))).map_err(|_| ()),
        }
    }

    /// `event.create`'s params, or the form's problem.
    pub fn params(&self) -> Result<EventCreateParams, String> {
        if let Some(problem) = self.problem() {
            return Err(problem);
        }
        let date = self.date.trim();
        let (start, end) = if self.all_day {
            let last = self.end.trim();
            (
                date.to_string(),
                (!last.is_empty()).then(|| last.to_string()),
            )
        } else {
            let end = self.end.trim();
            let end = if time(end).is_some() {
                format!("{date}T{end}")
            } else {
                end.to_string()
            };
            (format!("{date}T{}", self.start.trim()), Some(end))
        };
        let attendees = [
            (&self.to, "required"),
            (&self.optional, "optional"),
            (&self.rooms, "resource"),
        ]
        .into_iter()
        .flat_map(|(list, role)| {
            addresses(list).map(move |email| EventAttendeeInput {
                email: email.to_string(),
                name: None,
                role: Some(role.to_string()),
            })
        })
        .collect();
        let text = |value: &str| Some(value.trim().to_string()).filter(|v| !v.is_empty());
        let chosen = self.chosen();
        Ok(EventCreateParams {
            account: chosen.account.clone(),
            calendar: Some(chosen.id.clone()),
            transaction_id: self.transaction_id.clone(),
            event: EventInputParams {
                summary: self.title.trim().to_string(),
                start,
                end,
                zone: Some(self.zone.clone()),
                all_day: Some(self.all_day),
                location: text(&self.location),
                body: Some(self.body.clone()).filter(|b| !b.trim().is_empty()),
                attendees,
                online_meeting: (self.teams && chosen.teams).then_some(true),
                reminder: self.reminder_minutes().unwrap_or(None),
                ..Default::default()
            },
        })
    }
}

/// `HH:MM`.
fn time(raw: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(raw.trim(), "%H:%M").ok()
}

/// The addresses of a comma or semicolon separated list.
fn addresses(list: &str) -> impl Iterator<Item = &str> {
    list.split([',', ';'])
        .map(str::trim)
        .filter(|a| !a.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(account: &str, id: &str, default: bool, features: &[&str]) -> CalendarInfo {
        CalendarInfo {
            account: account.into(),
            id: id.into(),
            name: format!("Cal {id}"),
            color: None,
            is_default: default,
            can_edit: true,
            visible: true,
            backend: "graph".into(),
            features: features.iter().map(|f| f.to_string()).collect(),
        }
    }

    fn form() -> EventForm {
        let calendars = EventCalendarChoice::writable(&[
            info("evoqs", "second", false, &["write", "online-meeting"]),
            info("evoqs", "main", true, &["write", "online-meeting"]),
            info("perso", "imip", false, &[]),
        ]);
        let day = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        EventForm::new(calendars, "evoqs", day, "Europe/Berlin")
    }

    /// Only a calendar whose backend writes takes an event, each account's
    /// default first, and the form opens on the active account's default.
    #[test]
    fn the_form_offers_the_writable_calendars_default_first() {
        let form = form();
        let ids: Vec<&str> = form.calendars.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["main", "second"]);
        assert_eq!(form.chosen().label(), "evoqs/Cal main");
        assert!(form.rows().contains(&EventFormRow::Teams));
    }

    /// A timed form becomes `event.create`'s params in the Calendar view's
    /// zone, with each attendee list in its role; the reminder takes
    /// minutes, `none`, or nothing for the mailbox default.
    #[test]
    fn a_timed_form_becomes_its_params() {
        let mut form = form();
        form.title = " Planning ".into();
        form.start = "14:00".into();
        form.end = "15:30".into();
        form.to = "ada@example.com, bob@example.com".into();
        form.rooms = "room-1@example.com".into();
        form.teams = true;
        form.reminder = "none".into();
        form.transaction_id = Some("mp-1".into());
        let params = form.params().unwrap();
        assert_eq!(params.account, "evoqs");
        assert_eq!(params.calendar.as_deref(), Some("main"));
        assert_eq!(params.transaction_id.as_deref(), Some("mp-1"));
        let event = params.event;
        assert_eq!(event.summary, "Planning");
        assert_eq!(event.start, "2026-11-18T14:00");
        assert_eq!(event.end.as_deref(), Some("2026-11-18T15:30"));
        assert_eq!(event.zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(event.online_meeting, Some(true));
        assert_eq!(event.reminder, Some(None));
        let roles: Vec<(&str, &str)> = event
            .attendees
            .iter()
            .map(|a| (a.email.as_str(), a.role.as_deref().unwrap()))
            .collect();
        assert_eq!(
            roles,
            [
                ("ada@example.com", "required"),
                ("bob@example.com", "required"),
                ("room-1@example.com", "resource"),
            ]
        );
    }

    /// An all-day form hides the start row and sends bare dates, its end the
    /// last day or nothing for one day.
    #[test]
    fn an_all_day_form_sends_dates() {
        let mut form = form();
        form.title = "Away".into();
        form.row = EventFormRow::Start;
        form.toggle_all_day();
        assert_eq!(form.row, EventFormRow::Date);
        assert!(!form.rows().contains(&EventFormRow::Start));
        let params = form.params().unwrap();
        assert_eq!(params.event.start, "2026-11-18");
        assert_eq!(params.event.end, None);
        assert_eq!(params.event.all_day, Some(true));
        form.end = "2026-11-20".into();
        assert_eq!(
            form.params().unwrap().event.end.as_deref(),
            Some("2026-11-20")
        );
    }

    /// The fields are checked as typed, the first problem named.
    #[test]
    fn the_form_names_its_first_problem() {
        let mut form = form();
        assert_eq!(form.problem().as_deref(), Some("The event needs a title"));
        form.title = "Planning".into();
        assert_eq!(form.problem(), None);
        form.end = "08:00".into();
        assert_eq!(
            form.problem().as_deref(),
            Some("The end comes before the start")
        );
        form.end = "2026-11-19T08:00".into();
        assert_eq!(form.problem(), None);
        form.to = "ada".into();
        assert!(form.problem().unwrap().contains("not an email address"));
        form.to.clear();
        form.reminder = "soon".into();
        assert!(form.problem().unwrap().starts_with("The reminder"));
        assert!(form.params().is_err());
    }

    /// Moving to a calendar without online meetings drops the Teams switch
    /// and its row.
    #[test]
    fn a_calendar_without_teams_turns_it_off() {
        let mut form = form();
        form.calendars[1].teams = false;
        form.teams = true;
        form.cycle_calendar(true);
        assert_eq!(form.chosen().id, "second");
        assert!(!form.teams);
        assert!(!form.rows().contains(&EventFormRow::Teams));
        form.cycle_calendar(false);
        assert_eq!(form.chosen().id, "main");
    }
}
