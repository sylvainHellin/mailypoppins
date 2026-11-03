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

/// One attendee with role and status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
    pub role: AttendeeRole,
    pub status: ResponseStatus,
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
    /// The `RRULE` text of a series master; an occurrence carries `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// Minutes before the start a reminder fires; `None` when off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder: Option<u32>,
    /// The join link of a Teams or other online meeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_meeting_url: Option<String>,
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
    /// occurrence's place in time. The occurrence keeps its own ids, time,
    /// `RECURRENCE-ID`, change stamp and web link.
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
        self.online_meeting_url = master.online_meeting_url.clone();
        self.sensitivity = master.sensitivity;
        self.show_as = master.show_as;
        self.cancelled = master.cancelled;
        self.is_organizer = master.is_organizer;
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
            }],
            own_response: OwnResponse::NeedsAction,
            recurrence: None,
            reminder: Some(15),
            online_meeting_url: None,
            sensitivity: Sensitivity::Normal,
            show_as: ShowAs::Busy,
            cancelled: false,
            is_organizer: false,
            last_modified: Some(utc(2026, 10, 1, 8, 0)),
            web_link: None,
        }
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
