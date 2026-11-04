//! The event form (PERSO-255, `n` in the Calendar view): an overlay built
//! like the compose wizard and the respond overlay, one row per field, the
//! description in `$EDITOR`, which `event.create` takes as typed.
//!
//! The form checks its fields as they are typed ([`EventForm::problem`]) and
//! the daemon checks them again, so a refusal it words lands on the form's
//! error line with the content kept. A create that failed after it started,
//! or whose daemon restarted under it, reopens the form with the transaction
//! id it was sent with, so `Enter` retries it and the server makes one event.
//!
//! `E` opens the same form on an event the user may change
//! ([`EventForm::edit`]), filled with its values; `Enter` sends
//! `event.update` with the fields that differ from what the form opened
//! with, so a change made in Outlook to another field survives. The
//! calendar stays the event's, and a Teams meeting stays one.

use chrono::{Days, NaiveDate, NaiveTime};
use mp_protocol::calendar::{
    CalendarEvent, CalendarEventTime, CalendarInfo, EventAttendeeInput, EventCreateParams,
    EventInputParams, EventPatchParams, EventUpdateParams,
};

use super::Tz;

/// What an edit form changes: the event, and the form as it opened, which
/// the change is read against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEdit {
    pub account: String,
    pub event: String,
    /// The event is the user's own: an attendee changes only the reminder
    /// and the show-as.
    pub is_organizer: bool,
    /// The event is a Teams meeting, which stays one.
    pub online: bool,
    pub original: Box<EventForm>,
}

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
    ShowAs,
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
                | EventFormRow::ShowAs
                | EventFormRow::Description
        )
    }
}

/// The show-as words `event.create` and `event.update` take, with the label
/// the form shows, in the order `Left` and `Right` walk them. `busy` is
/// the server's default for a new event.
pub const SHOW_AS: [(&str, &str); 5] = [
    ("free", "Free"),
    ("tentative", "Tentative"),
    ("busy", "Busy"),
    ("oof", "Out of office"),
    ("working-elsewhere", "Working elsewhere"),
];

/// The show-as a new event gets when the form leaves it alone.
pub const DEFAULT_SHOW_AS: &str = "busy";

/// The date, start and end rows of a timed span from `start` to `end`,
/// RFC 3339 instants, read in `zone`: the end as a clock time on the same
/// day, else with its date.
fn timed_rows(start: &str, end: &str, zone: Tz) -> (String, String, String) {
    let local = |raw: &str| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|at| at.with_timezone(&zone).naive_local())
    };
    let (start, end) = (local(start), local(end));
    let date = start.map(|s| s.date().to_string()).unwrap_or_default();
    let start_clock = start.map(|s| s.format("%H:%M").to_string());
    let end = match (start, end) {
        (Some(s), Some(e)) if s.date() == e.date() => e.format("%H:%M").to_string(),
        (_, Some(e)) => e.format("%Y-%m-%dT%H:%M").to_string(),
        _ => String::new(),
    };
    (date, start_clock.unwrap_or_default(), end)
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
    /// How the time shows to others: a word of [`SHOW_AS`], or the word the
    /// server gave an event being edited (`unknown`) until the user picks
    /// one.
    pub show_as: String,
    /// The description, edited in `$EDITOR`.
    pub body: String,
    /// The IANA zone the times are read in: the Calendar view's.
    pub zone: String,
    /// The id the create was last sent with, which a retry sends again.
    pub transaction_id: Option<String>,
    /// Why the last attempt failed, shown under the rows.
    pub error: Option<String>,
    /// The event the form changes, for an edit form; `None` for a new event.
    pub editing: Option<EventEdit>,
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
            show_as: DEFAULT_SHOW_AS.into(),
            body: String::new(),
            zone: zone.to_string(),
            transaction_id: None,
            error: None,
            editing: None,
        }
    }

    /// A form on `event`, which `calendar` holds, filled with its values in
    /// `zone`, the Calendar view's: `Enter` sends what changed.
    pub fn edit(event: &CalendarEvent, calendar: EventCalendarChoice, zone: Tz) -> Self {
        let day = |raw: &str| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok();
        let (all_day, date, start, end) = match &event.time {
            CalendarEventTime::AllDay { start, end } => {
                let first = day(start);
                let last = day(end).and_then(|end| end.checked_sub_days(Days::new(1)));
                let end = match (first, last) {
                    (Some(first), Some(last)) if last > first => last.to_string(),
                    _ => String::new(),
                };
                (true, start.clone(), String::new(), end)
            }
            CalendarEventTime::Timed { start, end, .. } => {
                let (date, start, end) = timed_rows(start, end, zone);
                (false, date, start, end)
            }
        };
        let people = |role: &str| {
            event
                .attendees
                .iter()
                .filter(|a| a.role == role)
                .map(|a| a.email.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut form = EventForm {
            calendars: vec![calendar],
            calendar: 0,
            row: EventFormRow::Title,
            title: event.summary.clone(),
            all_day,
            date,
            start,
            end,
            location: event.location.clone().unwrap_or_default(),
            to: people("required"),
            optional: people("optional"),
            rooms: people("resource"),
            teams: event.is_online_meeting,
            reminder: match event.reminder {
                Some(minutes) => minutes.to_string(),
                None => "none".into(),
            },
            show_as: event.show_as.clone(),
            body: event.body.clone().unwrap_or_default(),
            zone: zone.name().to_string(),
            transaction_id: None,
            error: None,
            editing: None,
        };
        if !event.is_organizer {
            form.row = EventFormRow::Reminder;
        }
        form.editing = Some(EventEdit {
            account: event.account.clone(),
            event: event.id.clone(),
            is_organizer: event.is_organizer,
            online: event.is_online_meeting,
            original: Box::new(form.clone()),
        });
        form
    }

    /// Move the form's time to the span from `start` to `end`, RFC 3339
    /// instants, read in `zone`: what `p` on the event card does with an
    /// attendee's proposed time (PERSO-255), which `Enter` then sends as a
    /// change of the event's time.
    pub fn move_to(&mut self, start: &str, end: &str, zone: Tz) {
        let (date, start, end) = timed_rows(start, end, zone);
        self.all_day = false;
        self.date = date;
        self.start = start;
        self.end = end;
        self.error = None;
    }

    /// Flip the Teams switch, which a Teams meeting being edited keeps on.
    pub fn toggle_teams(&mut self) {
        if self.editing.as_ref().is_some_and(|edit| edit.online) {
            return;
        }
        self.teams = !self.teams;
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
            ShowAs,
            Description,
        ]
        .into_iter()
        .filter(|row| match row {
            Start => !self.all_day,
            Teams => self.chosen().teams || self.editing.as_ref().is_some_and(|e| e.online),
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

    /// Move to the next or the previous show-as; a word the server gave that
    /// [`SHOW_AS`] does not name moves to its first or last.
    pub fn cycle_show_as(&mut self, forward: bool) {
        let len = SHOW_AS.len();
        let at = SHOW_AS.iter().position(|(word, _)| *word == self.show_as);
        let to = match (at, forward) {
            (Some(at), true) => (at + 1) % len,
            (Some(at), false) => (at + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        };
        self.show_as = SHOW_AS[to].0.to_string();
    }

    /// The show-as as the form shows it.
    pub fn show_as_label(&self) -> &str {
        SHOW_AS
            .iter()
            .find(|(word, _)| *word == self.show_as)
            .map_or(self.show_as.as_str(), |(_, label)| label)
    }

    /// What is wrong with the fields as typed, the first thing only; `None`
    /// when the form can be sent. The daemon checks everything again. An
    /// edit form that changes nothing, or an attendee's that changes more
    /// than the reminder and the show-as, says so.
    pub fn problem(&self) -> Option<String> {
        if let Some(problem) = self.fields_problem() {
            return Some(problem);
        }
        let edit = self.editing.as_ref()?;
        let fields = self.changed_fields();
        if fields == EventPatchParams::default() {
            return Some("Nothing changed yet".into());
        }
        let own = EventPatchParams {
            reminder: fields.reminder,
            show_as: fields.show_as.clone(),
            ..Default::default()
        };
        if !edit.is_organizer && fields != own {
            return Some(ORGANIZER_ONLY.into());
        }
        None
    }

    /// [`Self::problem`] of the fields alone. A title is needed on a new
    /// event, and on an edit only when the user clears one the event had:
    /// an event the server holds with none keeps saving its other fields.
    fn fields_problem(&self) -> Option<String> {
        let had_title = self
            .editing
            .as_ref()
            .is_none_or(|edit| !edit.original.title.trim().is_empty());
        if self.title.trim().is_empty() && had_title {
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
    /// An edit form has no mailbox default to fall back on, so an empty
    /// reminder is none there.
    fn reminder_minutes(&self) -> Result<Option<Option<u32>>, ()> {
        match self.reminder.trim() {
            "" if self.editing.is_some() => Ok(Some(None)),
            "" => Ok(None),
            "none" | "off" => Ok(Some(None)),
            minutes => minutes.parse().map(|m| Some(Some(m))).map_err(|_| ()),
        }
    }

    /// The start and the end as the daemon reads them: a date-time and an
    /// end on a timed event, the first and the last day on an all-day one.
    fn span(&self) -> (String, Option<String>) {
        let date = self.date.trim();
        if self.all_day {
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
        }
    }

    /// The attendees of the three lists, each in its role.
    fn attendees(&self) -> Vec<EventAttendeeInput> {
        [
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
        .collect()
    }

    /// The fields an edit form changed against the form it opened with;
    /// empty on a new-event form.
    pub fn changed_fields(&self) -> EventPatchParams {
        let Some(edit) = &self.editing else {
            return EventPatchParams::default();
        };
        let was = &edit.original;
        let text = |value: &str| Some(value.trim().to_string()).filter(|v| !v.is_empty());
        let mut fields = EventPatchParams::default();
        if self.title.trim() != was.title.trim() {
            fields.summary = Some(self.title.trim().to_string());
        }
        if self.all_day != was.all_day || self.span() != was.span() {
            let (start, end) = self.span();
            fields.start = Some(start);
            // An all-day event with no last day is one day long, which the
            // daemon would otherwise read as "keep its length".
            if end.is_none() && self.all_day {
                fields.duration = Some("P1D".into());
            }
            fields.end = end;
            fields.all_day = Some(self.all_day);
            fields.zone = Some(self.zone.clone());
        }
        if text(&self.location) != text(&was.location) {
            fields.location = Some(text(&self.location));
        }
        if self.body.trim_end() != was.body.trim_end() {
            fields.body = Some(Some(self.body.clone()).filter(|b| !b.trim().is_empty()));
        }
        let people = |form: &EventForm| {
            let mut listed: Vec<(String, Option<String>)> = form
                .attendees()
                .into_iter()
                .map(|a| (a.email.to_ascii_lowercase(), a.role))
                .collect();
            listed.sort();
            listed
        };
        if people(self) != people(was) {
            fields.attendees = Some(self.attendees());
        }
        if self.teams && !was.teams {
            fields.online_meeting = Some(true);
        }
        let reminder = self.reminder_minutes().unwrap_or(None);
        if reminder != was.reminder_minutes().unwrap_or(None) {
            fields.reminder = reminder;
        }
        if self.show_as != was.show_as {
            fields.show_as = Some(self.show_as.clone());
        }
        fields
    }

    /// `event.update`'s params for an edit form, or the form's problem.
    pub fn update_params(&self) -> Result<EventUpdateParams, String> {
        if let Some(problem) = self.problem() {
            return Err(problem);
        }
        let edit = self
            .editing
            .as_ref()
            .ok_or_else(|| "This form makes a new event".to_string())?;
        Ok(EventUpdateParams {
            account: edit.account.clone(),
            event: edit.event.clone(),
            fields: self.changed_fields(),
        })
    }

    /// `event.create`'s params, or the form's problem.
    pub fn params(&self) -> Result<EventCreateParams, String> {
        if let Some(problem) = self.problem() {
            return Err(problem);
        }
        let (start, end) = self.span();
        let attendees = self.attendees();
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
                show_as: (self.show_as != DEFAULT_SHOW_AS).then(|| self.show_as.clone()),
                ..Default::default()
            },
        })
    }
}

/// What the form says when an attendee changes the organizer's fields.
pub const ORGANIZER_ONLY: &str =
    "Only the organizer can change this event; the reminder and show-as are yours";

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

    fn meeting(organizer: bool) -> CalendarEvent {
        serde_json::from_value(serde_json::json!({
            "account": "evoqs", "calendar": "main", "id": "ev-1", "uid": "u",
            "summary": "Budget",
            "time": {"kind": "timed", "start": "2026-11-18T13:00:00Z",
                     "end": "2026-11-18T14:30:00Z", "zone": "Europe/Berlin"},
            "location": "Room 1",
            "attendees": [
                {"email": "ada@example.com", "role": "required", "status": "accepted"},
                {"email": "room-1@example.com", "role": "resource", "status": "accepted"},
            ],
            "own_response": if organizer { "organizer" } else { "accepted" },
            "is_organizer": organizer, "is_online_meeting": true, "reminder": 15,
            "sensitivity": "normal", "show_as": "busy",
        }))
        .unwrap()
    }

    fn edit_form(organizer: bool) -> EventForm {
        let main = form().calendars[0].clone();
        EventForm::edit(&meeting(organizer), main, "Europe/Berlin".parse().unwrap())
    }

    /// An edit form opens with the event's values in the view's zone, its
    /// people in their lists, and nothing to send until a field changes;
    /// then it sends only what changed, a time with its zone.
    #[test]
    fn an_edit_form_sends_only_what_changed() {
        let mut form = edit_form(true);
        assert_eq!(form.title, "Budget");
        assert_eq!(
            (form.date.as_str(), form.start.as_str()),
            ("2026-11-18", "14:00")
        );
        assert_eq!(form.end, "15:30");
        assert_eq!(form.to, "ada@example.com");
        assert_eq!(form.rooms, "room-1@example.com");
        assert_eq!(form.reminder, "15");
        assert!(form.teams);
        assert_eq!(form.problem().as_deref(), Some("Nothing changed yet"));

        form.start = "15:00".into();
        form.end = "16:00".into();
        form.location.clear();
        let params = form.update_params().unwrap();
        assert_eq!(
            (params.account.as_str(), params.event.as_str()),
            ("evoqs", "ev-1")
        );
        assert_eq!(
            serde_json::to_value(&params.fields).unwrap(),
            serde_json::json!({
                "start": "2026-11-18T15:00", "end": "2026-11-18T16:00",
                "zone": "Europe/Berlin", "all_day": false, "location": null,
            })
        );

        let mut people = edit_form(true);
        people.optional = "bob@example.com".into();
        let fields = people.update_params().unwrap().fields;
        assert_eq!(fields.attendees.as_ref().map(Vec::len), Some(3));
        assert_eq!(fields.start, None);

        let mut teams = edit_form(true);
        teams.toggle_teams();
        assert!(teams.teams, "a Teams meeting stays one");

        let mut day = edit_form(true);
        day.toggle_all_day();
        let fields = day.update_params().unwrap().fields;
        assert_eq!(fields.start.as_deref(), Some("2026-11-18"));
        assert_eq!(
            (fields.end, fields.duration.as_deref()),
            (None, Some("P1D"))
        );
    }

    /// An attendee's form changes the reminder and nothing else, and an
    /// empty reminder there is none rather than the mailbox default.
    #[test]
    fn an_attendee_edits_only_the_reminder() {
        let mut form = edit_form(false);
        assert_eq!(form.row, EventFormRow::Reminder);
        form.reminder.clear();
        assert_eq!(
            serde_json::to_value(form.update_params().unwrap().fields).unwrap(),
            serde_json::json!({"reminder": null})
        );
        form.title = "Mine now".into();
        assert!(form.problem().unwrap().starts_with("Only the organizer"));
    }

    /// A new event shows as busy, the server's default, which it does not
    /// send; `Left` and `Right` walk the five words, wrapping, and a word
    /// other than busy is sent.
    #[test]
    fn a_new_event_shows_as_busy_unless_changed() {
        let mut form = form();
        form.title = "Planning".into();
        assert_eq!(form.show_as_label(), "Busy");
        assert_eq!(form.params().unwrap().event.show_as, None);
        form.cycle_show_as(true);
        assert_eq!(form.show_as_label(), "Out of office");
        assert_eq!(form.params().unwrap().event.show_as.as_deref(), Some("oof"));
        form.cycle_show_as(true);
        form.cycle_show_as(true);
        assert_eq!(form.show_as, "free");
        form.cycle_show_as(false);
        assert_eq!(form.show_as, "working-elsewhere");
    }

    /// An edit sends the show-as only when it changed, and an attendee may
    /// change it on their copy, alone or with the reminder.
    #[test]
    fn an_edit_sends_the_show_as_it_changed() {
        let mut organizer = edit_form(true);
        assert_eq!(organizer.show_as, "busy");
        organizer.cycle_show_as(false);
        assert_eq!(
            serde_json::to_value(organizer.update_params().unwrap().fields).unwrap(),
            serde_json::json!({"show_as": "tentative"})
        );
        let mut attendee = edit_form(false);
        assert!(attendee.rows().contains(&EventFormRow::ShowAs));
        attendee.cycle_show_as(true);
        attendee.reminder = "5".into();
        assert_eq!(
            serde_json::to_value(attendee.update_params().unwrap().fields).unwrap(),
            serde_json::json!({"reminder": 5, "show_as": "oof"})
        );
        let mut unknown = meeting(true);
        unknown.show_as = "unknown".into();
        let main = form().calendars[0].clone();
        let mut form = EventForm::edit(&unknown, main, "Europe/Berlin".parse().unwrap());
        assert_eq!(form.show_as_label(), "unknown");
        assert_eq!(form.problem().as_deref(), Some("Nothing changed yet"));
        form.cycle_show_as(true);
        assert_eq!(form.show_as, "free");
    }

    /// An event the server holds without a title still saves its other
    /// fields, an attendee's reminder among them; a title the event had
    /// cannot be cleared.
    #[test]
    fn an_untitled_event_saves_without_a_title() {
        let main = form().calendars[0].clone();
        let mut untitled = meeting(false);
        untitled.summary = String::new();
        let mut form = EventForm::edit(&untitled, main.clone(), "Europe/Berlin".parse().unwrap());
        form.reminder = "5".into();
        assert_eq!(
            serde_json::to_value(form.update_params().unwrap().fields).unwrap(),
            serde_json::json!({"reminder": 5})
        );
        let mut cleared = edit_form(true);
        cleared.title = " ".into();
        cleared.reminder = "5".into();
        assert_eq!(
            cleared.problem().as_deref(),
            Some("The event needs a title")
        );
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
