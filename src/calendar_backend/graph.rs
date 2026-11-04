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
//! (`GET /me/calendars/{id}/events/{seriesMasterId}`, which a calendar shared
//! with the account answers too), once per series: the store caches it
//! ([`SeriesInfo`], [`CalendarBackend::remember_series`]) and a baseline
//! reads it again to refresh the rule. The rule is read into the structured
//! [`Recurrence`] the series cache keeps ([`SeriesInfo::recurrence`]), and
//! every occurrence of the series carries it in words
//! ([`Event::recurrence`]), so a card says how the series repeats.
//!
//! Both delta routes trim an `occurrence` to its `id`, `seriesMasterId`,
//! `type`, `start` and `end`, while the series master in the same feed
//! carries the subject, the all-day flag, the people and the free/busy (the
//! plain `calendarView` answers every occurrence in full). A trimmed
//! occurrence takes its all-day flag, zone and details from its master, kept
//! with the series ([`SeriesInfo::master`]), and is listed in
//! [`SyncResult::inheriting`], so the store re-derives it when only the
//! master changes ([`Event::inherit_from`]).
//! A delta never names an occurrence that left its series (deleted, or ended
//! by a new rule) but re-sends the series whole: a master that comes with
//! some of its occurrences is listed in [`SyncResult::resent_series`], and
//! the store drops its cached occurrences that did not come; a removed
//! master takes its occurrences along in the store.
//!
//! Every request sends `Prefer: IdType="ImmutableId"`. No
//! `Prefer: outlook.timezone` is sent, so times come back in UTC and the
//! event's own zone is read from `originalStartTimeZone`, a Windows name
//! mapped through [`crate::windows_zones`]; a zone the table does not know
//! (`tzone://Microsoft/Custom`) is labelled UTC, with the instants exact.
//!
//! Writes (`write.rs`, PERSO-255) create, patch, cancel and delete events
//! and dismiss or snooze their reminders, sending each time as the event's
//! wall time in its Windows zone ([`crate::windows_zones::iana_to_windows_zone`]),
//! or in UTC for a zone the table lacks; an all-day event's midnights go out
//! in the Windows zone of the write's `zone`, the host's when absent.
//!
//! Free/busy (`freebusy.rs`, PERSO-255) is `POST /me/calendar/getSchedule`,
//! one schedule per address, an address Graph cannot read answering an
//! error of its own rather than failing the query.
//!
//! The HTTP layer is the [`GraphFetch`] seam: [`GraphHttp`] over the mail
//! client's [`GraphClient`] in production, a scripted fake in the tests, so
//! the paging, the fallbacks and the 410 handling run without a tenant.

use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::Mutex;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use super::{
    BackendResult, CalendarBackend, CalendarBackendError, Response, SeriesInfo, SyncResult, Written,
};
use crate::calendar_model::{
    Attendee, AttendeeRole, Calendar, CalendarFeature, CalendarWindow, Event, EventId, EventKind,
    EventTime, Frequency, OnlineMeetingProvider, OwnResponse, Person, Recurrence, ResponseStatus,
    Sensitivity, ShowAs, TimeSlot, WeekIndex, Weekday,
};
use crate::calendar_write::{EventInput, EventPatch, ProposedTime};
use crate::config::{AccountConfig, GraphConfig};
use crate::graph::{encode_id, GraphClient, GRAPH_BASE, PREFER_IMMUTABLE_ID};

/// How many events a delta or view page may carry.
const PAGE_SIZE: usize = 100;

/// The most pages one sync follows before it gives up on the chain: 50 000
/// events at [`PAGE_SIZE`], far beyond a year of anyone's calendar.
const MAX_PAGES: usize = 500;

/// The scope a 403 on a calendar call is reported missing.
const CALENDAR_SCOPE_NAME: &str = "Calendars.ReadWrite";

/// What this build serves on a Graph calendar beyond reading and answering
/// ([`super::features`]), each feature listed once its daemon method ships:
/// `event.create` writes events, with a Teams link on request on a calendar
/// whose `allowedOnlineMeetingProviders` names Teams
/// ([`super::calendar_features`]), `calendar.dismiss_reminder` and
/// `calendar.snooze_reminder` write a reminder's dismissal and snooze through,
/// `calendar.freebusy` reads `getSchedule` (`freebusy.rs`), and
/// `event.respond` proposes a new time with `proposedNewTime`.
pub const FEATURES: &[CalendarFeature] = &[
    CalendarFeature::Write,
    CalendarFeature::OnlineMeeting,
    CalendarFeature::ProposeTime,
    CalendarFeature::FreeBusy,
    CalendarFeature::ServerReminders,
];

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

/// The four kinds of request the adapter makes. Every one carries
/// `Prefer: IdType="ImmutableId"`, so the ids a write names and the ones it
/// answers with are the immutable ids the cache holds; `prefer` adds to it.
pub trait GraphFetch: Send + Sync {
    fn get(
        &self,
        url: &str,
        prefer: &[String],
    ) -> impl Future<Output = anyhow::Result<HttpAnswer>> + Send;

    /// A JSON `body`; an action that takes no parameters (`dismissReminder`)
    /// posts `{}`, since Exchange answers an empty POST, which reqwest sends
    /// without a `Content-Length`, with HTTP 411.
    fn post(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> impl Future<Output = anyhow::Result<HttpAnswer>> + Send;

    fn patch(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> impl Future<Output = anyhow::Result<HttpAnswer>> + Send;

    fn delete(&self, url: &str) -> impl Future<Output = anyhow::Result<HttpAnswer>> + Send;
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

impl GraphHttp {
    /// One request of the seam, with its `Prefer` header and its JSON body,
    /// when it has one.
    fn build(
        &self,
        method: reqwest::Method,
        url: &str,
        prefer: &[String],
        body: Option<serde_json::Value>,
    ) -> reqwest::RequestBuilder {
        let request = self
            .client
            .request(method, url)
            .header("Prefer", prefer_header(prefer));
        match body {
            None => request,
            Some(body) => request.json(&body),
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        prefer: &[String],
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<HttpAnswer> {
        let what = format!("{method} {}", strip_query(url));
        let resp = self
            .build(method, url, prefer, body)
            .send()
            .await
            .with_context(|| what)?;
        read_answer(resp).await
    }
}

impl GraphFetch for GraphHttp {
    async fn get(&self, url: &str, prefer: &[String]) -> anyhow::Result<HttpAnswer> {
        self.send(reqwest::Method::GET, url, prefer, None).await
    }

    async fn post(&self, url: &str, body: serde_json::Value) -> anyhow::Result<HttpAnswer> {
        self.send(reqwest::Method::POST, url, &[], Some(body)).await
    }

    async fn patch(&self, url: &str, body: serde_json::Value) -> anyhow::Result<HttpAnswer> {
        self.send(reqwest::Method::PATCH, url, &[], Some(body))
            .await
    }

    async fn delete(&self, url: &str) -> anyhow::Result<HttpAnswer> {
        self.send(reqwest::Method::DELETE, url, &[], None).await
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
        /// `teamsForBusiness`, `skypeForBusiness`, `skypeForConsumer` or
        /// `unknown`; empty for a mailbox without a Teams licence.
        #[serde(default)]
        pub allowed_online_meeting_providers: Vec<String>,
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
    pub struct TimeSlot {
        pub start: Option<DateTimeZone>,
        pub end: Option<DateTimeZone>,
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Attendee {
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub status: Option<Status>,
        pub email_address: Option<EmailAddress>,
        pub proposed_new_time: Option<TimeSlot>,
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
        pub start_date: Option<String>,
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
    /// has rather than failing the page; a trimmed occurrence takes its
    /// details from its series master ([`super::from_graph`]).
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
        pub is_all_day: Option<bool>,
        pub location: Option<Location>,
        pub organizer: Option<Recipient>,
        pub attendees: Option<Vec<Attendee>>,
        pub response_status: Option<Status>,
        pub recurrence: Option<Recurrence>,
        pub series_master_id: Option<String>,
        #[serde(rename = "type")]
        pub kind: Option<String>,
        pub original_start: Option<String>,
        pub original_start_time_zone: Option<String>,
        pub reminder_minutes_before_start: Option<i64>,
        pub is_reminder_on: Option<bool>,
        pub online_meeting: Option<OnlineMeeting>,
        pub online_meeting_url: Option<String>,
        pub is_online_meeting: Option<bool>,
        pub online_meeting_provider: Option<String>,
        pub allow_new_time_proposals: Option<bool>,
        pub response_requested: Option<bool>,
        pub transaction_id: Option<String>,
        pub sensitivity: Option<String>,
        pub show_as: Option<String>,
        pub is_cancelled: Option<bool>,
        pub is_organizer: Option<bool>,
        pub web_link: Option<String>,
        pub last_modified_date_time: Option<String>,
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
        online_meeting_providers: cal
            .allowed_online_meeting_providers
            .iter()
            .filter_map(|raw| meeting_provider(Some(raw)))
            .collect(),
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
        proposed_time: raw.proposed_new_time.as_ref().and_then(time_slot),
    })
}

/// A Graph `timeSlot` as instants; `None` when a bound is missing or
/// unreadable, or the slot ends before it starts.
fn time_slot(slot: &wire::TimeSlot) -> Option<TimeSlot> {
    let start = instant_of(slot.start.as_ref()?).ok()?;
    let end = instant_of(slot.end.as_ref()?).ok()?;
    (end >= start).then_some(TimeSlot { start, end })
}

/// A Graph event `type` as the model's kind; an event without one is single.
fn event_kind(raw: Option<&str>) -> EventKind {
    match raw {
        Some("seriesMaster") => EventKind::SeriesMaster,
        Some("occurrence") => EventKind::Occurrence,
        Some("exception") => EventKind::Exception,
        _ => EventKind::Single,
    }
}

/// A Graph `onlineMeetingProvider`; `unknown`, which Graph answers for an
/// event that is no online meeting, is none.
fn meeting_provider(raw: Option<&str>) -> Option<OnlineMeetingProvider> {
    match raw {
        Some("teamsForBusiness") => Some(OnlineMeetingProvider::TeamsForBusiness),
        Some("skypeForBusiness") => Some(OnlineMeetingProvider::SkypeForBusiness),
        Some("skypeForConsumer") => Some(OnlineMeetingProvider::SkypeForConsumer),
        _ => None,
    }
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
    event_kind(kind).is_instance()
}

/// Whether `event` is an occurrence Graph's delta trimmed to its ids and
/// times, which takes every detail from its series master
/// ([`SyncResult::inheriting`]).
fn is_trimmed(event: &wire::Event) -> bool {
    is_instance(event.kind.as_deref()) && event.subject.is_none() && event.is_all_day.is_none()
}

/// A Graph event as the neutral model has it. `series` is the event's series
/// master, for an occurrence: its `UID` is the one the series shares, and an
/// occurrence a delta trimmed to its times places them with the master's
/// all-day flag and zone and takes every detail from it
/// ([`Event::inherit_from`]). An occurrence carries its series' rule in
/// words, and a series master its own.
pub(crate) fn from_graph(
    account: &str,
    calendar: &str,
    event: &wire::Event,
    series: Option<&SeriesInfo>,
) -> Result<Event, String> {
    let kind = event_kind(event.kind.as_deref());
    let instance = kind.is_instance();
    let series = series.filter(|_| instance);
    let master = series.and_then(|s| s.master.as_deref());

    let start = event.start.as_ref().ok_or("no start")?;
    let end = event.end.as_ref().ok_or("no end")?;
    let own_zone = match event.original_start_time_zone.as_deref() {
        Some(name) => zone_of(Some(name)),
        None => zone_of(series.and_then(|s| s.zone.as_deref())),
    };
    let is_all_day = event
        .is_all_day
        .or(master.map(Event::is_all_day))
        .unwrap_or(false);
    let time = if is_all_day {
        all_day_time(start, end, own_zone)?
    } else {
        let zone = own_zone.map_or("UTC", |tz| tz.name());
        EventTime::timed(instant_of(start)?, instant_of(end)?, zone).map_err(|e| e.to_string())?
    };

    let own_uid = event
        .ical_uid
        .clone()
        .filter(|uid| !uid.trim().is_empty())
        .unwrap_or_else(|| event.id.clone());
    let uid = match series {
        Some(series) => series.uid.clone(),
        None => own_uid,
    };
    let recurrence_id = if instance {
        Some(recurrence_id(event, &time, own_zone)?)
    } else {
        None
    };
    let rule = match kind {
        EventKind::SeriesMaster => event.recurrence.as_ref().and_then(recurrence_of),
        _ => series.and_then(|s| s.recurrence.clone()),
    };

    let mut converted = Event {
        account: account.to_string(),
        calendar: calendar.to_string(),
        id: event.id.clone(),
        uid,
        recurrence_id,
        series_master_id: event.series_master_id.clone().filter(|_| instance),
        kind,
        summary: event.subject.clone().unwrap_or_default(),
        location: event
            .location
            .as_ref()
            .and_then(|location| location.display_name.as_deref())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string),
        body: plain_body(event),
        organizer: person(event.organizer.as_ref()),
        attendees: event
            .attendees
            .iter()
            .flatten()
            .filter_map(attendee)
            .collect(),
        own_response: own_response(
            event
                .response_status
                .as_ref()
                .and_then(|s| s.response.as_deref()),
        ),
        recurrence: rule.as_ref().map(Recurrence::describe),
        reminder: event
            .reminder_minutes_before_start
            .filter(|_| event.is_reminder_on.unwrap_or(false))
            .and_then(|minutes| u32::try_from(minutes).ok()),
        online_meeting_url: event
            .online_meeting
            .as_ref()
            .and_then(|m| m.join_url.clone())
            .or_else(|| event.online_meeting_url.clone())
            .filter(|url| !url.trim().is_empty()),
        is_online_meeting: event.is_online_meeting.unwrap_or(false),
        online_meeting_provider: meeting_provider(event.online_meeting_provider.as_deref()),
        sensitivity: sensitivity(event.sensitivity.as_deref()),
        show_as: show_as(event.show_as.as_deref()),
        cancelled: event.is_cancelled.unwrap_or(false),
        is_organizer: event.is_organizer.unwrap_or(false),
        last_modified: event
            .last_modified_date_time
            .as_deref()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|at| at.with_timezone(&Utc)),
        web_link: event.web_link.clone(),
        allow_new_time_proposals: event.allow_new_time_proposals.unwrap_or(true),
        response_requested: event.response_requested.unwrap_or(true),
        transaction_id: event
            .transaction_id
            .clone()
            .filter(|id| !id.trim().is_empty()),
        time,
    };
    if let Some(master) = master.filter(|_| is_trimmed(event)) {
        converted.inherit_from(master);
    }
    Ok(converted)
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

/// A Graph `patternedRecurrence` as the model's rule, or `None` for a
/// pattern it cannot express.
///
/// Graph fills every pattern field whatever the type (a weekly rule comes
/// with `dayOfMonth: 0`, `month: 0` and `index: "first"`, and a range with
/// `endDate: "0001-01-01"`), so each type reads only the fields it names
/// and a zero or year-one value is no value.
fn recurrence_of(recurrence: &wire::Recurrence) -> Option<Recurrence> {
    let pattern = recurrence.pattern.as_ref()?;
    let (frequency, relative) = match pattern.kind.as_deref()? {
        "daily" => (Frequency::Daily, false),
        "weekly" => (Frequency::Weekly, false),
        "absoluteMonthly" => (Frequency::Monthly, false),
        "relativeMonthly" => (Frequency::Monthly, true),
        "absoluteYearly" => (Frequency::Yearly, false),
        "relativeYearly" => (Frequency::Yearly, true),
        _ => return None,
    };
    let positive = |value: Option<u32>| value.filter(|v| *v > 0);
    let date = |raw: Option<&String>| {
        raw.and_then(|raw| NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok())
            .filter(|date| date.year() > 1)
    };
    let weekdays: Vec<Weekday> = pattern
        .days_of_week
        .iter()
        .filter_map(|day| Weekday::from_name(day))
        .collect();
    let mut rule = Recurrence {
        frequency,
        interval: positive(pattern.interval).unwrap_or(1),
        weekdays: Vec::new(),
        day_of_month: None,
        week_index: None,
        month: None,
        first_day_of_week: None,
        start: date(
            recurrence
                .range
                .as_ref()
                .and_then(|r| r.start_date.as_ref()),
        ),
        until: None,
        count: None,
    };
    match frequency {
        Frequency::Daily => {}
        Frequency::Weekly => {
            rule.weekdays = weekdays;
            rule.first_day_of_week = pattern
                .first_day_of_week
                .as_deref()
                .and_then(Weekday::from_name);
        }
        Frequency::Monthly | Frequency::Yearly => {
            if relative {
                rule.week_index = Some(WeekIndex::from_name(
                    pattern.index.as_deref().unwrap_or("first"),
                )?);
                rule.weekdays = weekdays;
            } else {
                rule.day_of_month = positive(pattern.day_of_month);
            }
            if frequency == Frequency::Yearly {
                rule.month = positive(pattern.month).filter(|m| *m <= 12);
            }
        }
    }
    if let Some(range) = &recurrence.range {
        match range.kind.as_deref() {
            Some("endDate") => rule.until = date(range.end_date.as_ref()),
            Some("numbered") => rule.count = positive(range.number_of_occurrences),
            _ => {}
        }
    }
    Some(rule)
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

    /// The series master `master_id` of `calendar`, read from the server with
    /// the properties a trimmed occurrence takes from it.
    async fn fetch_series(
        &self,
        calendar: &str,
        master_id: &str,
    ) -> Result<SeriesInfo, MasterError> {
        let url = format!(
            "{GRAPH_BASE}/me/calendars/{}/events/{}?$select={MASTER_SELECT}",
            encode_id(calendar),
            encode_id(master_id)
        );
        let answer = self
            .fetch
            .get(&url, &[])
            .await
            .map_err(MasterError::Other)?;
        if answer.status == 404 {
            return Err(MasterError::Gone(error_text(&answer)));
        }
        if !(200..300).contains(&answer.status) {
            return Err(MasterError::Other(anyhow!("{}", error_text(&answer))));
        }
        let master: wire::Event = serde_json::from_str(&answer.body)
            .context("unreadable series master")
            .map_err(MasterError::Other)?;
        series_of(&self.account, calendar, &master)
            .ok_or_else(|| MasterError::Other(anyhow!("series master without iCalUId")))
    }

    /// Fold a walked chain into a [`SyncResult`]: the last word on each id
    /// wins, series masters are resolved (afresh on a baseline, else only
    /// the ones not known yet), and each event converted.
    ///
    /// The flag is `true` when a trimmed occurrence found no master to take
    /// its details from: it is stored as Graph sent it (untitled, timed in
    /// UTC) and not as inheriting, since a later master could fix its details
    /// but not its time, and the caller hands back no token, so the next sync
    /// is a baseline that reads every master again and the occurrence anew.
    /// A master Graph answers 404 for is gone or unreadable to the account,
    /// which a baseline would only meet again at every poll: its occurrences
    /// are stored the same way, but the token is kept.
    async fn collect(
        &self,
        calendar: &Calendar,
        walked: Walked,
        full_resync: bool,
    ) -> (SyncResult, bool) {
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
        // A delta brings the series masters of the window beside their
        // occurrences, and calendarView none: a master is the answer to the
        // lookup below and the details of its trimmed occurrences, never an
        // event to show.
        for id in &order {
            if let Some(Item::Upsert(event)) = last.get(id) {
                if event.kind.as_deref() != Some("seriesMaster") {
                    continue;
                }
                if let Some(series) = series_of(&self.account, &calendar.id, event) {
                    self.remember_series(vec![series.clone()]);
                    result.series.push(series);
                }
            }
        }
        let mut wanted: Vec<String> = Vec::new();
        for id in &order {
            if let Some(Item::Upsert(event)) = last.get(id) {
                if !is_instance(event.kind.as_deref()) {
                    continue;
                }
                if let Some(master) = event.series_master_id.as_ref() {
                    let seen_now = result.series.iter().any(|s| &s.master_id == master);
                    let known = self.known_series(master);
                    // A trimmed occurrence needs the master's details, which a
                    // series cached by an older build lacks.
                    let detailless = is_trimmed(event)
                        && known.as_ref().is_some_and(|series| series.master.is_none());
                    let stale = !seen_now && (full_resync || known.is_none() || detailless);
                    if stale && !wanted.contains(master) {
                        wanted.push(master.clone());
                    }
                }
            }
        }
        let mut gone: Vec<String> = Vec::new();
        for master in wanted {
            match self.fetch_series(&calendar.id, &master).await {
                Ok(series) => {
                    self.series
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(master.clone(), series.clone());
                    result.series.push(series);
                }
                Err(MasterError::Gone(why)) => {
                    warn!(
                        "[calendar] {}: series master {master} of {} is gone or unreadable \
                         ({why}); its occurrences keep what Graph sent of them",
                        self.account, calendar.name
                    );
                    gone.push(master);
                }
                Err(MasterError::Other(err)) => warn!(
                    "[calendar] {}: could not read series master {master}: {err:#}; its \
                     occurrences keep their own iCalUId until the next sync",
                    self.account
                ),
            }
        }

        // A delta re-sends a series whole when anything in it changed: the
        // master and every occurrence of the window, never naming one that
        // left (probes of 2026-11-04, `.agents/research/`). A master that came
        // with at least one of its occurrences is such a series, whose cached
        // occurrences the store holds to what came; a master alone prunes
        // nothing, so a delta that re-sent no occurrence drops none.
        if !full_resync {
            for id in &order {
                let Some(Item::Upsert(event)) = last.get(id) else {
                    continue;
                };
                if event.kind.as_deref() != Some("seriesMaster") {
                    continue;
                }
                let brought = last.values().any(|item| {
                    matches!(item, Item::Upsert(occurrence)
                        if is_instance(occurrence.kind.as_deref())
                            && occurrence.series_master_id.as_deref() == Some(id.as_str()))
                });
                if brought {
                    result.resent_series.push(id.clone());
                }
            }
        }

        let mut orphaned = false;
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
                        Ok(converted) => {
                            if is_trimmed(&event) {
                                if series.as_ref().is_some_and(|s| s.master.is_some()) {
                                    result.inheriting.push(converted.id.clone());
                                } else if !event
                                    .series_master_id
                                    .as_ref()
                                    .is_some_and(|master| gone.contains(master))
                                {
                                    orphaned = true;
                                }
                            }
                            result.upserts.push(converted);
                        }
                        Err(why) => warn!(
                            "[calendar] {}: skipping Graph event {}: {why}",
                            self.account, event.id
                        ),
                    }
                }
                None => {}
            }
        }
        (result, orphaned)
    }
}

/// Why [`GraphCalendarBackend::fetch_series`] read no series master.
enum MasterError {
    /// Graph answered 404: the master is gone or the account cannot read it,
    /// which no later sync changes.
    Gone(String),
    /// Anything else (the network, a 5xx, a 429, an unreadable answer),
    /// which a later sync may get past.
    Other(anyhow::Error),
}

/// The properties [`GraphCalendarBackend::fetch_series`] reads of a series
/// master: its `UID`, rule and zone, and the details a trimmed occurrence
/// takes from it.
const MASTER_SELECT: &str = "iCalUId,recurrence,originalStartTimeZone,type,start,end,isAllDay,\
    subject,body,bodyPreview,location,organizer,attendees,responseStatus,isOrganizer,\
    isCancelled,isReminderOn,reminderMinutesBeforeStart,onlineMeeting,onlineMeetingUrl,\
    isOnlineMeeting,onlineMeetingProvider,allowNewTimeProposals,responseRequested,transactionId,\
    sensitivity,showAs";

/// What a series master tells about its series: its `UID` (`None` without
/// one), its rule, its zone and its details as an event.
fn series_of(account: &str, calendar: &str, master: &wire::Event) -> Option<SeriesInfo> {
    let uid = master
        .ical_uid
        .as_deref()
        .filter(|uid| !uid.trim().is_empty())?;
    let details = match from_graph(account, calendar, master, None) {
        Ok(details) => Some(Box::new(details)),
        Err(why) => {
            debug!(
                "[calendar] {account}: series master {} gives no details: {why}",
                master.id
            );
            None
        }
    };
    Some(SeriesInfo {
        master_id: master.id.clone(),
        uid: uid.to_string(),
        recurrence: master.recurrence.as_ref().and_then(recurrence_of),
        master: details,
        zone: zone_of(master.original_start_time_zone.as_deref()).map(|tz| tz.name().to_string()),
    })
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
        let (mut result, orphaned) = self.collect(calendar, walked, full_resync).await;
        result.path = Some(path.as_str().to_string());
        if orphaned {
            warn!(
                "[calendar] {}: an occurrence of {} came without its series master; \
                 the next sync reads the window afresh",
                self.account, calendar.name
            );
        } else {
            result.token = Some(GraphToken { path, link }.encode());
        }
        Ok(result)
    }

    async fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
        proposed: Option<ProposedTime>,
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
        // Graph takes a proposal on tentativelyAccept and decline only, as
        // a timeSlot in the wall time of the zone it was typed in.
        if let Some(proposed) = proposed.filter(|_| response != Response::Accept) {
            body["proposedNewTime"] = write::time_slot_json(&proposed);
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

    async fn create_event(
        &self,
        calendar: &Calendar,
        input: &EventInput,
        transaction_id: &str,
    ) -> BackendResult<Written> {
        self.post_event(calendar, input, transaction_id).await
    }

    async fn update_event(
        &self,
        calendar: &str,
        event: &str,
        patch: &EventPatch,
    ) -> BackendResult<Written> {
        self.patch_event(calendar, event, patch).await
    }

    async fn cancel_event(&self, event: &str, comment: Option<String>) -> BackendResult<()> {
        self.post_cancel(event, comment).await
    }

    async fn delete_event(&self, event: &str) -> BackendResult<()> {
        self.send_delete(event).await
    }

    async fn dismiss_reminder(&self, event: &str) -> BackendResult<()> {
        self.post_dismiss(event).await
    }

    async fn snooze_reminder(&self, event: &str, until: DateTime<Utc>) -> BackendResult<()> {
        self.post_snooze(event, until).await
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

    async fn free_busy(
        &self,
        query: &crate::calendar_freebusy::FreeBusyQuery,
    ) -> BackendResult<Vec<crate::calendar_freebusy::Schedule>> {
        self.post_schedule(query).await
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

mod freebusy;
mod write;

#[cfg(test)]
mod tests;
