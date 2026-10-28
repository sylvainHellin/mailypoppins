//! The Microsoft Graph calendar backend (PERSO-200, `docs/plans/calendar.md`,
//! "Backend notes", Graph).
//!
//! Reads go through `calendarView/delta` over the cache window, one calendar
//! at a time:
//!
//! - `GET /me/calendars/{id}/calendarView/delta` first (`calendar-delta`).
//!   v1.0 has the route, but only the default-calendar form is documented.
//! - When that answers 400 or 404, the default calendar falls back to the
//!   documented `GET /me/calendarView/delta` (`default-delta`), and any other
//!   calendar to a plain `GET /me/calendars/{id}/calendarView` read in full at
//!   every sync (`calendar-view`), whose token is a marker that skips the
//!   failing probe until the next baseline asks again.
//!
//! The token handed to the engine is a [`GraphToken`]: the path plus the
//! `@odata.deltaLink`, followed verbatim by the next sync. A delta link is
//! bound to the window its first request named, which the engine guarantees
//! by passing no token for a moved window. HTTP 410, or an error code
//! `syncStateNotFound` / `resyncRequired`, drops the link for a fresh
//! baseline (`SyncResult::full_resync`).
//!
//! Graph gives each occurrence of a series an `iCalUId` of its own, so an
//! occurrence's `UID` is read from its series master
//! (`GET /me/events/{seriesMasterId}`), once per series: the store caches it
//! ([`SeriesInfo`], [`CalendarBackend::remember_series`]) and a baseline
//! reads it again to refresh the rule. An occurrence carries no
//! [`Event::recurrence`], as the model asks; the series rule stays in the
//! series cache.
//!
//! Every request sends `Prefer: IdType="ImmutableId"`. No
//! `Prefer: outlook.timezone` is sent, so times come back in UTC and the
//! event's own zone is read from `originalStartTimeZone`, a Windows name
//! mapped through [`crate::windows_zones`]; a zone the table does not know
//! (`tzone://Microsoft/Custom`) is labelled UTC, with the instants exact.
//!
//! The HTTP layer is the [`GraphFetch`] seam: [`GraphHttp`] over the mail
//! client's [`GraphClient`] in production, a scripted fake in the tests, so
//! the paging, the fallbacks and the 410 handling run without a tenant.

use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::Mutex;

use anyhow::{anyhow, Context};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use super::{
    BackendResult, CalendarBackend, CalendarBackendError, Response, SeriesInfo, SyncResult,
};
use crate::calendar_model::{
    Attendee, AttendeeRole, Calendar, CalendarWindow, Event, EventId, EventTime, OwnResponse,
    Person, ResponseStatus, Sensitivity, ShowAs,
};
use crate::config::{AccountConfig, GraphConfig};
use crate::graph::{encode_id, GraphClient, GRAPH_BASE, PREFER_IMMUTABLE_ID};

/// How many events a delta or view page may carry.
const PAGE_SIZE: usize = 100;

/// The most pages one sync follows before it gives up on the chain: 50 000
/// events at [`PAGE_SIZE`], far beyond a year of anyone's calendar.
const MAX_PAGES: usize = 500;

/// The scope a 403 on a calendar call is reported missing.
const CALENDAR_SCOPE_NAME: &str = "Calendars.ReadWrite";

// ---------------------------------------------------------------------------
// The HTTP seam
// ---------------------------------------------------------------------------

/// One HTTP answer, reduced to what the adapter reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAnswer {
    pub status: u16,
    pub body: String,
    /// The `Location` header, which a 410 on a delta link carries.
    pub location: Option<String>,
}

/// The two kinds of request the adapter makes. Every one carries
/// `Prefer: IdType="ImmutableId"`; `prefer` adds to it.
pub trait GraphFetch: Send + Sync {
    fn get(
        &self,
        url: &str,
        prefer: &[String],
    ) -> impl Future<Output = anyhow::Result<HttpAnswer>> + Send;

    fn post(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> impl Future<Output = anyhow::Result<HttpAnswer>> + Send;
}

/// [`GraphFetch`] over the mail client: its connection pool, its timeouts
/// and its token, refreshed when [`GraphCalendarBackend::connect`] builds it.
/// An access token lasts about an hour, so the engine connects once per sync
/// pass, as the mail pass does, rather than keeping one for the daemon's life.
pub struct GraphHttp {
    client: GraphClient,
}

/// The `Prefer` header value: the immutable-id preference plus `extra`, one
/// header with comma-separated preferences (RFC 7240).
fn prefer_header(extra: &[String]) -> String {
    std::iter::once(PREFER_IMMUTABLE_ID.to_string())
        .chain(extra.iter().cloned())
        .collect::<Vec<_>>()
        .join(", ")
}

async fn read_answer(resp: reqwest::Response) -> anyhow::Result<HttpAnswer> {
    let status = resp.status().as_u16();
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = resp.text().await.context("reading a Graph answer")?;
    Ok(HttpAnswer {
        status,
        body,
        location,
    })
}

impl GraphFetch for GraphHttp {
    async fn get(&self, url: &str, prefer: &[String]) -> anyhow::Result<HttpAnswer> {
        let resp = self
            .client
            .request(reqwest::Method::GET, url)
            .header("Prefer", prefer_header(prefer))
            .send()
            .await
            .with_context(|| format!("GET {}", strip_query(url)))?;
        read_answer(resp).await
    }

    async fn post(&self, url: &str, body: serde_json::Value) -> anyhow::Result<HttpAnswer> {
        let resp = self
            .client
            .request(reqwest::Method::POST, url)
            .header("Prefer", prefer_header(&[]))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {}", strip_query(url)))?;
        read_answer(resp).await
    }
}

/// A URL without its query, for a log line: a delta link's query is a token.
fn strip_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(path, _)| path)
}

// ---------------------------------------------------------------------------
// The token
// ---------------------------------------------------------------------------

/// Which route a calendar syncs through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncPath {
    /// `GET /me/calendars/{id}/calendarView/delta`.
    CalendarDelta,
    /// `GET /me/calendarView/delta`, the default calendar's fallback.
    DefaultDelta,
    /// `GET /me/calendars/{id}/calendarView` in full at every sync, the
    /// fallback of any other calendar.
    CalendarView,
}

impl SyncPath {
    fn as_str(self) -> &'static str {
        match self {
            SyncPath::CalendarDelta => "calendar-delta",
            SyncPath::DefaultDelta => "default-delta",
            SyncPath::CalendarView => "calendar-view",
        }
    }
}

/// The token this backend hands the engine, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphToken {
    pub path: SyncPath,
    /// The `@odata.deltaLink` to follow; `None` on [`SyncPath::CalendarView`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl GraphToken {
    fn encode(&self) -> String {
        serde_json::to_string(self).expect("a GraphToken always serialises")
    }

    /// A token this build cannot read is no token: the sync is a baseline.
    fn decode(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

mod wire {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub struct Page<T> {
        #[serde(default = "Vec::new")]
        pub value: Vec<T>,
        #[serde(rename = "@odata.nextLink")]
        pub next_link: Option<String>,
        #[serde(rename = "@odata.deltaLink")]
        pub delta_link: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Calendar {
        pub id: String,
        #[serde(default)]
        pub name: String,
        pub color: Option<String>,
        pub hex_color: Option<String>,
        #[serde(default)]
        pub is_default_calendar: bool,
        #[serde(default)]
        pub can_edit: bool,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DateTimeZone {
        pub date_time: String,
        pub time_zone: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct EmailAddress {
        pub name: Option<String>,
        pub address: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Recipient {
        pub email_address: Option<EmailAddress>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct Status {
        pub response: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Attendee {
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub status: Option<Status>,
        pub email_address: Option<EmailAddress>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Body {
        pub content_type: Option<String>,
        pub content: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Location {
        pub display_name: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OnlineMeeting {
        pub join_url: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RecurrencePattern {
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub interval: Option<u32>,
        pub month: Option<u32>,
        pub day_of_month: Option<u32>,
        #[serde(default)]
        pub days_of_week: Vec<String>,
        pub first_day_of_week: Option<String>,
        pub index: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RecurrenceRange {
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub end_date: Option<String>,
        pub number_of_occurrences: Option<u32>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct Recurrence {
        pub pattern: Option<RecurrencePattern>,
        pub range: Option<RecurrenceRange>,
    }

    /// An event as `calendarView` and its delta return it. Every field but
    /// the id is optional, so an event Graph trims is converted on what it
    /// has rather than failing the page.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Event {
        pub id: String,
        #[serde(rename = "iCalUId")]
        pub ical_uid: Option<String>,
        pub subject: Option<String>,
        pub body_preview: Option<String>,
        pub body: Option<Body>,
        pub start: Option<DateTimeZone>,
        pub end: Option<DateTimeZone>,
        #[serde(default)]
        pub is_all_day: bool,
        pub location: Option<Location>,
        pub organizer: Option<Recipient>,
        #[serde(default)]
        pub attendees: Vec<Attendee>,
        pub response_status: Option<Status>,
        pub recurrence: Option<Recurrence>,
        pub series_master_id: Option<String>,
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub original_start: Option<String>,
        pub original_start_time_zone: Option<String>,
        pub reminder_minutes_before_start: Option<i64>,
        #[serde(default)]
        pub is_reminder_on: bool,
        pub online_meeting: Option<OnlineMeeting>,
        pub online_meeting_url: Option<String>,
        pub sensitivity: Option<String>,
        pub show_as: Option<String>,
        #[serde(default)]
        pub is_cancelled: bool,
        #[serde(default)]
        pub is_organizer: bool,
        pub web_link: Option<String>,
        pub last_modified_date_time: Option<String>,
    }

    /// A series master as `GET /me/events/{id}?$select=...` returns it.
    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Master {
        pub id: Option<String>,
        #[serde(rename = "iCalUId")]
        pub ical_uid: Option<String>,
        pub recurrence: Option<Recurrence>,
    }
}

/// One entry of a delta or view page.
#[derive(Debug, Clone)]
enum Item {
    Upsert(Box<wire::Event>),
    Removed(String),
}

/// Read one page entry: `@removed` is a removal, anything else an event. An
/// entry that does not parse is skipped with a log line rather than failing
/// the sync it came in.
fn parse_item(value: serde_json::Value) -> Option<Item> {
    if value.get("@removed").is_some() {
        return value
            .get("id")
            .and_then(|id| id.as_str())
            .map(|id| Item::Removed(id.to_string()));
    }
    match serde_json::from_value::<wire::Event>(value) {
        Ok(event) => Some(Item::Upsert(Box::new(event))),
        Err(err) => {
            warn!("[calendar] skipping a Graph event that does not parse: {err}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/// Hex values for Graph's named calendar colours, used when `hexColor` is
/// empty. Approximations of Outlook's palette; `auto` has no colour of its
/// own and the client picks one.
const NAMED_COLORS: &[(&str, &str)] = &[
    ("lightBlue", "#a6d1f5"),
    ("lightBrown", "#d9b38c"),
    ("lightGray", "#c0c0c0"),
    ("lightGreen", "#87d28e"),
    ("lightOrange", "#fcab73"),
    ("lightPink", "#f4a3c6"),
    ("lightRed", "#f69a9a"),
    ("lightTeal", "#7fd6cf"),
    ("lightYellow", "#f4d07a"),
];

fn calendar_color(hex: Option<&str>, named: Option<&str>) -> Option<String> {
    let hex = hex.map(str::trim).unwrap_or_default();
    let valid_hex =
        hex.len() == 7 && hex.starts_with('#') && hex[1..].chars().all(|c| c.is_ascii_hexdigit());
    if valid_hex {
        return Some(hex.to_ascii_lowercase());
    }
    let named = named?;
    NAMED_COLORS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(named))
        .map(|(_, hex)| hex.to_string())
}

fn calendar_from_graph(account: &str, cal: wire::Calendar) -> Calendar {
    Calendar {
        account: account.to_string(),
        color: calendar_color(cal.hex_color.as_deref(), cal.color.as_deref()),
        id: cal.id,
        name: cal.name,
        is_default: cal.is_default_calendar,
        can_edit: cal.can_edit,
        visible: true,
    }
}

/// A Graph `dateTime` (`2026-11-03T09:00:00.0000000`), which carries no
/// offset of its own.
fn parse_graph_local(raw: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%dT%H:%M:%S%.f").ok()
}

/// The IANA zone a Graph zone name means, if any.
fn zone_of(name: Option<&str>) -> Option<Tz> {
    let iana = crate::windows_zones::iana_or_windows(name?)?;
    Tz::from_str(&iana).ok()
}

/// A Graph `{dateTime, timeZone}` as an instant. The adapter asks for no
/// zone, so Graph answers in UTC; any other zone is honoured all the same.
fn instant_of(value: &wire::DateTimeZone) -> Result<DateTime<Utc>, String> {
    let local = parse_graph_local(&value.date_time)
        .ok_or_else(|| format!("unreadable dateTime {:?}", value.date_time))?;
    let zone_name = value.time_zone.as_deref().unwrap_or("UTC");
    if zone_name.eq_ignore_ascii_case("UTC") {
        return Ok(local.and_utc());
    }
    let zone = zone_of(Some(zone_name)).ok_or_else(|| format!("unknown zone {zone_name:?}"))?;
    zone.from_local_datetime(&local)
        .earliest()
        .map(|at| at.with_timezone(&Utc))
        .ok_or_else(|| format!("{local} does not exist in {zone_name}"))
}

/// An all-day event's time from its midnight pair. Graph names an all-day
/// event's midnights in its own zone; should it answer them converted to UTC
/// instead, they are read back into the event's zone first.
fn all_day_time(
    start: &wire::DateTimeZone,
    end: &wire::DateTimeZone,
    own_zone: Option<Tz>,
) -> Result<EventTime, String> {
    let naive = |value: &wire::DateTimeZone| {
        parse_graph_local(&value.date_time)
            .ok_or_else(|| format!("unreadable dateTime {:?}", value.date_time))
    };
    let (s, e) = (naive(start)?, naive(end)?);
    match EventTime::all_day_from_midnights(s, e) {
        Ok(time) => Ok(time),
        Err(first) => {
            let zone = own_zone.ok_or_else(|| first.to_string())?;
            let local = |value| -> Result<NaiveDateTime, String> {
                Ok(instant_of(value)?.with_timezone(&zone).naive_local())
            };
            EventTime::all_day_from_midnights(local(start)?, local(end)?)
                .map_err(|err| err.to_string())
        }
    }
}

fn person(recipient: Option<&wire::Recipient>) -> Option<Person> {
    let address = recipient?.email_address.as_ref()?;
    let email = address.address.as_deref()?.trim();
    if email.is_empty() {
        return None;
    }
    Some(Person {
        name: address
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != email)
            .map(str::to_string),
        email: email.to_string(),
    })
}

fn attendee(raw: &wire::Attendee) -> Option<Attendee> {
    let address = raw.email_address.as_ref()?;
    let email = address.address.as_deref()?.trim();
    if email.is_empty() {
        return None;
    }
    let role = match raw.kind.as_deref() {
        Some("optional") => AttendeeRole::Optional,
        Some("resource") => AttendeeRole::Resource,
        _ => AttendeeRole::Required,
    };
    let status = match raw.status.as_ref().and_then(|s| s.response.as_deref()) {
        Some("accepted") | Some("organizer") => ResponseStatus::Accepted,
        Some("tentativelyAccepted") => ResponseStatus::Tentative,
        Some("declined") => ResponseStatus::Declined,
        _ => ResponseStatus::NeedsAction,
    };
    Some(Attendee {
        name: address
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != email)
            .map(str::to_string),
        email: email.to_string(),
        role,
        status,
    })
}

fn own_response(raw: Option<&str>) -> OwnResponse {
    match raw {
        Some("organizer") => OwnResponse::Organizer,
        Some("accepted") => OwnResponse::Accepted,
        Some("tentativelyAccepted") => OwnResponse::Tentative,
        Some("declined") => OwnResponse::Declined,
        Some("notResponded") => OwnResponse::NeedsAction,
        _ => OwnResponse::None,
    }
}

fn sensitivity(raw: Option<&str>) -> Sensitivity {
    match raw {
        Some("personal") => Sensitivity::Personal,
        Some("private") => Sensitivity::Private,
        Some("confidential") => Sensitivity::Confidential,
        _ => Sensitivity::Normal,
    }
}

fn show_as(raw: Option<&str>) -> ShowAs {
    match raw {
        Some("free") => ShowAs::Free,
        Some("tentative") => ShowAs::Tentative,
        Some("busy") => ShowAs::Busy,
        Some("oof") => ShowAs::Oof,
        Some("workingElsewhere") => ShowAs::WorkingElsewhere,
        _ => ShowAs::Unknown,
    }
}

/// The description as plain text: a delta may deliver the HTML body, which
/// goes through the mail path's HTML-to-text; `bodyPreview` stands in when
/// there is no body.
fn plain_body(event: &wire::Event) -> Option<String> {
    let text = match &event.body {
        Some(wire::Body {
            content: Some(content),
            content_type,
        }) if !content.trim().is_empty() => {
            if content_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("html"))
            {
                crate::parse::html_to_plain(content)
            } else {
                content.clone()
            }
        }
        _ => event.body_preview.clone().unwrap_or_default(),
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Whether a Graph event `type` is one occurrence of a series.
fn is_instance(kind: Option<&str>) -> bool {
    matches!(kind, Some("occurrence") | Some("exception"))
}

/// A Graph event as the neutral model has it. `series` is the event's series
/// master, for an occurrence: its `UID` is the one the series shares.
pub(crate) fn from_graph(
    account: &str,
    calendar: &str,
    event: &wire::Event,
    series: Option<&SeriesInfo>,
) -> Result<Event, String> {
    let start = event.start.as_ref().ok_or("no start")?;
    let end = event.end.as_ref().ok_or("no end")?;
    let own_zone = zone_of(event.original_start_time_zone.as_deref());
    let time = if event.is_all_day {
        all_day_time(start, end, own_zone)?
    } else {
        let zone = own_zone.map_or("UTC", |tz| tz.name());
        EventTime::timed(instant_of(start)?, instant_of(end)?, zone).map_err(|e| e.to_string())?
    };

    let instance = is_instance(event.kind.as_deref());
    let own_uid = event
        .ical_uid
        .clone()
        .filter(|uid| !uid.trim().is_empty())
        .unwrap_or_else(|| event.id.clone());
    let uid = match series {
        Some(series) if instance => series.uid.clone(),
        _ => own_uid,
    };
    let recurrence_id = if instance {
        Some(recurrence_id(event, &time, own_zone)?)
    } else {
        None
    };

    Ok(Event {
        account: account.to_string(),
        calendar: calendar.to_string(),
        id: event.id.clone(),
        uid,
        recurrence_id,
        series_master_id: event.series_master_id.clone().filter(|_| instance),
        summary: event.subject.clone().unwrap_or_default(),
        location: event
            .location
            .as_ref()
            .and_then(|l| l.display_name.as_deref())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string),
        body: plain_body(event),
        organizer: person(event.organizer.as_ref()),
        attendees: event.attendees.iter().filter_map(attendee).collect(),
        own_response: own_response(
            event
                .response_status
                .as_ref()
                .and_then(|s| s.response.as_deref()),
        ),
        recurrence: None,
        reminder: event
            .reminder_minutes_before_start
            .filter(|_| event.is_reminder_on)
            .and_then(|minutes| u32::try_from(minutes).ok()),
        online_meeting_url: event
            .online_meeting
            .as_ref()
            .and_then(|m| m.join_url.clone())
            .or_else(|| event.online_meeting_url.clone())
            .filter(|url| !url.trim().is_empty()),
        sensitivity: sensitivity(event.sensitivity.as_deref()),
        show_as: show_as(event.show_as.as_deref()),
        cancelled: event.is_cancelled,
        is_organizer: event.is_organizer,
        last_modified: event
            .last_modified_date_time
            .as_deref()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|at| at.with_timezone(&Utc)),
        web_link: event.web_link.clone(),
        time,
    })
}

/// An occurrence's `RECURRENCE-ID`: its original start as a UTC instant, or
/// as a date in the series' zone for an all-day series, the form an iMIP
/// mail about it names it in. An occurrence without `originalStart` falls
/// back to its own start.
fn recurrence_id(
    event: &wire::Event,
    time: &EventTime,
    own_zone: Option<Tz>,
) -> Result<String, String> {
    let original = match event.original_start.as_deref() {
        Some(raw) => DateTime::parse_from_rfc3339(raw)
            .map(|at| at.with_timezone(&Utc))
            .map_err(|err| format!("unreadable originalStart {raw:?}: {err}"))?,
        None => match time {
            EventTime::Timed { start, .. } => *start,
            EventTime::AllDay { start, .. } => return Ok(start.to_string()),
        },
    };
    Ok(match time {
        EventTime::Timed { .. } => original.to_rfc3339_opts(SecondsFormat::Secs, true),
        EventTime::AllDay { .. } => {
            let zone = own_zone.unwrap_or(chrono_tz::UTC);
            original.with_timezone(&zone).date_naive().to_string()
        }
    })
}

/// A Graph `recurrence` as `RRULE` text, or `None` for a pattern it cannot
/// express.
fn rrule_of(recurrence: &wire::Recurrence) -> Option<String> {
    let pattern = recurrence.pattern.as_ref()?;
    let day = |name: &str| -> Option<&'static str> {
        Some(match name.to_ascii_lowercase().as_str() {
            "monday" => "MO",
            "tuesday" => "TU",
            "wednesday" => "WE",
            "thursday" => "TH",
            "friday" => "FR",
            "saturday" => "SA",
            "sunday" => "SU",
            _ => return None,
        })
    };
    let days: Vec<&str> = pattern.days_of_week.iter().filter_map(|d| day(d)).collect();
    let mut parts: Vec<String> = Vec::new();
    let (freq, relative) = match pattern.kind.as_deref()? {
        "daily" => ("DAILY", false),
        "weekly" => ("WEEKLY", false),
        "absoluteMonthly" => ("MONTHLY", false),
        "relativeMonthly" => ("MONTHLY", true),
        "absoluteYearly" => ("YEARLY", false),
        "relativeYearly" => ("YEARLY", true),
        _ => return None,
    };
    parts.push(format!("FREQ={freq}"));
    if let Some(interval) = pattern.interval.filter(|i| *i > 1) {
        parts.push(format!("INTERVAL={interval}"));
    }
    if !days.is_empty() && freq != "DAILY" {
        parts.push(format!("BYDAY={}", days.join(",")));
    }
    if relative {
        let position = match pattern.index.as_deref().unwrap_or("first") {
            "first" => 1,
            "second" => 2,
            "third" => 3,
            "fourth" => 4,
            "last" => -1,
            _ => return None,
        };
        parts.push(format!("BYSETPOS={position}"));
    } else if freq == "MONTHLY" || freq == "YEARLY" {
        if let Some(day) = pattern.day_of_month.filter(|d| *d > 0) {
            parts.push(format!("BYMONTHDAY={day}"));
        }
    }
    if freq == "YEARLY" {
        if let Some(month) = pattern.month.filter(|m| *m > 0) {
            parts.push(format!("BYMONTH={month}"));
        }
    }
    if freq == "WEEKLY" {
        if let Some(first) = pattern.first_day_of_week.as_deref().and_then(day) {
            parts.push(format!("WKST={first}"));
        }
    }
    if let Some(range) = &recurrence.range {
        match range.kind.as_deref() {
            Some("endDate") => {
                if let Some(end) = &range.end_date {
                    parts.push(format!("UNTIL={}", end.replace('-', "")));
                }
            }
            Some("numbered") => {
                if let Some(count) = range.number_of_occurrences.filter(|c| *c > 0) {
                    parts.push(format!("COUNT={count}"));
                }
            }
            _ => {}
        }
    }
    Some(parts.join(";"))
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

/// Why a page chain stopped short.
#[derive(Debug)]
enum WalkError {
    /// The server dropped the sync state (410, `syncStateNotFound`,
    /// `resyncRequired`): start a new baseline.
    Reset(String),
    /// A 400 or 404 on the route itself: the route is not there for this
    /// calendar.
    Unsupported(String),
    /// A 403: the token lacks the calendar permission.
    Forbidden,
    Failed(anyhow::Error),
}

impl From<WalkError> for CalendarBackendError {
    fn from(err: WalkError) -> Self {
        match err {
            WalkError::Forbidden => CalendarBackendError::MissingScope {
                scope: CALENDAR_SCOPE_NAME.into(),
            },
            WalkError::Failed(err) => CalendarBackendError::Other(err),
            WalkError::Reset(why) | WalkError::Unsupported(why) => {
                CalendarBackendError::Other(anyhow!(why))
            }
        }
    }
}

/// A Graph error body's `error.code`, if it has one.
fn error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value["error"]["code"].as_str().map(str::to_string)
}

/// A Graph error answer as one line: its code and message, else the status.
fn error_text(answer: &HttpAnswer) -> String {
    let value: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_default();
    match (
        value["error"]["code"].as_str(),
        value["error"]["message"].as_str(),
    ) {
        (Some(code), Some(message)) => format!("HTTP {}: {code}: {message}", answer.status),
        (Some(code), None) => format!("HTTP {}: {code}", answer.status),
        _ => format!("HTTP {}", answer.status),
    }
}

/// What a non-success answer means.
fn classify_failure(answer: &HttpAnswer) -> WalkError {
    let code = error_code(&answer.body).unwrap_or_default();
    let reset_code = code.eq_ignore_ascii_case("syncStateNotFound")
        || code.eq_ignore_ascii_case("resyncRequired")
        || code.eq_ignore_ascii_case("syncStateInvalid");
    if answer.status == 410 || reset_code {
        return WalkError::Reset(error_text(answer));
    }
    match answer.status {
        400 | 404 => WalkError::Unsupported(error_text(answer)),
        403 => WalkError::Forbidden,
        _ => WalkError::Failed(anyhow!(
            "Graph calendar request failed: {}",
            error_text(answer)
        )),
    }
}

/// What a page chain brought: its entries in order, and the delta link that
/// ended it (`None` for a plain view).
struct Walked {
    items: Vec<Item>,
    delta_link: Option<String>,
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// One Graph account's calendar.
pub struct GraphCalendarBackend<F = GraphHttp> {
    account: String,
    fetch: F,
    /// The series resolved so far, by master id.
    series: Mutex<HashMap<String, SeriesInfo>>,
}

impl GraphCalendarBackend<GraphHttp> {
    /// The backend of `account`, signed in with its stored Graph token,
    /// refreshed when expired. A token without the calendar scope is
    /// [`CalendarBackendError::MissingScope`] before any network call.
    pub async fn connect(account: &AccountConfig) -> BackendResult<Self> {
        super::calendar_scope_check(account)?;
        let config = GraphConfig::load(account)?;
        let client = GraphClient::new_async(&config).await?;
        Ok(Self::with_fetch(&account.name, GraphHttp { client }))
    }
}

impl<F: GraphFetch> GraphCalendarBackend<F> {
    /// The backend over any [`GraphFetch`], the seam the tests use.
    pub fn with_fetch(account: &str, fetch: F) -> Self {
        GraphCalendarBackend {
            account: account.to_string(),
            fetch,
            series: Mutex::new(HashMap::new()),
        }
    }

    fn known_series(&self, master_id: &str) -> Option<SeriesInfo> {
        self.series
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(master_id)
            .cloned()
    }

    /// Follow a chain of pages from `first`, `@odata.nextLink` after
    /// `@odata.nextLink`, until a page carries an `@odata.deltaLink` (a
    /// delta) or none of the two (a plain view).
    async fn walk(&self, first: &str, delta: bool) -> Result<Walked, WalkError> {
        let prefer = [format!("odata.maxpagesize={PAGE_SIZE}")];
        let mut items = Vec::new();
        let mut url = first.to_string();
        for _ in 0..MAX_PAGES {
            let answer = self
                .fetch
                .get(&url, &prefer)
                .await
                .map_err(WalkError::Failed)?;
            if !(200..300).contains(&answer.status) {
                return Err(classify_failure(&answer));
            }
            let page: wire::Page<serde_json::Value> = serde_json::from_str(&answer.body)
                .map_err(|err| WalkError::Failed(anyhow!("unreadable Graph page: {err}")))?;
            items.extend(page.value.into_iter().filter_map(parse_item));
            match (page.next_link, page.delta_link) {
                (Some(next), _) => url = next,
                (None, Some(link)) if delta => {
                    return Ok(Walked {
                        items,
                        delta_link: Some(link),
                    })
                }
                (None, _) if !delta => {
                    return Ok(Walked {
                        items,
                        delta_link: None,
                    })
                }
                (None, _) => {
                    return Err(WalkError::Failed(anyhow!(
                        "a Graph delta chain ended without a deltaLink"
                    )))
                }
            }
        }
        Err(WalkError::Failed(anyhow!(
            "a Graph page chain ran past {MAX_PAGES} pages"
        )))
    }

    /// A fresh baseline of `calendar` over `window`, through the first route
    /// that answers.
    async fn baseline(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
    ) -> Result<(SyncPath, Walked), WalkError> {
        let range = window_query(window);
        let id = encode_id(&calendar.id);
        let own = format!("{GRAPH_BASE}/me/calendars/{id}/calendarView/delta?{range}");
        match self.walk(&own, true).await {
            Ok(walked) => return Ok((SyncPath::CalendarDelta, walked)),
            Err(WalkError::Unsupported(why)) => {
                debug!(
                    "[calendar] {}: per-calendar delta refused for {} ({why}); falling back",
                    self.account, calendar.name
                );
            }
            Err(err) => return Err(err),
        }
        if calendar.is_default {
            let url = format!("{GRAPH_BASE}/me/calendarView/delta?{range}");
            let walked = self.walk(&url, true).await?;
            Ok((SyncPath::DefaultDelta, walked))
        } else {
            Ok((SyncPath::CalendarView, self.view(calendar, window).await?))
        }
    }

    /// The whole window of `calendar` through the plain `calendarView`.
    async fn view(&self, calendar: &Calendar, window: CalendarWindow) -> Result<Walked, WalkError> {
        let url = format!(
            "{GRAPH_BASE}/me/calendars/{}/calendarView?{}",
            encode_id(&calendar.id),
            window_query(window)
        );
        self.walk(&url, false).await
    }

    /// The pages of one `sync_window`: on from the delta link `token` holds,
    /// or a fresh baseline when there is none or the server dropped it, with
    /// whether the result replaces the calendar's cache.
    async fn walk_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> Result<(SyncPath, Walked, bool), WalkError> {
        match token.as_deref().and_then(GraphToken::decode) {
            Some(GraphToken {
                path: SyncPath::CalendarView,
                ..
            }) => Ok((
                SyncPath::CalendarView,
                self.view(calendar, window).await?,
                true,
            )),
            Some(GraphToken {
                path,
                link: Some(link),
            }) => match self.walk(&link, true).await {
                Ok(walked) => Ok((path, walked, false)),
                Err(WalkError::Reset(why)) => {
                    info!(
                        "[calendar] {}: Graph dropped the sync state of {} ({why}); \
                         syncing the window afresh",
                        self.account, calendar.name
                    );
                    let (path, walked) = self.baseline(calendar, window).await?;
                    Ok((path, walked, true))
                }
                Err(err) => Err(err),
            },
            _ => {
                let (path, walked) = self.baseline(calendar, window).await?;
                Ok((path, walked, true))
            }
        }
    }

    /// The series master `master_id`, read from the server.
    async fn fetch_series(&self, master_id: &str) -> anyhow::Result<SeriesInfo> {
        let url = format!(
            "{GRAPH_BASE}/me/events/{}?$select=iCalUId,recurrence,originalStartTimeZone",
            encode_id(master_id)
        );
        let answer = self.fetch.get(&url, &[]).await?;
        if !(200..300).contains(&answer.status) {
            return Err(anyhow!("{}", error_text(&answer)));
        }
        let master: wire::Master =
            serde_json::from_str(&answer.body).context("unreadable series master")?;
        let uid = master
            .ical_uid
            .filter(|uid| !uid.trim().is_empty())
            .ok_or_else(|| anyhow!("series master without iCalUId"))?;
        Ok(SeriesInfo {
            master_id: master.id.unwrap_or_else(|| master_id.to_string()),
            uid,
            recurrence: master.recurrence.as_ref().and_then(rrule_of),
        })
    }

    /// Fold a walked chain into a [`SyncResult`]: the last word on each id
    /// wins, series masters are resolved (afresh on a baseline, else only
    /// the ones not known yet), and each event converted.
    async fn collect(&self, calendar: &Calendar, walked: Walked, full_resync: bool) -> SyncResult {
        let mut order: Vec<String> = Vec::new();
        let mut last: HashMap<String, Item> = HashMap::new();
        for item in walked.items {
            let id = match &item {
                Item::Upsert(event) => event.id.clone(),
                Item::Removed(id) => id.clone(),
            };
            if last.insert(id.clone(), item).is_none() {
                order.push(id);
            }
        }

        let mut result = SyncResult {
            full_resync,
            ..Default::default()
        };
        // calendarView returns no series master; should one come anyway, it
        // is a free answer to the lookup below rather than an event to show.
        for id in &order {
            if let Some(Item::Upsert(event)) = last.get(id) {
                let uid = event
                    .ical_uid
                    .as_deref()
                    .filter(|uid| !uid.trim().is_empty());
                if let (Some("seriesMaster"), Some(uid)) = (event.kind.as_deref(), uid) {
                    let series = SeriesInfo {
                        master_id: event.id.clone(),
                        uid: uid.to_string(),
                        recurrence: event.recurrence.as_ref().and_then(rrule_of),
                    };
                    self.remember_series(vec![series.clone()]);
                    result.series.push(series);
                }
            }
        }
        let mut wanted: Vec<String> = Vec::new();
        for id in &order {
            if let Some(Item::Upsert(event)) = last.get(id) {
                if event.kind.as_deref() == Some("seriesMaster") {
                    continue;
                }
                if let Some(master) = event.series_master_id.as_ref() {
                    let seen_now = result.series.iter().any(|s| &s.master_id == master);
                    let stale = !seen_now && (full_resync || self.known_series(master).is_none());
                    if is_instance(event.kind.as_deref()) && stale && !wanted.contains(master) {
                        wanted.push(master.clone());
                    }
                }
            }
        }
        for master in wanted {
            match self.fetch_series(&master).await {
                Ok(series) => {
                    self.series
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(master.clone(), series.clone());
                    result.series.push(series);
                }
                Err(err) => warn!(
                    "[calendar] {}: could not read series master {master}: {err:#}; its \
                     occurrences keep their own iCalUId until the next sync",
                    self.account
                ),
            }
        }

        for id in order {
            match last.remove(&id) {
                Some(Item::Removed(id)) => result.removed.push(id),
                Some(Item::Upsert(event)) => {
                    if event.kind.as_deref() == Some("seriesMaster") {
                        continue;
                    }
                    let series = event
                        .series_master_id
                        .as_deref()
                        .and_then(|master| self.known_series(master));
                    match from_graph(&self.account, &calendar.id, &event, series.as_ref()) {
                        Ok(converted) => result.upserts.push(converted),
                        Err(why) => warn!(
                            "[calendar] {}: skipping Graph event {}: {why}",
                            self.account, event.id
                        ),
                    }
                }
                None => {}
            }
        }
        result
    }
}

/// The `startDateTime` and `endDateTime` query of `window`.
fn window_query(window: CalendarWindow) -> String {
    format!(
        "startDateTime={}&endDateTime={}",
        window.start.to_rfc3339_opts(SecondsFormat::Secs, true),
        window.end.to_rfc3339_opts(SecondsFormat::Secs, true)
    )
}

impl<F: GraphFetch> CalendarBackend for GraphCalendarBackend<F> {
    async fn list_calendars(&self) -> BackendResult<Vec<Calendar>> {
        // A 403 here is the token's scope, not one calendar's ACL.
        let url = format!("{GRAPH_BASE}/me/calendars");
        let mut calendars = Vec::new();
        let mut next = Some(url);
        let mut pages = 0;
        while let Some(url) = next.take() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(anyhow!("the Graph calendar list ran past {MAX_PAGES} pages").into());
            }
            let answer = self.fetch.get(&url, &[]).await?;
            if !(200..300).contains(&answer.status) {
                return Err(classify_failure(&answer).into());
            }
            let page: wire::Page<wire::Calendar> =
                serde_json::from_str(&answer.body).context("unreadable Graph calendar list")?;
            calendars.extend(
                page.value
                    .into_iter()
                    .map(|cal| calendar_from_graph(&self.account, cal)),
            );
            next = page.next_link;
        }
        Ok(calendars)
    }

    async fn sync_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> BackendResult<SyncResult> {
        let (path, walked, full_resync) =
            self.walk_window(calendar, window, token)
                .await
                .map_err(|err| match err {
                    // One calendar the account may not read (a shared calendar
                    // its owner's ACL refuses) fails alone: the sync logs it
                    // and goes on with the others, where a missing scope would
                    // stop the account with a re-login prompt the sign-in
                    // cannot fix. The scope itself shows on `list_calendars`.
                    WalkError::Forbidden => CalendarBackendError::Other(anyhow!(
                        "Graph refused access to the calendar {} (HTTP 403)",
                        calendar.name
                    )),
                    other => other.into(),
                })?;
        debug!(
            "[calendar] {}: synced {} through {}",
            self.account,
            calendar.name,
            path.as_str()
        );
        let link = walked.delta_link.clone();
        let mut result = self.collect(calendar, walked, full_resync).await;
        result.path = Some(path.as_str().to_string());
        result.token = Some(GraphToken { path, link }.encode());
        Ok(result)
    }

    async fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
    ) -> BackendResult<()> {
        let action = match response {
            Response::Accept => "accept",
            Response::Tentative => "tentativelyAccept",
            Response::Decline => "decline",
        };
        let url = format!("{GRAPH_BASE}/me/events/{}/{action}", encode_id(event));
        let mut body = serde_json::json!({ "sendResponse": send_response });
        if let Some(comment) = comment.filter(|c| !c.trim().is_empty()) {
            body["comment"] = serde_json::Value::String(comment);
        }
        let answer = self.fetch.post(&url, body).await?;
        if (200..300).contains(&answer.status) {
            return Ok(());
        }
        if answer.status == 403 {
            return Err(CalendarBackendError::MissingScope {
                scope: CALENDAR_SCOPE_NAME.into(),
            });
        }
        Err(anyhow!("Graph refused the {action}: {}", error_text(&answer)).into())
    }

    /// `GET /me/messages/{id}?$expand=microsoft.graph.eventMessage/event`:
    /// the immutable id of the event an `eventMessage` stands for. A message
    /// that is no `eventMessage` carries no `event`, and one the server no
    /// longer has (404) stands for nothing either.
    async fn event_for_message(&self, message_id: &str) -> BackendResult<Option<EventId>> {
        let url = format!(
            "{GRAPH_BASE}/me/messages/{}?$select=id&$expand=microsoft.graph.eventMessage/event($select=id)",
            encode_id(message_id)
        );
        let answer = self.fetch.get(&url, &[]).await?;
        if answer.status == 404 {
            return Ok(None);
        }
        if answer.status == 403 {
            return Err(CalendarBackendError::MissingScope {
                scope: CALENDAR_SCOPE_NAME.into(),
            });
        }
        if !(200..300).contains(&answer.status) {
            return Err(anyhow!(
                "Graph refused the invitation's event lookup: {}",
                error_text(&answer)
            )
            .into());
        }
        let body: serde_json::Value =
            serde_json::from_str(&answer.body).context("unreadable Graph message")?;
        Ok(body
            .get("event")
            .and_then(|event| event.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string))
    }

    fn remember_series(&self, known: Vec<SeriesInfo>) {
        let mut cache = self
            .series
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for series in known {
            cache.insert(series.master_id.clone(), series);
        }
    }
}

#[cfg(test)]
mod tests;
