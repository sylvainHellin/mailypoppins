//! The neutral calendar event model (PERSO-200, `docs/plans/calendar.md`,
//! "Neutral event model").
//!
//! Every calendar backend (Graph, then EWS, then CalDAV) converts its events
//! into these types, and the engine merges, deduplicates and serves them
//! without knowing which backend an event came from (D7).
//!
//! Times are UTC instants plus the IANA zone of the event, so a view renders
//! in the user's zone and an edit keeps the organizer's. An all-day event is a
//! date range with an exclusive end, as in iCalendar: a one-day event on
//! 3 November is `2026-11-03..2026-11-04`. Graph and EWS deliver the same
//! event as a midnight-to-midnight pair, which
//! [`EventTime::all_day_from_midnights`] converts and
//! [`EventTime::all_day_midnights`] gives back.
//!
//! This module is the iMIP-free half: [`crate::calendar`] keeps parsing
//! invitation mails into their frontmatter block, unchanged.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Months, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// The zone type this module's API speaks, re-exported so a client that
/// reads the model's times (the TUI's calendar grid) names the same type
/// without a dependency of its own on `chrono-tz`.
pub use chrono_tz::Tz;

/// A calendar's server id, as its backend names it. Ids are plain strings
/// across the protocol, and this alias only documents which one is meant.
pub type CalendarId = String;

/// An event's server id: a Graph immutable id, an EWS `ItemId`, a CalDAV
/// resource href. An occurrence of a series has an id of its own.
pub type EventId = String;

/// How far back the cached window reaches from today (D6).
pub const WINDOW_MONTHS_BACK: u32 = 1;

/// How far ahead the cached window reaches from today (D6).
pub const WINDOW_MONTHS_AHEAD: u32 = 12;

/// One calendar of an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    /// The configured account name the calendar belongs to.
    pub account: String,
    /// The server id.
    pub id: CalendarId,
    pub name: String,
    /// `#rrggbb`, when the server names a colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The account's default calendar, where a new event goes.
    #[serde(default)]
    pub is_default: bool,
    /// Whether the user may create and change events in it.
    #[serde(default)]
    pub can_edit: bool,
    /// Shown in the views. A local preference, not a server property: it
    /// survives a cache rebuild, and a backend always reports `true`.
    #[serde(default = "default_true")]
    pub visible: bool,
}

fn default_true() -> bool {
    true
}

/// When an event happens.
///
/// Deserialising validates what the constructors validate, so an
/// `EventTime` read back from the store or the wire is as sound as one built
/// by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", try_from = "RawEventTime")]
pub enum EventTime {
    /// A timed event: UTC instants and the IANA zone the event was written in.
    Timed {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        zone: String,
    },
    /// An all-day event: local dates, `end` exclusive.
    AllDay { start: NaiveDate, end: NaiveDate },
}

/// The unvalidated shape [`EventTime`] deserialises through.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum RawEventTime {
    Timed {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        zone: String,
    },
    AllDay {
        start: NaiveDate,
        end: NaiveDate,
    },
}

impl TryFrom<RawEventTime> for EventTime {
    type Error = EventTimeError;

    fn try_from(raw: RawEventTime) -> Result<Self, Self::Error> {
        match raw {
            RawEventTime::Timed { start, end, zone } => EventTime::timed(start, end, &zone),
            RawEventTime::AllDay { start, end } => EventTime::all_day(start, end),
        }
    }
}

/// Why an [`EventTime`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTimeError {
    /// The zone is not an IANA zone chrono-tz knows. A Windows zone name
    /// (`W. Europe Standard Time`) lands here until the adapter maps it.
    UnknownZone(String),
    /// The end comes before the start, or an all-day range is empty.
    EndBeforeStart,
    /// An all-day bound given as a date-time is not at midnight.
    NotMidnight(NaiveDateTime),
}

impl fmt::Display for EventTimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EventTimeError::UnknownZone(zone) => write!(f, "unknown IANA time zone {zone:?}"),
            EventTimeError::EndBeforeStart => write!(f, "the event ends before it starts"),
            EventTimeError::NotMidnight(at) => {
                write!(f, "an all-day bound must be at midnight, got {at}")
            }
        }
    }
}

impl std::error::Error for EventTimeError {}

impl EventTime {
    /// A timed event; the zone must be an IANA zone and `end` not before
    /// `start` (a zero-length event is valid).
    pub fn timed(
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        zone: &str,
    ) -> Result<Self, EventTimeError> {
        Tz::from_str(zone).map_err(|_| EventTimeError::UnknownZone(zone.to_string()))?;
        if end < start {
            return Err(EventTimeError::EndBeforeStart);
        }
        Ok(EventTime::Timed {
            start,
            end,
            zone: zone.to_string(),
        })
    }

    /// An all-day event over `start..end`, `end` exclusive, so at least one
    /// day long.
    pub fn all_day(start: NaiveDate, end: NaiveDate) -> Result<Self, EventTimeError> {
        if end <= start {
            return Err(EventTimeError::EndBeforeStart);
        }
        Ok(EventTime::AllDay { start, end })
    }

    /// An all-day event from the midnight-to-midnight pair Graph and EWS
    /// deliver: a one-day event on 3 November is `2026-11-03T00:00` to
    /// `2026-11-04T00:00`, and the second midnight is already the exclusive
    /// end date.
    pub fn all_day_from_midnights(
        start: NaiveDateTime,
        end: NaiveDateTime,
    ) -> Result<Self, EventTimeError> {
        for bound in [start, end] {
            if bound.time() != NaiveTime::MIN {
                return Err(EventTimeError::NotMidnight(bound));
            }
        }
        EventTime::all_day(start.date(), end.date())
    }

    /// The midnight-to-midnight pair a Graph or EWS write wants, for an
    /// all-day event; `None` for a timed one.
    pub fn all_day_midnights(&self) -> Option<(NaiveDateTime, NaiveDateTime)> {
        match self {
            EventTime::AllDay { start, end } => {
                Some((start.and_time(NaiveTime::MIN), end.and_time(NaiveTime::MIN)))
            }
            EventTime::Timed { .. } => None,
        }
    }

    /// The last day an all-day event covers, inclusive, the day a view
    /// prints as its end; `None` for a timed event.
    pub fn last_day(&self) -> Option<NaiveDate> {
        match self {
            EventTime::AllDay { end, .. } => end.pred_opt(),
            EventTime::Timed { .. } => None,
        }
    }

    pub fn is_all_day(&self) -> bool {
        matches!(self, EventTime::AllDay { .. })
    }

    /// The event's own zone, for a timed event.
    pub fn zone(&self) -> Option<Tz> {
        match self {
            EventTime::Timed { zone, .. } => Tz::from_str(zone).ok(),
            EventTime::AllDay { .. } => None,
        }
    }

    /// What the event occupies, in the terms it was written in.
    pub fn occupancy(&self) -> Occupancy {
        match self {
            EventTime::Timed { start, end, .. } => Occupancy::Instants {
                start: *start,
                end: *end,
            },
            EventTime::AllDay { start, end } => Occupancy::Dates {
                start: *start,
                end: *end,
            },
        }
    }

    /// The occupied interval as UTC instants, half-open, for window queries.
    ///
    /// An all-day event has no instants of its own: it covers its dates in
    /// whichever zone it is looked at from, so `viewer` places its midnights.
    pub fn utc_interval(&self, viewer: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
        match self {
            EventTime::Timed { start, end, .. } => (*start, *end),
            EventTime::AllDay { start, end } => {
                (local_midnight(viewer, *start), local_midnight(viewer, *end))
            }
        }
    }

    /// Whether the event falls in `window`, seen from `viewer`. A zero-length
    /// event counts when its instant is inside the window.
    pub fn overlaps(&self, window: &CalendarWindow, viewer: Tz) -> bool {
        let (start, end) = self.utc_interval(viewer);
        if start == end {
            return window.start <= start && start < window.end;
        }
        start < window.end && end > window.start
    }
}

/// What an event occupies: instants for a timed event, a local date range
/// with an exclusive end for an all-day one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    Instants {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    Dates {
        start: NaiveDate,
        end: NaiveDate,
    },
}

/// The viewer's own zone: `TZ` when it names an IANA zone, else the zone
/// `/etc/localtime` links to, else UTC. The daemon's default zone and the
/// zone the CLI and the TUI lay their calendar views out in.
pub fn local_zone() -> Tz {
    if let Some(zone) = std::env::var("TZ")
        .ok()
        .and_then(|tz| parse_zone(tz.trim_start_matches(':')))
    {
        return zone;
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| {
            let target = target.to_string_lossy().into_owned();
            target
                .split_once("zoneinfo/")
                .and_then(|(_, name)| parse_zone(name))
        })
        .unwrap_or(Tz::UTC)
}

/// An IANA zone by name.
pub fn parse_zone(name: &str) -> Option<Tz> {
    Tz::from_str(name.trim()).ok()
}

/// The instant `day` starts in `zone`: its midnight, or the first instant
/// after it when a daylight-saving jump skips midnight there. A view's day
/// runs from one of these to the next.
pub fn day_start(zone: Tz, day: NaiveDate) -> DateTime<Utc> {
    let midnight = day.and_time(NaiveTime::MIN);
    (0..=3)
        .find_map(|hour| {
            zone.from_local_datetime(&(midnight + chrono::Duration::hours(hour)))
                .earliest()
        })
        .map(|instant| instant.with_timezone(&Utc))
        .unwrap_or_else(|| midnight.and_utc())
}

/// Midnight of `date` in `zone` as an instant, a midnight lost to a DST gap
/// resolved the way RFC 5545 resolves any local time.
fn local_midnight(zone: Tz, date: NaiveDate) -> DateTime<Utc> {
    let local = date.and_time(NaiveTime::MIN);
    crate::calendar::resolve_local(&zone, &local)
        .map(|zoned| zoned.with_timezone(&Utc))
        .unwrap_or_else(|| local.and_utc())
}

/// A person named on an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

/// An attendee's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttendeeRole {
    Required,
    Optional,
    /// A room or equipment, booked by inviting its mailbox.
    Resource,
}

/// An attendee's answer, in the vocabulary the iMIP path already uses
/// (`needs-action`, `accepted`, `tentative`, `declined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseStatus {
    NeedsAction,
    Accepted,
    Tentative,
    Declined,
}

/// A span of time as UTC instants, half-open: a time an attendee proposed
/// instead of the event's (Graph `timeSlot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSlot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// One attendee with role and status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
    pub role: AttendeeRole,
    pub status: ResponseStatus,
    /// The time the attendee proposed instead, which the server shows the
    /// organizer on the event (Graph `proposedNewTime`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_time: Option<TimeSlot>,
}

/// The user's own standing on an event: an attendee's answer, the organizer,
/// or nothing (an event in the user's calendar with no meeting attached).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OwnResponse {
    None,
    Organizer,
    NeedsAction,
    Accepted,
    Tentative,
    Declined,
}

/// The event's sensitivity (Graph `sensitivity`, EWS `Sensitivity`, iCalendar
/// `CLASS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sensitivity {
    Normal,
    Personal,
    Private,
    Confidential,
}

/// How the event shows on the user's free/busy (Graph `showAs`, EWS
/// `LegacyFreeBusyStatus`, iCalendar `TRANSP` and `X-MICROSOFT-CDO-BUSYSTATUS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShowAs {
    Free,
    Tentative,
    Busy,
    Oof,
    WorkingElsewhere,
    Unknown,
}

/// What an event is to its series (Graph `type`, iCalendar `RRULE` and
/// `RECURRENCE-ID`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// An event of its own, in no series.
    #[default]
    Single,
    /// The series itself, which carries the rule; the views show its
    /// occurrences rather than it.
    SeriesMaster,
    /// One occurrence of a series, as the rule placed it.
    Occurrence,
    /// One occurrence of a series, changed apart from the rule (moved,
    /// renamed, answered differently).
    Exception,
}

impl EventKind {
    /// Whether the event is one occurrence of a series, changed or not.
    pub fn is_instance(self) -> bool {
        matches!(self, EventKind::Occurrence | EventKind::Exception)
    }
}

/// The service an online meeting runs on (Graph `onlineMeetingProvider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnlineMeetingProvider {
    TeamsForBusiness,
    SkypeForBusiness,
    SkypeForConsumer,
}

/// What a calendar backend lets a client do beyond reading the calendar and
/// answering an invitation, which `calendar.list` reports per calendar so a
/// client greys out what an account lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CalendarFeature {
    /// Write events: create them through `event.create`.
    Write,
    /// Give an event an online meeting (a Teams link).
    OnlineMeeting,
    /// Propose a new time when answering.
    ProposeTime,
    /// The free/busy of other people and rooms.
    FreeBusy,
    /// Dismiss and snooze a reminder on the server.
    ServerReminders,
}

impl CalendarFeature {
    /// The feature's word on the wire, as serde spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            CalendarFeature::Write => "write",
            CalendarFeature::OnlineMeeting => "online-meeting",
            CalendarFeature::ProposeTime => "propose-time",
            CalendarFeature::FreeBusy => "free-busy",
            CalendarFeature::ServerReminders => "server-reminders",
        }
    }
}

/// How often a series repeats (iCalendar `FREQ`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A day of the week, in a recurrence rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// The day by its English name, any case (`wednesday`, Graph's spelling).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "monday" => Weekday::Monday,
            "tuesday" => Weekday::Tuesday,
            "wednesday" => Weekday::Wednesday,
            "thursday" => Weekday::Thursday,
            "friday" => Weekday::Friday,
            "saturday" => Weekday::Saturday,
            "sunday" => Weekday::Sunday,
            _ => return None,
        })
    }

    /// The iCalendar two-letter code (`WE`).
    pub fn code(self) -> &'static str {
        match self {
            Weekday::Monday => "MO",
            Weekday::Tuesday => "TU",
            Weekday::Wednesday => "WE",
            Weekday::Thursday => "TH",
            Weekday::Friday => "FR",
            Weekday::Saturday => "SA",
            Weekday::Sunday => "SU",
        }
    }

    /// The English name, capitalised (`Wednesday`).
    pub fn name(self) -> &'static str {
        match self {
            Weekday::Monday => "Monday",
            Weekday::Tuesday => "Tuesday",
            Weekday::Wednesday => "Wednesday",
            Weekday::Thursday => "Thursday",
            Weekday::Friday => "Friday",
            Weekday::Saturday => "Saturday",
            Weekday::Sunday => "Sunday",
        }
    }
}

/// Which week of the month a relative monthly or yearly rule names (Graph
/// `weekIndex`, iCalendar `BYSETPOS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WeekIndex {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

impl WeekIndex {
    /// The index by Graph's word for it (`first` to `fourth`, `last`).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "first" => WeekIndex::First,
            "second" => WeekIndex::Second,
            "third" => WeekIndex::Third,
            "fourth" => WeekIndex::Fourth,
            "last" => WeekIndex::Last,
            _ => return None,
        })
    }

    /// The iCalendar `BYSETPOS` value: 1 to 4, or -1 for the last.
    pub fn position(self) -> i8 {
        match self {
            WeekIndex::First => 1,
            WeekIndex::Second => 2,
            WeekIndex::Third => 3,
            WeekIndex::Fourth => 4,
            WeekIndex::Last => -1,
        }
    }

    fn word(self) -> &'static str {
        match self {
            WeekIndex::First => "first",
            WeekIndex::Second => "second",
            WeekIndex::Third => "third",
            WeekIndex::Fourth => "fourth",
            WeekIndex::Last => "last",
        }
    }
}

/// The rule of a recurring series, in the terms Graph's `patternedRecurrence`
/// and Outlook's repeat menu use: a frequency and an interval, the weekdays,
/// day of month, week index and month the pattern names, and a range that
/// ends on a date, after a count, or never.
///
/// Which fields a pattern reads:
///
/// - daily: `interval`;
/// - weekly: `weekdays` and `first_day_of_week`;
/// - monthly on a date: `day_of_month`;
/// - monthly on a weekday: `week_index` and `weekdays`;
/// - yearly on a date: `month` and `day_of_month`;
/// - yearly on a weekday: `month`, `week_index` and `weekdays`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recurrence {
    pub frequency: Frequency,
    /// Every how many days, weeks, months or years; at least 1.
    #[serde(default = "one")]
    pub interval: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weekdays: Vec<Weekday>,
    /// 1 to 31.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_of_month: Option<u32>,
    /// Set on a relative rule (the last Thursday), unset on one by date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_index: Option<WeekIndex>,
    /// 1 to 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_day_of_week: Option<Weekday>,
    /// The day the range starts, in the series' zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<NaiveDate>,
    /// The last day an occurrence may fall on, inclusive; `None` with no
    /// `count` is a series without end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<NaiveDate>,
    /// How many occurrences the series has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

fn one() -> u32 {
    1
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The last day an `UNTIL` value allows, in the event's `zone`: a UTC
/// date-time on its day there, a floating date-time or a date on its own.
fn until_date(value: &str, zone: Tz) -> Option<NaiveDate> {
    let value = value.trim().to_ascii_uppercase();
    if let Some(utc) = value.strip_suffix('Z') {
        let at = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some(at.and_utc().with_timezone(&zone).date_naive());
    }
    if value.len() == 8 {
        return NaiveDate::parse_from_str(&value, "%Y%m%d").ok();
    }
    NaiveDateTime::parse_from_str(&value, "%Y%m%dT%H%M%S")
        .ok()
        .map(|at| at.date())
}

/// The weekday an iCalendar two-letter code (`WE`) names.
fn weekday_of_code(code: &str) -> Option<Weekday> {
    [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ]
    .into_iter()
    .find(|day| day.code() == code)
}

fn month_name(month: u32) -> Option<&'static str> {
    MONTH_NAMES.get(month.checked_sub(1)? as usize).copied()
}

impl Recurrence {
    /// The rule an iCalendar `RRULE` value (`FREQ=WEEKLY;BYDAY=WE`) states,
    /// or `None` for a rule this model cannot hold: another frequency, a
    /// list of month days or months, an hour or week-number part, weekdays
    /// with different ordinals, both `UNTIL` and `COUNT`, or a part it does
    /// not know.
    ///
    /// `zone` is the zone of the event's `DTSTART`, UTC when it names none:
    /// a UTC `UNTIL` (`20270630T035959Z`) is read on its day there, the
    /// series' last local day, while a floating or date-only `UNTIL` keeps its
    /// own date. The range start is `DTSTART`'s, which an `RRULE` does not
    /// carry.
    pub fn from_rrule(rrule: &str, zone: Tz) -> Option<Self> {
        let mut rule = Recurrence {
            frequency: Frequency::Daily,
            interval: 1,
            weekdays: Vec::new(),
            day_of_month: None,
            week_index: None,
            month: None,
            first_day_of_week: None,
            start: None,
            until: None,
            count: None,
        };
        let mut frequency = None;
        let mut ordinal: Option<i8> = None;
        let mut set_position: Option<i8> = None;
        let mut first_day = None;
        for part in rrule.trim().split(';').filter(|p| !p.trim().is_empty()) {
            let (key, value) = part.split_once('=')?;
            let value = value.trim();
            match key.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    frequency = Some(match value.to_ascii_uppercase().as_str() {
                        "DAILY" => Frequency::Daily,
                        "WEEKLY" => Frequency::Weekly,
                        "MONTHLY" => Frequency::Monthly,
                        "YEARLY" => Frequency::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" => rule.interval = value.parse().ok().filter(|i| *i > 0)?,
                "COUNT" => rule.count = Some(value.parse().ok().filter(|c| *c > 0)?),
                "UNTIL" => rule.until = Some(until_date(value, zone)?),
                "BYDAY" => {
                    for day in value.split(',') {
                        let day = day.trim().to_ascii_uppercase();
                        let split = day.len().checked_sub(2)?;
                        let (position, code) = day.split_at(split);
                        rule.weekdays.push(weekday_of_code(code)?);
                        let position = match position {
                            "" => None,
                            raw => Some(raw.trim_start_matches('+').parse::<i8>().ok()?),
                        };
                        if rule.weekdays.len() > 1 && position != ordinal {
                            return None;
                        }
                        ordinal = position;
                    }
                }
                "BYSETPOS" => set_position = Some(value.parse().ok()?),
                "BYMONTHDAY" => {
                    rule.day_of_month = Some(value.parse().ok().filter(|d| (1..=31).contains(d))?)
                }
                "BYMONTH" => {
                    rule.month = Some(value.parse().ok().filter(|m| (1..=12).contains(m))?)
                }
                "WKST" => first_day = Some(weekday_of_code(&value.to_ascii_uppercase())?),
                _ => return None,
            }
        }
        rule.frequency = frequency?;
        if rule.until.is_some() && rule.count.is_some() {
            return None;
        }
        let position = match (ordinal, set_position) {
            (Some(_), Some(_)) => return None,
            (position, None) | (None, position) => position,
        };
        let relative = match position {
            None => None,
            Some(position) => Some(
                [
                    WeekIndex::First,
                    WeekIndex::Second,
                    WeekIndex::Third,
                    WeekIndex::Fourth,
                    WeekIndex::Last,
                ]
                .into_iter()
                .find(|index| index.position() == position)?,
            ),
        };
        match rule.frequency {
            Frequency::Daily => {
                let plain = rule.weekdays.is_empty()
                    && relative.is_none()
                    && rule.day_of_month.is_none()
                    && rule.month.is_none();
                plain.then_some(())?;
            }
            Frequency::Weekly => {
                let plain =
                    relative.is_none() && rule.day_of_month.is_none() && rule.month.is_none();
                plain.then_some(())?;
                rule.first_day_of_week = first_day;
            }
            Frequency::Monthly | Frequency::Yearly => {
                if rule.frequency == Frequency::Monthly && rule.month.is_some() {
                    return None;
                }
                match relative {
                    // The first to fourth or the last of some weekdays.
                    Some(index) if !rule.weekdays.is_empty() && rule.day_of_month.is_none() => {
                        rule.week_index = Some(index);
                    }
                    Some(_) => return None,
                    // Every one of some weekdays in the month: no Outlook
                    // pattern.
                    None if !rule.weekdays.is_empty() => return None,
                    None => {}
                }
            }
        }
        Some(rule)
    }

    /// The rule in words, as the event cards print it: `Weekly on
    /// Wednesday`, `Every 2 months on the last Thursday (6 times)`, `Yearly
    /// on 31 October until 2030-10-31`.
    pub fn describe(&self) -> String {
        let (unit, units) = match self.frequency {
            Frequency::Daily => ("Daily", "days"),
            Frequency::Weekly => ("Weekly", "weeks"),
            Frequency::Monthly => ("Monthly", "months"),
            Frequency::Yearly => ("Yearly", "years"),
        };
        let mut text = if self.interval > 1 {
            format!("Every {} {units}", self.interval)
        } else {
            unit.to_string()
        };
        let days = self
            .weekdays
            .iter()
            .map(|d| d.name())
            .collect::<Vec<_>>()
            .join(", ");
        let month = self.month.and_then(month_name);
        match (self.frequency, self.week_index) {
            (Frequency::Daily, _) => {}
            (Frequency::Weekly, _) => {
                if !days.is_empty() {
                    text.push_str(&format!(" on {days}"));
                }
            }
            (Frequency::Monthly | Frequency::Yearly, Some(index)) if !days.is_empty() => {
                text.push_str(&format!(" on the {} {days}", index.word()));
                if let (Frequency::Yearly, Some(month)) = (self.frequency, month) {
                    text.push_str(&format!(" of {month}"));
                }
            }
            (Frequency::Monthly, _) => {
                if let Some(day) = self.day_of_month {
                    text.push_str(&format!(" on day {day}"));
                }
            }
            (Frequency::Yearly, _) => match (self.day_of_month, month) {
                (Some(day), Some(month)) => text.push_str(&format!(" on {day} {month}")),
                (None, Some(month)) => text.push_str(&format!(" in {month}")),
                _ => {}
            },
        }
        if let Some(until) = self.until {
            text.push_str(&format!(" until {until}"));
        } else if let Some(count) = self.count {
            text.push_str(&format!(" ({count} times)"));
        }
        text
    }
}

/// One event, or one occurrence of a series, from any backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The configured account name.
    pub account: String,
    /// The server id of the calendar holding it.
    pub calendar: CalendarId,
    /// The server id.
    pub id: EventId,
    /// The iCalendar `UID`, shared by every occurrence of a series and by the
    /// invitation mails about it.
    pub uid: String,
    /// The iCalendar `RECURRENCE-ID` of an occurrence: its original start,
    /// RFC 3339 in UTC, or a bare date (`2026-07-20`) for an all-day series.
    /// `None` for a single event and for a series master. [`EventKey`] reads
    /// the iMIP parser's spellings of the same values as these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<String>,
    /// The server id of the series master, on an occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_master_id: Option<EventId>,
    /// What the event is to its series. An event cached by a build that did
    /// not record it reads as single until the next baseline.
    #[serde(default)]
    pub kind: EventKind,
    pub summary: String,
    pub time: EventTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The description as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<Person>,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    pub own_response: OwnResponse,
    /// The series rule in words ([`Recurrence::describe`]), on a series
    /// master and on each occurrence of it the backend knows the rule of; on
    /// an invitation mail's event, the summary of the mail's `RRULE`. The
    /// rule itself stays in the series cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// Minutes before the start a reminder fires; `None` when off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder: Option<u32>,
    /// The join link of a Teams or other online meeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_meeting_url: Option<String>,
    /// Whether the event is an online meeting (Graph `isOnlineMeeting`),
    /// which a server can say before its join link exists.
    #[serde(default)]
    pub is_online_meeting: bool,
    /// The service of the online meeting, when the server names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_meeting_provider: Option<OnlineMeetingProvider>,
    pub sensitivity: Sensitivity,
    pub show_as: ShowAs,
    #[serde(default)]
    pub cancelled: bool,
    /// Whether the user organizes it, which decides between the organizer's
    /// operations (update, cancel) and the attendee's (respond).
    #[serde(default)]
    pub is_organizer: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<DateTime<Utc>>,
    /// The event in the server's own web client (Outlook on the web).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_link: Option<String>,
    /// Whether the organizer lets attendees propose a new time; the servers'
    /// default is yes.
    #[serde(default = "default_true")]
    pub allow_new_time_proposals: bool,
    /// Whether the organizer asks for an answer; the servers' default is yes.
    #[serde(default = "default_true")]
    pub response_requested: bool,
    /// The client's id for the create that made the event (Graph
    /// `transactionId`), which a server returns only for an event an app
    /// created with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
}

impl Event {
    /// The key events are deduplicated on across sources: the server copy of
    /// an invitation and the iMIP mail about it share it.
    pub fn dedup_key(&self) -> EventKey {
        EventKey::new(&self.uid, self.recurrence_id.as_deref())
    }

    pub fn is_all_day(&self) -> bool {
        self.time.is_all_day()
    }

    /// See [`EventTime::occupancy`].
    pub fn occupancy(&self) -> Occupancy {
        self.time.occupancy()
    }

    /// See [`EventTime::utc_interval`].
    pub fn utc_interval(&self, viewer: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
        self.time.utc_interval(viewer)
    }

    /// See [`EventTime::overlaps`].
    pub fn overlaps(&self, window: &CalendarWindow, viewer: Tz) -> bool {
        self.time.overlaps(window, viewer)
    }

    /// Take the details of an occurrence from its series `master`: the
    /// `UID` and everything that describes the meeting rather than this
    /// occurrence's place in time, the series rule in words included. The
    /// occurrence keeps its own ids, kind, time, `RECURRENCE-ID`, change
    /// stamp and web link.
    ///
    /// What an occurrence Graph's delta trims to its times shows (PERSO-200):
    /// the Graph adapter and the store's re-derivation, whenever its master
    /// changes, both go through here, so they agree on what it inherits.
    pub fn inherit_from(&mut self, master: &Event) {
        self.uid = master.uid.clone();
        self.summary = master.summary.clone();
        self.location = master.location.clone();
        self.body = master.body.clone();
        self.organizer = master.organizer.clone();
        self.attendees = master.attendees.clone();
        self.own_response = master.own_response;
        self.reminder = master.reminder;
        self.recurrence = master.recurrence.clone();
        self.online_meeting_url = master.online_meeting_url.clone();
        self.is_online_meeting = master.is_online_meeting;
        self.online_meeting_provider = master.online_meeting_provider;
        self.sensitivity = master.sensitivity;
        self.show_as = master.show_as;
        self.cancelled = master.cancelled;
        self.is_organizer = master.is_organizer;
        self.allow_new_time_proposals = master.allow_new_time_proposals;
        self.response_requested = master.response_requested;
        self.transaction_id = master.transaction_id.clone();
    }
}

/// `UID` plus `RECURRENCE-ID`, the identity "Events are deduplicated across
/// sources on" (`docs/plans/calendar.md`, "Invitations on an account with a
/// backend").
///
/// The `RECURRENCE-ID` is normalised to one canonical form per kind of value:
///
/// - a time with an offset becomes its UTC instant, so
///   `2026-07-20T14:00:00+02:00` from an iMIP mail and `2026-07-20T12:00:00Z`
///   from a server name the same occurrence;
/// - a date stays the bare date, and so does an offset-less midnight
///   (`2026-07-20T00:00:00`), which is how [`crate::calendar::parse_ics`]
///   spells a `VALUE=DATE` `RECURRENCE-ID`: an all-day occurrence from a mail
///   and from a server (which [`Event::recurrence_id`] names by its date)
///   meet on `2026-07-20`;
/// - any other floating time is kept as written.
///
/// A midnight with an offset stays an instant: without knowing whether the
/// series is all-day, `2026-07-20T00:00:00+02:00` is as likely a timed
/// occurrence at local midnight, which a server names in UTC.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventKey {
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<String>,
}

impl EventKey {
    pub fn new(uid: &str, recurrence_id: Option<&str>) -> Self {
        EventKey {
            uid: uid.trim().to_string(),
            recurrence_id: recurrence_id.map(normalise_recurrence_id),
        }
    }
}

fn normalise_recurrence_id(raw: &str) -> String {
    let raw = raw.trim();
    if let Ok(instant) = DateTime::parse_from_rfc3339(raw) {
        return instant
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true);
    }
    if let Ok(floating) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        if floating.time() == NaiveTime::MIN {
            return floating.date().to_string();
        }
    }
    raw.to_string()
}

/// The rolling window a calendar cache holds, half-open in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// The cache window for `now`: [`WINDOW_MONTHS_BACK`] before today and
/// [`WINDOW_MONTHS_AHEAD`] after it, both at UTC midnight.
///
/// Day-aligned so every poll of one day names the same window, which a Graph
/// delta token is bound to; the daily re-baseline moves it.
pub fn window_for(now: DateTime<Utc>) -> CalendarWindow {
    let today = now.date_naive();
    let start = today
        .checked_sub_months(Months::new(WINDOW_MONTHS_BACK))
        .unwrap_or(NaiveDate::MIN);
    let end = today
        .checked_add_months(Months::new(WINDOW_MONTHS_AHEAD))
        .unwrap_or(NaiveDate::MAX);
    CalendarWindow {
        start: start.and_time(NaiveTime::MIN).and_utc(),
        end: end.and_time(NaiveTime::MIN).and_utc(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn event(time: EventTime) -> Event {
        Event {
            account: "evoqs".into(),
            calendar: "cal-1".into(),
            id: "ev-1".into(),
            uid: "uid-1".into(),
            recurrence_id: None,
            series_master_id: None,
            kind: EventKind::Single,
            summary: "Standup".into(),
            time,
            location: None,
            body: None,
            organizer: Some(Person {
                name: Some("Ada".into()),
                email: "ada@example.com".into(),
            }),
            attendees: vec![Attendee {
                name: None,
                email: "sylvain@example.com".into(),
                role: AttendeeRole::Required,
                status: ResponseStatus::NeedsAction,
                proposed_time: None,
            }],
            own_response: OwnResponse::NeedsAction,
            recurrence: None,
            reminder: Some(15),
            online_meeting_url: None,
            is_online_meeting: false,
            online_meeting_provider: None,
            sensitivity: Sensitivity::Normal,
            show_as: ShowAs::Busy,
            cancelled: false,
            is_organizer: false,
            last_modified: Some(utc(2026, 10, 1, 8, 0)),
            web_link: None,
            allow_new_time_proposals: true,
            response_requested: true,
            transaction_id: None,
        }
    }

    fn rule(frequency: Frequency) -> Recurrence {
        Recurrence {
            frequency,
            interval: 1,
            weekdays: Vec::new(),
            day_of_month: None,
            week_index: None,
            month: None,
            first_day_of_week: None,
            start: None,
            until: None,
            count: None,
        }
    }

    /// Each pattern an `RRULE` can state in the model's terms reads into the
    /// rule, an ordinal weekday or a `BYSETPOS` alike, and a rule beyond the
    /// model is none.
    #[test]
    fn an_rrule_reads_into_the_rule_of_its_pattern() {
        let read = |rrule: &str| Recurrence::from_rrule(rrule, Tz::UTC);
        assert_eq!(
            read("FREQ=WEEKLY;BYDAY=WE;WKST=SU"),
            Some(Recurrence {
                weekdays: vec![Weekday::Wednesday],
                first_day_of_week: Some(Weekday::Sunday),
                ..rule(Frequency::Weekly)
            })
        );
        assert_eq!(
            read("freq=weekly;interval=2;byday=MO,TH;until=20270629T215959Z"),
            Some(Recurrence {
                interval: 2,
                weekdays: vec![Weekday::Monday, Weekday::Thursday],
                until: Some(date(2027, 6, 29)),
                ..rule(Frequency::Weekly)
            })
        );
        assert_eq!(
            read("FREQ=DAILY;INTERVAL=3;COUNT=5"),
            Some(Recurrence {
                interval: 3,
                count: Some(5),
                ..rule(Frequency::Daily)
            })
        );
        assert_eq!(
            read("FREQ=MONTHLY;BYMONTHDAY=15"),
            Some(Recurrence {
                day_of_month: Some(15),
                ..rule(Frequency::Monthly)
            })
        );
        let last_thursday = Recurrence {
            interval: 2,
            weekdays: vec![Weekday::Thursday],
            week_index: Some(WeekIndex::Last),
            count: Some(6),
            ..rule(Frequency::Monthly)
        };
        assert_eq!(
            read("FREQ=MONTHLY;INTERVAL=2;BYDAY=TH;BYSETPOS=-1;COUNT=6").as_ref(),
            Some(&last_thursday)
        );
        assert_eq!(
            read("FREQ=MONTHLY;INTERVAL=2;BYDAY=-1TH;COUNT=6"),
            Some(last_thursday)
        );
        assert_eq!(
            read("FREQ=YEARLY;BYMONTH=10;BYMONTHDAY=31;UNTIL=20301031"),
            Some(Recurrence {
                month: Some(10),
                day_of_month: Some(31),
                until: Some(date(2030, 10, 31)),
                ..rule(Frequency::Yearly)
            })
        );
        assert_eq!(
            read("FREQ=YEARLY;BYDAY=4TH;BYMONTH=11"),
            Some(Recurrence {
                month: Some(11),
                weekdays: vec![Weekday::Thursday],
                week_index: Some(WeekIndex::Fourth),
                ..rule(Frequency::Yearly)
            })
        );
        for beyond in [
            "FREQ=HOURLY",
            "BYDAY=MO",
            "FREQ=MONTHLY;BYDAY=MO",
            "FREQ=MONTHLY;BYDAY=1MO,-1FR",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
            "FREQ=MONTHLY;BYDAY=5MO",
            "FREQ=YEARLY;BYMONTH=1,7",
            "FREQ=WEEKLY;BYDAY=2MO",
            "FREQ=DAILY;BYDAY=MO",
            "FREQ=DAILY;BYHOUR=9",
            "FREQ=DAILY;COUNT=3;UNTIL=20270101",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=DAILY;UNTIL=soon",
            "garbage",
        ] {
            assert_eq!(read(beyond), None, "{beyond}");
        }
    }

    /// A New York series ending at 23:59:59 local on 29 June has a UTC
    /// `UNTIL` on 30 June, read back on its New York day; a floating or
    /// date-only `UNTIL` keeps its own date wherever the event is.
    #[test]
    fn a_utc_until_is_read_on_its_day_in_the_events_zone() {
        let new_york = chrono_tz::America::New_York;
        let until = |rrule: &str, zone| Recurrence::from_rrule(rrule, zone).unwrap().until;
        let utc_until = "FREQ=WEEKLY;BYDAY=MO;UNTIL=20270630T035959Z";
        assert_eq!(until(utc_until, new_york), Some(date(2027, 6, 29)));
        assert_eq!(until(utc_until, Tz::UTC), Some(date(2027, 6, 30)));
        assert_eq!(
            until("FREQ=DAILY;UNTIL=20270630T235959", new_york),
            Some(date(2027, 6, 30))
        );
        assert_eq!(
            until("FREQ=DAILY;UNTIL=20270630", new_york),
            Some(date(2027, 6, 30))
        );
    }

    #[test]
    fn every_pattern_reads_in_words() {
        let weekly = Recurrence {
            weekdays: vec![Weekday::Wednesday],
            first_day_of_week: Some(Weekday::Sunday),
            start: Some(date(2024, 10, 23)),
            ..rule(Frequency::Weekly)
        };
        assert_eq!(weekly.describe(), "Weekly on Wednesday");

        let fortnightly = Recurrence {
            interval: 2,
            weekdays: vec![Weekday::Monday, Weekday::Thursday],
            until: Some(date(2027, 6, 29)),
            ..rule(Frequency::Weekly)
        };
        assert_eq!(
            fortnightly.describe(),
            "Every 2 weeks on Monday, Thursday until 2027-06-29"
        );

        let daily = Recurrence {
            interval: 3,
            count: Some(5),
            ..rule(Frequency::Daily)
        };
        assert_eq!(daily.describe(), "Every 3 days (5 times)");

        let by_date = Recurrence {
            day_of_month: Some(15),
            ..rule(Frequency::Monthly)
        };
        assert_eq!(by_date.describe(), "Monthly on day 15");

        let last_thursday = Recurrence {
            interval: 2,
            weekdays: vec![Weekday::Thursday],
            week_index: Some(WeekIndex::Last),
            count: Some(6),
            ..rule(Frequency::Monthly)
        };
        assert_eq!(
            last_thursday.describe(),
            "Every 2 months on the last Thursday (6 times)"
        );

        let birthday = Recurrence {
            month: Some(10),
            day_of_month: Some(31),
            ..rule(Frequency::Yearly)
        };
        assert_eq!(birthday.describe(), "Yearly on 31 October");

        let thanksgiving = Recurrence {
            month: Some(11),
            weekdays: vec![Weekday::Thursday],
            week_index: Some(WeekIndex::Fourth),
            ..rule(Frequency::Yearly)
        };
        assert_eq!(
            thanksgiving.describe(),
            "Yearly on the fourth Thursday of November"
        );
    }

    #[test]
    fn a_feature_spells_its_wire_word_as_serde_does() {
        for feature in [
            CalendarFeature::Write,
            CalendarFeature::OnlineMeeting,
            CalendarFeature::ProposeTime,
            CalendarFeature::FreeBusy,
            CalendarFeature::ServerReminders,
        ] {
            assert_eq!(
                serde_json::to_value(feature).unwrap(),
                serde_json::json!(feature.as_str())
            );
        }
    }

    #[test]
    fn a_rule_round_trips_through_json_in_kebab_case() {
        let rule = Recurrence {
            weekdays: vec![Weekday::Thursday],
            week_index: Some(WeekIndex::Last),
            start: Some(date(2026, 11, 26)),
            ..rule(Frequency::Monthly)
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "frequency": "monthly",
                "interval": 1,
                "weekdays": ["thursday"],
                "week_index": "last",
                "start": "2026-11-26"
            })
        );
        assert_eq!(serde_json::from_value::<Recurrence>(json).unwrap(), rule);
    }

    #[test]
    fn an_event_cached_before_the_phase_2_fields_reads_with_the_servers_defaults() {
        let ev = event(EventTime::all_day(date(2026, 11, 3), date(2026, 11, 4)).unwrap());
        let mut json = serde_json::to_value(&ev).unwrap();
        let object = json.as_object_mut().unwrap();
        for key in [
            "kind",
            "is_online_meeting",
            "allow_new_time_proposals",
            "response_requested",
        ] {
            assert!(object.remove(key).is_some(), "{key} is always written");
        }
        let read: Event = serde_json::from_value(json).unwrap();
        assert_eq!(read, ev);
    }

    #[test]
    fn an_occurrence_inherits_the_meeting_but_keeps_its_own_kind_and_slot() {
        let mut master = event(
            EventTime::timed(
                utc(2026, 10, 7, 8, 0),
                utc(2026, 10, 7, 9, 0),
                "Europe/Berlin",
            )
            .unwrap(),
        );
        master.id = "master".into();
        master.kind = EventKind::SeriesMaster;
        master.recurrence = Some("Weekly on Wednesday".into());
        master.is_online_meeting = true;
        master.online_meeting_provider = Some(OnlineMeetingProvider::TeamsForBusiness);
        master.allow_new_time_proposals = false;
        master.response_requested = false;
        master.transaction_id = Some("tx-1".into());
        master.attendees[0].proposed_time = Some(TimeSlot {
            start: utc(2026, 11, 4, 9, 0),
            end: utc(2026, 11, 4, 10, 0),
        });

        let time = EventTime::timed(utc(2026, 11, 4, 8, 0), utc(2026, 11, 4, 9, 0), "UTC").unwrap();
        let mut occurrence = event(time.clone());
        occurrence.id = "occ".into();
        occurrence.kind = EventKind::Occurrence;
        occurrence.summary = String::new();
        occurrence.inherit_from(&master);

        assert_eq!(occurrence.id, "occ");
        assert_eq!(occurrence.kind, EventKind::Occurrence);
        assert_eq!(occurrence.time, time);
        assert_eq!(occurrence.summary, "Standup");
        assert_eq!(
            occurrence.recurrence.as_deref(),
            Some("Weekly on Wednesday")
        );
        assert!(occurrence.is_online_meeting);
        assert_eq!(
            occurrence.online_meeting_provider,
            Some(OnlineMeetingProvider::TeamsForBusiness)
        );
        assert!(!occurrence.allow_new_time_proposals && !occurrence.response_requested);
        assert_eq!(occurrence.transaction_id.as_deref(), Some("tx-1"));
        assert_eq!(occurrence.attendees, master.attendees);
    }

    #[test]
    fn a_one_day_midnight_pair_becomes_one_day_with_an_exclusive_end_and_back() {
        let start = date(2026, 11, 3).and_hms_opt(0, 0, 0).unwrap();
        let end = date(2026, 11, 4).and_hms_opt(0, 0, 0).unwrap();
        let time = EventTime::all_day_from_midnights(start, end).unwrap();
        assert_eq!(
            time,
            EventTime::AllDay {
                start: date(2026, 11, 3),
                end: date(2026, 11, 4)
            }
        );
        assert_eq!(time.last_day(), Some(date(2026, 11, 3)));
        assert_eq!(time.all_day_midnights(), Some((start, end)));
    }

    #[test]
    fn a_multi_day_midnight_pair_round_trips_across_a_month_end() {
        let start = date(2026, 10, 30).and_hms_opt(0, 0, 0).unwrap();
        let end = date(2026, 11, 2).and_hms_opt(0, 0, 0).unwrap();
        let time = EventTime::all_day_from_midnights(start, end).unwrap();
        assert_eq!(time.last_day(), Some(date(2026, 11, 1)));
        assert_eq!(
            time.occupancy(),
            Occupancy::Dates {
                start: date(2026, 10, 30),
                end: date(2026, 11, 2)
            }
        );
        assert_eq!(time.all_day_midnights(), Some((start, end)));
    }

    #[test]
    fn an_all_day_pair_off_midnight_or_empty_is_refused() {
        let noon = date(2026, 11, 3).and_hms_opt(12, 0, 0).unwrap();
        let midnight = date(2026, 11, 4).and_hms_opt(0, 0, 0).unwrap();
        assert_eq!(
            EventTime::all_day_from_midnights(noon, midnight),
            Err(EventTimeError::NotMidnight(noon))
        );
        assert_eq!(
            EventTime::all_day_from_midnights(midnight, midnight),
            Err(EventTimeError::EndBeforeStart)
        );
    }

    #[test]
    fn a_timed_event_needs_an_iana_zone_and_an_ordered_range() {
        let (a, b) = (utc(2026, 11, 3, 9, 0), utc(2026, 11, 3, 10, 0));
        assert!(EventTime::timed(a, b, "Europe/Berlin").is_ok());
        assert!(EventTime::timed(a, a, "UTC").is_ok());
        assert_eq!(
            EventTime::timed(a, b, "W. Europe Standard Time"),
            Err(EventTimeError::UnknownZone(
                "W. Europe Standard Time".into()
            ))
        );
        assert_eq!(
            EventTime::timed(b, a, "Europe/Berlin"),
            Err(EventTimeError::EndBeforeStart)
        );
    }

    #[test]
    fn deserialising_validates_the_zone_and_the_range() {
        let ok = r#"{"kind":"timed","start":"2026-11-03T09:00:00Z","end":"2026-11-03T10:00:00Z","zone":"Europe/Berlin"}"#;
        assert!(serde_json::from_str::<EventTime>(ok).is_ok());
        let windows = ok.replace("Europe/Berlin", "W. Europe Standard Time");
        assert!(serde_json::from_str::<EventTime>(&windows).is_err());
        let empty = r#"{"kind":"all-day","start":"2026-11-03","end":"2026-11-03"}"#;
        assert!(serde_json::from_str::<EventTime>(empty).is_err());
    }

    #[test]
    fn an_event_round_trips_through_json() {
        let ev = event(EventTime::all_day(date(2026, 11, 3), date(2026, 11, 4)).unwrap());
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""kind":"all-day""#), "{json}");
        assert!(json.contains(r#""own_response":"needs-action""#), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), ev);

        let timed = event(
            EventTime::timed(
                utc(2026, 11, 3, 8, 0),
                utc(2026, 11, 3, 9, 0),
                "Europe/Berlin",
            )
            .unwrap(),
        );
        let json = serde_json::to_string(&timed).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), timed);
    }

    #[test]
    fn a_calendar_without_visibility_reads_as_visible() {
        let cal: Calendar =
            serde_json::from_str(r#"{"account":"evoqs","id":"c","name":"Calendar"}"#).unwrap();
        assert!(cal.visible);
        assert!(!cal.is_default);
        assert_eq!(cal.color, None);
    }

    #[test]
    fn an_all_day_event_occupies_the_viewers_midnights() {
        let ev = event(EventTime::all_day(date(2026, 11, 3), date(2026, 11, 4)).unwrap());
        // Berlin is UTC+1 in November.
        assert_eq!(
            ev.utc_interval(chrono_tz::Europe::Berlin),
            (utc(2026, 11, 2, 23, 0), utc(2026, 11, 3, 23, 0))
        );
        assert_eq!(
            ev.utc_interval(chrono_tz::UTC),
            (utc(2026, 11, 3, 0, 0), utc(2026, 11, 4, 0, 0))
        );
    }

    #[test]
    fn a_timed_event_occupies_its_instants_and_overlap_is_half_open() {
        let ev = event(
            EventTime::timed(
                utc(2026, 11, 3, 8, 0),
                utc(2026, 11, 3, 9, 0),
                "Europe/Berlin",
            )
            .unwrap(),
        );
        assert_eq!(
            ev.occupancy(),
            Occupancy::Instants {
                start: utc(2026, 11, 3, 8, 0),
                end: utc(2026, 11, 3, 9, 0)
            }
        );
        let berlin = chrono_tz::Europe::Berlin;
        let window = |s, e| CalendarWindow { start: s, end: e };
        let before = window(utc(2026, 11, 3, 7, 0), utc(2026, 11, 3, 8, 0));
        let after = window(utc(2026, 11, 3, 9, 0), utc(2026, 11, 3, 10, 0));
        let inside = window(utc(2026, 11, 3, 8, 30), utc(2026, 11, 3, 8, 45));
        assert!(!ev.overlaps(&before, berlin));
        assert!(!ev.overlaps(&after, berlin));
        assert!(ev.overlaps(&inside, berlin));

        let instant =
            event(EventTime::timed(utc(2026, 11, 3, 9, 0), utc(2026, 11, 3, 9, 0), "UTC").unwrap());
        assert!(instant.overlaps(&after, berlin));
        assert!(!instant.overlaps(&before, berlin));
    }

    #[test]
    fn the_dedup_key_matches_an_occurrence_named_with_an_offset_or_in_utc() {
        let mut server = event(EventTime::all_day(date(2026, 7, 20), date(2026, 7, 21)).unwrap());
        server.recurrence_id = Some("2026-07-20T12:00:00Z".into());
        let imip = EventKey::new(" uid-1 ", Some("2026-07-20T14:00:00+02:00"));
        assert_eq!(server.dedup_key(), imip);
        assert_ne!(server.dedup_key(), EventKey::new("uid-1", None));
        assert_eq!(
            EventKey::new("uid-1", Some("2026-07-20"))
                .recurrence_id
                .as_deref(),
            Some("2026-07-20")
        );
        // An offset-less midnight is the parser's spelling of a date; a
        // floating time of day and a zoned midnight are not.
        let key = |rid: &str| EventKey::new("uid-1", Some(rid)).recurrence_id.unwrap();
        assert_eq!(key("2026-07-20T00:00:00"), "2026-07-20");
        assert_eq!(key("2026-07-20T09:30:00"), "2026-07-20T09:30:00");
        assert_eq!(key("2026-07-20T00:00:00+02:00"), "2026-07-19T22:00:00Z");
    }

    /// An all-day occurrence invitation, read by the real iMIP parser, keys
    /// to the same occurrence the model names by its date.
    #[test]
    fn an_all_day_occurrence_from_a_mail_matches_the_server_copy() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nMETHOD:REQUEST\r\n\
                   BEGIN:VEVENT\r\nUID:uid-1\r\nSEQUENCE:1\r\nDTSTAMP:20260701T090000Z\r\n\
                   RECURRENCE-ID;VALUE=DATE:20260720\r\n\
                   DTSTART;VALUE=DATE:20260721\r\nDTEND;VALUE=DATE:20260722\r\n\
                   SUMMARY:Offsite (moved)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let parsed = crate::calendar::parse_ics(ics.as_bytes()).expect("the invitation parses");
        let from_mail = EventKey::new(
            parsed.uid.as_deref().unwrap(),
            parsed.recurrence_id.as_deref(),
        );

        let mut server = event(EventTime::all_day(date(2026, 7, 21), date(2026, 7, 22)).unwrap());
        server.recurrence_id = Some("2026-07-20".into());
        assert_eq!(server.dedup_key(), from_mail);
    }

    #[test]
    fn the_window_is_a_month_back_and_twelve_ahead_at_utc_midnight() {
        let window = window_for(utc(2026, 10, 28, 15, 42));
        assert_eq!(window.start, utc(2026, 9, 28, 0, 0));
        assert_eq!(window.end, utc(2027, 10, 28, 0, 0));
        // A month-end clamps rather than overflowing into the next month.
        let window = window_for(utc(2026, 3, 31, 0, 0));
        assert_eq!(window.start, utc(2026, 2, 28, 0, 0));
        assert_eq!(window.end, utc(2027, 3, 31, 0, 0));
    }
}
