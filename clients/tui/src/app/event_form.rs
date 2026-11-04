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
//!
//! A new event repeats through the Repeat row (`Left` and `Right` walk the
//! patterns Outlook's repeat menu offers, read against the form's date)
//! with its interval, its weekdays and its end on the rows under it; the
//! row shows the rule in words. `E` on an occurrence of a series asks first
//! whether the change is to that occurrence ([`EventForm::edit_occurrence`],
//! whose Repeat row only names its series) or to the whole series
//! ([`EventForm::edit_series`], the master's values and rule, which a new
//! pattern replaces as a whole).

use chrono::{Datelike, Days, NaiveDate, NaiveTime};
use mp_core::calendar_model::{WeekIndex, Weekday};
use mp_core::calendar_write::resolve_recurrence;
use mp_protocol::calendar::{
    CalendarEvent, CalendarEventTime, CalendarInfo, CalendarRecurrence, EventAttendeeInput,
    EventCreateParams, EventInputParams, EventPatchParams, EventUpdateParams,
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
    /// `occurrence` or `series` for a change to a recurring event, which
    /// `event` names: the occurrence, or the series master.
    pub scope: Option<String>,
    pub original: Box<EventForm>,
}

/// The patterns the Repeat row walks, Outlook's repeat menu's, each read
/// against the form's date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatPattern {
    Daily,
    Weekly,
    /// On the date's day of the month.
    MonthlyByDate,
    /// On the date's weekday in the same week of the month.
    MonthlyByWeekday,
    /// On the date's day and month.
    Yearly,
}

impl RepeatPattern {
    /// In the order `Right` walks them.
    pub const ALL: [RepeatPattern; 5] = [
        RepeatPattern::Daily,
        RepeatPattern::Weekly,
        RepeatPattern::MonthlyByDate,
        RepeatPattern::MonthlyByWeekday,
        RepeatPattern::Yearly,
    ];

    fn frequency(self) -> &'static str {
        match self {
            RepeatPattern::Daily => "daily",
            RepeatPattern::Weekly => "weekly",
            RepeatPattern::MonthlyByDate | RepeatPattern::MonthlyByWeekday => "monthly",
            RepeatPattern::Yearly => "yearly",
        }
    }
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
    /// The pattern, or the series an occurrence belongs to.
    Repeat,
    /// Every how many days, weeks, months or years.
    Every,
    /// The weekdays of a weekly series.
    RepeatOn,
    /// The series' last day, its number of occurrences, or never.
    Ends,
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
                | EventFormRow::Repeat
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
    /// How the event repeats; `None` for a single event, and for a series
    /// whose rule no pattern names ([`Self::kept_rule`]).
    pub repeat: Option<RepeatPattern>,
    /// Every how many units of the pattern, `1` by default.
    pub every: String,
    /// A weekly series' days as two-letter codes (`MO, WE`); empty for the
    /// date's own.
    pub repeat_on: String,
    /// The series' end: empty for never, `YYYY-MM-DD` for its last day, a
    /// number for its count of occurrences.
    pub ends: String,
    /// The rule of a series being edited that no pattern of the form names
    /// (the Nth weekday of a month every year, several weekdays of a
    /// month), kept as it is until a pattern replaces it.
    pub kept_rule: Option<CalendarRecurrence>,
    /// The series an occurrence being edited on its own belongs to, in
    /// words.
    pub series_rule: Option<String>,
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
            repeat: None,
            every: "1".into(),
            repeat_on: String::new(),
            ends: String::new(),
            kept_rule: None,
            series_rule: None,
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
        let local = |raw: &str| {
            chrono::DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|at| at.with_timezone(&zone).naive_local())
        };
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
                let (start, end) = (local(start), local(end));
                let date = start.map(|s| s.date().to_string()).unwrap_or_default();
                let start_clock = start.map(|s| s.format("%H:%M").to_string());
                let end = match (start, end) {
                    (Some(s), Some(e)) if s.date() == e.date() => e.format("%H:%M").to_string(),
                    (_, Some(e)) => e.format("%Y-%m-%dT%H:%M").to_string(),
                    _ => String::new(),
                };
                (false, date, start_clock.unwrap_or_default(), end)
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
            repeat: None,
            every: "1".into(),
            repeat_on: String::new(),
            ends: String::new(),
            kept_rule: None,
            series_rule: None,
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
            scope: None,
            original: Box::new(form.clone()),
        });
        form
    }

    /// A form on the occurrence `event` alone: its own values, its Repeat
    /// row naming the series it stays part of.
    pub fn edit_occurrence(event: &CalendarEvent, calendar: EventCalendarChoice, zone: Tz) -> Self {
        let mut form = EventForm::edit(event, calendar, zone);
        form.series_rule = Some(
            event
                .recurrence
                .clone()
                .unwrap_or_else(|| "A recurring series".into()),
        );
        form.rescope("occurrence");
        form
    }

    /// A form on the whole series whose master is `master`, filled with the
    /// master's values and `rule`, the series' own: a pattern the form
    /// names fills the Repeat rows, any other is kept as it is until a
    /// pattern replaces it.
    pub fn edit_series(
        master: &CalendarEvent,
        rule: Option<&CalendarRecurrence>,
        calendar: EventCalendarChoice,
        zone: Tz,
    ) -> Self {
        let mut form = EventForm::edit(master, calendar, zone);
        if let Some(rule) = rule {
            form.take_rule(rule);
        }
        form.rescope("series");
        form
    }

    /// Mark the edit as one in `scope`, the form as it is now its original.
    fn rescope(&mut self, scope: &str) {
        let mut snapshot = self.clone();
        snapshot.editing = None;
        if let Some(edit) = self.editing.as_mut() {
            edit.scope = Some(scope.to_string());
            *edit.original = snapshot;
        }
    }

    /// Fill the Repeat rows from `rule`, or keep it whole when no pattern
    /// of the form names it.
    fn take_rule(&mut self, rule: &CalendarRecurrence) {
        let date = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d").ok();
        let weekday = date.map(Weekday::of_date);
        let one_own_day =
            rule.weekdays.len() == 1 && weekday.is_some_and(|day| rule.weekdays[0] == day.word());
        let pattern = match (rule.frequency.as_str(), rule.week_index.as_deref()) {
            ("daily", _) => Some(RepeatPattern::Daily),
            ("weekly", _) => Some(RepeatPattern::Weekly),
            ("monthly", None)
                if rule.day_of_month.is_none() || rule.day_of_month == date.map(|d| d.day()) =>
            {
                Some(RepeatPattern::MonthlyByDate)
            }
            ("monthly", Some(index))
                if one_own_day && date.map(|d| WeekIndex::of_date(d).word()) == Some(index) =>
            {
                Some(RepeatPattern::MonthlyByWeekday)
            }
            ("yearly", None)
                if (rule.day_of_month.is_none() || rule.day_of_month == date.map(|d| d.day()))
                    && (rule.month.is_none() || rule.month == date.map(|d| d.month())) =>
            {
                Some(RepeatPattern::Yearly)
            }
            _ => None,
        };
        self.every = rule.interval.max(1).to_string();
        self.ends = match (&rule.until, rule.count) {
            (Some(until), _) => until.clone(),
            (None, Some(count)) => count.to_string(),
            (None, None) => String::new(),
        };
        match pattern {
            Some(pattern) => {
                self.repeat = Some(pattern);
                if pattern == RepeatPattern::Weekly {
                    self.repeat_on = rule
                        .weekdays
                        .iter()
                        .filter_map(|word| Weekday::from_name(word))
                        .map(|day| title_code(day.code()))
                        .collect::<Vec<_>>()
                        .join(", ");
                }
            }
            None => self.kept_rule = Some(rule.clone()),
        }
    }

    /// The scope of an edit, `None` on a new event and a single event.
    pub fn scope(&self) -> Option<&str> {
        self.editing.as_ref().and_then(|edit| edit.scope.as_deref())
    }

    /// Move to the next or the previous repeat pattern: a new event walks
    /// "does not repeat" too, a series always repeats, and an occurrence
    /// repeats by its series.
    pub fn cycle_repeat(&mut self, forward: bool) {
        let mut options: Vec<Option<RepeatPattern>> =
            RepeatPattern::ALL.into_iter().map(Some).collect();
        match self.scope() {
            Some("occurrence") => return,
            Some(_) => {}
            None => options.insert(0, None),
        }
        let len = options.len();
        let at = options.iter().position(|option| *option == self.repeat);
        let to = match (at, forward) {
            (Some(at), true) => (at + 1) % len,
            (Some(at), false) => (at + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        };
        self.repeat = options[to];
        self.kept_rule = None;
        self.error = None;
    }

    /// The rule the Repeat rows name, read against the form's date: `None`
    /// for an event that does not repeat and for an occurrence, which keeps
    /// its series'; the sentence that says what is wrong otherwise.
    pub fn rule(&self) -> Result<Option<CalendarRecurrence>, String> {
        if self.scope() == Some("occurrence") {
            return Ok(None);
        }
        let Some(pattern) = self.repeat else {
            return Ok(self.kept_rule.clone());
        };
        let interval = self
            .every
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=99).contains(n))
            .ok_or_else(|| "Every is a number from 1 to 99".to_string())?;
        let mut weekdays = Vec::new();
        if pattern == RepeatPattern::Weekly {
            for code in addresses(&self.repeat_on) {
                let day = Weekday::from_code(code)
                    .or_else(|| Weekday::from_name(code))
                    .ok_or_else(|| {
                        format!("{code:?} is not a weekday; use MO, TU, WE, TH, FR, SA or SU")
                    })?;
                weekdays.push(day.word().to_string());
            }
        }
        let ends = self.ends.trim();
        let (until, count) =
            if ends.is_empty() || ends.eq_ignore_ascii_case("never") {
                (None, None)
            } else if NaiveDate::parse_from_str(ends, "%Y-%m-%d").is_ok() {
                (Some(ends.to_string()), None)
            } else {
                let digits = ends.trim_end_matches(|c: char| !c.is_ascii_digit());
                match digits.parse::<u32>() {
                    Ok(count) if count > 0 => (None, Some(count)),
                    _ => return Err(
                        "Ends is a last day (YYYY-MM-DD), a number of times, or empty for never"
                            .into(),
                    ),
                }
            };
        let date = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d")
            .map_err(|_| "The date is YYYY-MM-DD".to_string())?;
        let rule = CalendarRecurrence {
            frequency: pattern.frequency().to_string(),
            interval,
            weekdays,
            day_of_month: None,
            week_index: (pattern == RepeatPattern::MonthlyByWeekday)
                .then(|| WeekIndex::of_date(date).word().to_string()),
            month: None,
            first_day_of_week: None,
            start: None,
            until,
            count,
        };
        resolve_recurrence(&rule, date)?;
        Ok(Some(rule))
    }

    /// The Repeat row's words: the rule as the event cards print it, the
    /// series an occurrence stays part of, or that the event does not
    /// repeat.
    pub fn repeat_label(&self) -> String {
        if let Some(series) = &self.series_rule {
            return format!("{series} (this occurrence only)");
        }
        let date = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d").ok();
        match (self.rule(), date) {
            (Ok(Some(rule)), Some(date)) => resolve_recurrence(&rule, date)
                .map(|rule| rule.describe())
                .unwrap_or_else(|why| why),
            (Ok(None), _) => "Does not repeat".to_string(),
            (Ok(Some(_)), None) | (Err(_), _) => match self.repeat {
                Some(RepeatPattern::Daily) => "Daily".into(),
                Some(RepeatPattern::Weekly) => "Weekly".into(),
                Some(RepeatPattern::MonthlyByDate) => "Monthly on its day".into(),
                Some(RepeatPattern::MonthlyByWeekday) => "Monthly on its weekday".into(),
                Some(RepeatPattern::Yearly) => "Yearly".into(),
                None => "Does not repeat".into(),
            },
        }
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
    /// where the backend has no online meetings, the Repeat row on a new
    /// event and a recurring one, with the rows that shape a rule under it
    /// while the form makes one.
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
            Repeat,
            Every,
            RepeatOn,
            Ends,
            Description,
        ]
        .into_iter()
        .filter(|row| match row {
            Start => !self.all_day,
            Teams => self.chosen().teams || self.editing.as_ref().is_some_and(|e| e.online),
            Repeat => self.editing.is_none() || self.scope().is_some(),
            Every | Ends => self.repeat.is_some() && self.scope() != Some("occurrence"),
            RepeatOn => self.repeat == Some(RepeatPattern::Weekly),
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
            EventFormRow::Every => &mut self.every,
            EventFormRow::RepeatOn => &mut self.repeat_on,
            EventFormRow::Ends => &mut self.ends,
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
        if let Err(problem) = self.rule() {
            return Some(problem);
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
        let rule = self.rule().unwrap_or(None);
        if self.scope() == Some("series") && rule != was.rule().unwrap_or(None) {
            fields.recurrence = rule;
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
            scope: edit.scope.clone(),
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
                recurrence: self.rule().unwrap_or(None),
                ..Default::default()
            },
        })
    }
}

/// What the form says when an attendee changes the organizer's fields.
pub const ORGANIZER_ONLY: &str =
    "Only the organizer can change this event; the reminder and show-as are yours";

/// A weekday code as the form shows it (`Mo`).
fn title_code(code: &str) -> String {
    let mut chars = code.chars();
    match chars.next() {
        Some(first) => first.to_string() + &chars.as_str().to_ascii_lowercase(),
        None => String::new(),
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

    /// The Repeat row walks "does not repeat" and the five patterns, the
    /// rows that shape a rule showing only while the form makes one; the
    /// rule goes to `event.create` read against the form's date, and a
    /// wrong interval or end is the form's problem.
    #[test]
    fn a_new_event_repeats_by_the_pattern_it_names() {
        let mut form = form();
        form.title = "Standup".into();
        assert!(form.rows().contains(&EventFormRow::Repeat));
        assert!(!form.rows().contains(&EventFormRow::Every));
        assert_eq!(form.repeat_label(), "Does not repeat");
        assert_eq!(form.params().unwrap().event.recurrence, None);
        form.cycle_repeat(false);
        assert_eq!(form.repeat, Some(RepeatPattern::Yearly));
        assert_eq!(form.repeat_label(), "Yearly on 18 November");
        form.cycle_repeat(false);
        assert_eq!(form.repeat_label(), "Monthly on the third Wednesday");
        let rule = form.params().unwrap().event.recurrence.unwrap();
        assert_eq!(rule.week_index.as_deref(), Some("third"));
        form.cycle_repeat(false);
        form.cycle_repeat(false);
        assert_eq!(form.repeat, Some(RepeatPattern::Weekly));
        assert!(form.rows().contains(&EventFormRow::RepeatOn));
        form.repeat_on = "mo, fr".into();
        form.every = "2".into();
        form.ends = "6 times".into();
        assert_eq!(
            form.repeat_label(),
            "Every 2 weeks on Monday, Friday (6 times)"
        );
        let rule = form.params().unwrap().event.recurrence.unwrap();
        assert_eq!(rule.weekdays, ["monday", "friday"]);
        assert_eq!((rule.interval, rule.count), (2, Some(6)));
        form.every = "0".into();
        assert_eq!(
            form.problem().as_deref(),
            Some("Every is a number from 1 to 99")
        );
        form.every = "1".into();
        form.ends = "2026-11-01".into();
        assert!(form
            .problem()
            .unwrap()
            .contains("ends before its first day"));
        form.ends = "soon".into();
        assert!(form.problem().unwrap().starts_with("Ends is"));
    }

    /// A series form fills its Repeat rows from the series' rule and sends
    /// a new one only when it changed; a rule no pattern names is kept as
    /// it is until a pattern replaces it; an occurrence's form names its
    /// series and sends no rule.
    #[test]
    fn a_series_form_sends_its_rule_only_when_it_changed() {
        let main = form().calendars[0].clone();
        let zone: Tz = "Europe/Berlin".parse().unwrap();
        let mut master = meeting(true);
        master.kind = "series-master".into();
        let weekly: CalendarRecurrence = serde_json::from_value(serde_json::json!({
            "frequency": "weekly", "interval": 1, "weekdays": ["wednesday"], "count": 4,
        }))
        .unwrap();
        let mut form = EventForm::edit_series(&master, Some(&weekly), main.clone(), zone);
        assert_eq!(form.scope(), Some("series"));
        assert_eq!(form.repeat, Some(RepeatPattern::Weekly));
        assert_eq!((form.repeat_on.as_str(), form.ends.as_str()), ("We", "4"));
        assert_eq!(form.problem().as_deref(), Some("Nothing changed yet"));
        form.title = "Budget, weekly".into();
        let params = form.update_params().unwrap();
        assert_eq!(params.scope.as_deref(), Some("series"));
        assert_eq!(params.fields.recurrence, None);
        form.ends = "2".into();
        let rule = form.update_params().unwrap().fields.recurrence.unwrap();
        assert_eq!(rule.count, Some(2));
        form.cycle_repeat(true);
        form.cycle_repeat(false);
        form.cycle_repeat(false);
        assert_eq!(
            form.repeat,
            Some(RepeatPattern::Daily),
            "a series always repeats"
        );

        let odd: CalendarRecurrence = serde_json::from_value(serde_json::json!({
            "frequency": "yearly", "interval": 1, "weekdays": ["thursday"],
            "week_index": "fourth", "month": 11,
        }))
        .unwrap();
        let mut kept = EventForm::edit_series(&master, Some(&odd), main.clone(), zone);
        assert_eq!(kept.repeat, None);
        assert_eq!(
            kept.repeat_label(),
            "Yearly on the fourth Thursday of November"
        );
        kept.title = "Thanksgiving".into();
        assert_eq!(kept.update_params().unwrap().fields.recurrence, None);
        kept.cycle_repeat(true);
        assert_eq!(kept.repeat, Some(RepeatPattern::Daily));
        assert!(kept.update_params().unwrap().fields.recurrence.is_some());

        let mut occurrence = meeting(true);
        occurrence.kind = "occurrence".into();
        occurrence.recurrence = Some("Weekly on Wednesday".into());
        let mut alone = EventForm::edit_occurrence(&occurrence, main, zone);
        assert_eq!(
            alone.repeat_label(),
            "Weekly on Wednesday (this occurrence only)"
        );
        assert!(!alone.rows().contains(&EventFormRow::Every));
        alone.cycle_repeat(true);
        assert_eq!(alone.repeat, None);
        alone.title = "Budget, moved".into();
        let params = alone.update_params().unwrap();
        assert_eq!(params.scope.as_deref(), Some("occurrence"));
        assert_eq!(params.fields.recurrence, None);
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
