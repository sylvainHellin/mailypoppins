//! The `calendar.*` and invite-shaped wire types (P5-U10).
//!
//! [`EventFrontmatter`] and [`EventAttendee`] were `mailypoppins::types`'
//! until this unit; they are pure serde structs with no engine dependency, and
//! both ends of the socket now need them, so they live here and the root crate
//! re-exports them under their old paths. Nothing about the YAML frontmatter
//! they also serialise changed: the field names, the `#[serde]` attributes and
//! the skip rules are the ones `.agents/plans/calendar-invites.md` D2 fixed.
//!
//! [`AgendaEvent`] is one invitation row of the local agenda: one
//! `messages.id`, the frontmatter block the shared event card renders, and the
//! five derived columns the agenda sorts by. The daemon turns it into the
//! events of an account's invitation calendar, and the desktop fixtures hold
//! their invitations in this shape.
//!
//! The server calendar (PERSO-200) adds [`CalendarInfo`], the row of
//! `calendar.list`, and [`CalendarEvent`], the event of `calendar.view` and
//! `event.get`. `CalendarEvent` mirrors `mp_core::calendar_model::Event`
//! field for field and key for key, so the daemon serialises its model and a
//! client decodes this, without either crate linking the other: times are the
//! RFC 3339 strings and dates the `YYYY-MM-DD` strings chrono writes, and the
//! model's enums travel as their kebab-case words.

use serde::{Deserialize, Serialize};

/// A single attendee within an `event:` frontmatter block.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventAttendee {
    pub address: String,
    /// needs-action | accepted | tentative | declined
    pub status: String,
}

/// The nested `event:` frontmatter block populated when an email carries an
/// iMIP calendar invitation. The sidecar `.ics` is the source of truth; this
/// block is a render/query cache (see `.agents/plans/calendar-invites.md`, D2).
///
/// Every field is optional or defaulted so that emails without an `event:`
/// block (the vast majority) round-trip unchanged.
#[derive(Debug, Default, Deserialize, Serialize, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventFrontmatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// `RECURRENCE-ID` of a single-occurrence payload (#0031), `None` for the
    /// whole series. Part of the event identity together with `uid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<String>,
    /// REQUEST | REPLY | CANCEL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default)]
    pub sequence: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// RFC3339 with offset where the source carried a resolvable timezone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<String>,
    /// Own RSVP status: needs-action | accepted | tentative | declined.
    #[serde(default)]
    pub rsvp: String,
    /// Human-readable RRULE summary, empty when the event does not recur (D6).
    #[serde(default)]
    pub recurrence: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<EventAttendee>,
    /// Derived (#0031): a `METHOD:CANCEL` for this identity exists locally with
    /// a sequence at least this event's. The event is kept and shown, marked
    /// cancelled -- never deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    /// Derived (#0031): a newer `METHOD:REQUEST` for the same identity exists
    /// locally (higher `SEQUENCE`, or the same one with a later `DTSTAMP`), so
    /// this copy is a superseded version of the event.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub superseded: bool,
    /// Derived (#0031): the `RECURRENCE-ID`s of occurrences of this series that
    /// were cancelled individually. Empty for a non-recurring event and for a
    /// single-occurrence payload (which reports its own state in `cancelled`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled_instances: Vec<String>,
}

/// One row of the local agenda: an invitation, already deduped, folded and
/// sorted by the daemon, which `calendar.view` answers as an event of the
/// account's invitation calendar.
///
/// The derived columns are computed where the store is because every one of
/// them is a fold over the account's *other* rows (the reply statuses, the
/// cancellation chain, the sent-copy tiebreak), which is exactly what a client
/// without a store cannot do.
///
/// No field is skipped when empty: a row is one object per event and is
/// decoded whole, so a stable key set is worth more than the bytes.
#[derive(Debug, Default, Deserialize, Serialize, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgendaEvent {
    /// The `messages.id` of the winning copy, the address an RSVP from the
    /// agenda uses (#0050).
    pub row_id: i64,
    /// The `event:` frontmatter block, rendered by the shared event card.
    pub event: EventFrontmatter,
    /// Email subject, the row title when the event carries no `summary`.
    pub subject: String,
    /// UTC-normalised `YYYY-MM-DDTHH:MM:SS` sort key, empty when the start is
    /// missing or unparseable (those rows sort last).
    pub start_sort: String,
    /// UTC-normalised end key, empty when unknown.
    pub end_sort: String,
    /// Local, human-readable start (`YYYY-MM-DD HH:MM`, or the date alone for
    /// an all-day event). Empty when the start is unknown.
    pub start_display: String,
    /// True when the winning copy came from the Sent mailbox, i.e. we are the
    /// organizer (no own-RSVP, and RSVP is refused).
    pub is_organizer: bool,
    /// True when a `METHOD:CANCEL` for this identity is stored with a sequence
    /// at least as high as this event's (#0031). Mirrors `event.cancelled`.
    pub cancelled: bool,
}

/// The virtual calendar id an account's invitation mails show under
/// (`calendar.list`, `calendar.view`), so a client can hide them as it hides a
/// server calendar.
pub const IMIP_CALENDAR_ID: &str = "imip";

/// The prefix of an event id `calendar.view` derives from an invitation mail:
/// `imip:<messages.id>`.
pub const IMIP_EVENT_PREFIX: &str = "imip:";

/// One calendar of one account, as `calendar.list` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarInfo {
    /// The configured account name.
    pub account: String,
    /// The server id, or [`IMIP_CALENDAR_ID`] for the invitation mails.
    pub id: String,
    pub name: String,
    /// `#rrggbb`, `null` when the server names no colour.
    pub color: Option<String>,
    /// The account's default calendar, where a new event goes.
    pub is_default: bool,
    /// Whether the user may create and change events in it.
    pub can_edit: bool,
    /// Shown in the views: a local preference that survives a rebuild.
    pub visible: bool,
    /// The account's calendar backend as the configuration spells it
    /// (`graph`, `ews`, `caldav`, `none`); `none` on the invitation calendar.
    pub backend: String,
    /// What the backend lets a client do beyond reading and answering, each
    /// listed once the daemon serves it there: `write`, `online-meeting`,
    /// `propose-time`, `free-busy`, `server-reminders`. Empty on the
    /// invitation calendar.
    #[serde(default)]
    pub features: Vec<String>,
}

/// When an event happens: UTC instants and the event's IANA zone, or local
/// dates with an exclusive end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CalendarEventTime {
    /// `start` and `end` are RFC 3339 instants in UTC.
    Timed {
        start: String,
        end: String,
        zone: String,
    },
    /// `start` and `end` are `YYYY-MM-DD`, `end` exclusive.
    AllDay { start: String, end: String },
}

/// A person named on an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarPerson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    pub email: String,
}

/// A span of time, RFC 3339 instants in UTC, the end exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarTimeSlot {
    pub start: String,
    pub end: String,
}

/// One attendee of an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarAttendee {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    pub email: String,
    /// `required`, `optional` or `resource`.
    pub role: String,
    /// `needs-action`, `accepted`, `tentative` or `declined`.
    pub status: String,
    /// The time the attendee proposed instead, which the server shows the
    /// organizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub proposed_time: Option<CalendarTimeSlot>,
}

/// The rule of a recurring series, mirroring
/// `mp_core::calendar_model::Recurrence`: which fields a pattern reads is
/// documented there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarRecurrence {
    /// `daily`, `weekly`, `monthly` or `yearly`.
    pub frequency: String,
    /// Every how many days, weeks, months or years.
    pub interval: u32,
    /// `monday` to `sunday`; empty on a pattern that names no weekday.
    #[serde(default)]
    pub weekdays: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub day_of_month: Option<u32>,
    /// `first`, `second`, `third`, `fourth` or `last`, on a rule by weekday.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub week_index: Option<String>,
    /// 1 to 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub month: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub first_day_of_week: Option<String>,
    /// `YYYY-MM-DD`, the day the range starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub start: Option<String>,
    /// `YYYY-MM-DD`, the last day an occurrence may fall on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub until: Option<String>,
    /// How many occurrences the series has; with no `until` either, the
    /// series has no end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub count: Option<u32>,
}

/// The series an occurrence belongs to, as `event.get` answers it from the
/// series cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarSeries {
    /// The server id of the series master.
    pub master_id: String,
    /// The series' own summary, when the cache holds its master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub summary: Option<String>,
    /// The rule, when the backend could express it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recurrence: Option<CalendarRecurrence>,
}

/// Where an event `calendar.view` derived from an invitation mail came from,
/// so a client can open the mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarEventSource {
    /// `imip`, the one source so far.
    pub kind: String,
    /// The `messages.id` of the invitation's winning copy.
    pub row_id: i64,
    /// The canonical selector of that message.
    pub selector: String,
    /// Its subject.
    pub subject: String,
}

/// One event, or one occurrence of a series, as `calendar.view` and
/// `event.get` answer it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarEvent {
    /// The configured account name.
    pub account: String,
    /// The calendar id, [`IMIP_CALENDAR_ID`] for an invitation mail's event.
    pub calendar: String,
    /// The id `event.get` and `event.respond` take: the server id, or
    /// `imip:<row_id>` for an invitation mail's event.
    pub id: String,
    /// The iCalendar `UID`, `row:<row_id>` for an invitation that has none.
    pub uid: String,
    /// The `RECURRENCE-ID` of an occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recurrence_id: Option<String>,
    /// The server id of the series master, on an occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub series_master_id: Option<String>,
    /// `single`, `series-master`, `occurrence`, or `exception` for an
    /// occurrence changed apart from its series.
    #[serde(default = "single")]
    pub kind: String,
    pub summary: String,
    pub time: CalendarEventTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub location: Option<String>,
    /// The description as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub organizer: Option<CalendarPerson>,
    #[serde(default)]
    pub attendees: Vec<CalendarAttendee>,
    /// `none`, `organizer`, `needs-action`, `accepted`, `tentative` or
    /// `declined`.
    pub own_response: String,
    /// The series rule in words (`Weekly on Wednesday`), on an occurrence and
    /// a series master; on an invitation mail's event, the summary the mail's
    /// agenda row carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recurrence: Option<String>,
    /// Minutes before the start a reminder fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reminder: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub online_meeting_url: Option<String>,
    /// Whether the event is an online meeting, which a server can say before
    /// its join link exists.
    #[serde(default)]
    pub is_online_meeting: bool,
    /// `teams-for-business`, `skype-for-business` or `skype-for-consumer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub online_meeting_provider: Option<String>,
    /// `normal`, `personal`, `private` or `confidential`.
    pub sensitivity: String,
    /// `free`, `tentative`, `busy`, `oof`, `working-elsewhere` or `unknown`.
    pub show_as: String,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub is_organizer: bool,
    /// RFC 3339, when the server reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_modified: Option<String>,
    /// The event in the server's own web client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub web_link: Option<String>,
    /// Whether the organizer lets attendees propose a new time.
    #[serde(default = "yes")]
    pub allow_new_time_proposals: bool,
    /// Whether the organizer asks for an answer.
    #[serde(default = "yes")]
    pub response_requested: bool,
    /// The id of the create that made the event, when an app created it
    /// with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub transaction_id: Option<String>,
    /// The invitation mail an `imip:` event was derived from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<CalendarEventSource>,
}

impl CalendarEvent {
    /// Whether the event is one occurrence of a series, which a change
    /// addresses with a scope: an occurrence, an exception, or any event
    /// naming its series master.
    pub fn is_occurrence(&self) -> bool {
        matches!(self.kind.as_str(), "occurrence" | "exception") || self.series_master_id.is_some()
    }
}

fn single() -> String {
    "single".to_string()
}

fn yes() -> bool {
    true
}

/// A half-open range of instants, RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarRange {
    pub start: String,
    pub end: String,
}

/// The `result` of `calendar.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarListResult {
    pub calendars: Vec<CalendarInfo>,
}

/// The `params` of `calendar.view`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarViewParams {
    /// RFC 3339 with an offset.
    pub start: String,
    /// RFC 3339 with an offset, exclusive.
    pub end: String,
    /// The accounts to show, every configured one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub accounts: Option<Vec<String>>,
    /// The calendar ids to show, every calendar when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub calendars: Option<Vec<String>>,
    /// Show the hidden calendars' events too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub include_hidden: Option<bool>,
    /// The viewer's IANA zone, which places an all-day event's midnights; the
    /// daemon's own zone when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub zone: Option<String>,
}

/// The `result` of `calendar.view`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarViewResult {
    /// The range asked for, as UTC instants.
    pub range: CalendarRange,
    /// The zone the all-day events were placed in.
    pub zone: String,
    /// The window the server cache holds today; outside it the view answers
    /// what the cache still has.
    pub window: CalendarRange,
    /// The events, sorted by start.
    pub events: Vec<CalendarEvent>,
}

/// The `result` of `event.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventGetResult {
    pub account: String,
    pub event: CalendarEvent,
    /// The series of an occurrence, when the cache knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub series: Option<CalendarSeries>,
}

/// The `params` of `event.respond`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventRespondParams {
    pub account: String,
    /// A [`CalendarEvent::id`].
    pub event: String,
    /// `accept`, `tentative` or `decline`.
    pub response: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub comment: Option<String>,
    /// Tell the organizer; `true` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub send_response: Option<bool>,
    /// A new time to propose to the organizer, with `tentative` or
    /// `decline` on a server event whose organizer accepts proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub proposed_time: Option<EventProposedTime>,
}

/// The time an answer proposes instead of the event's, as a client types
/// it; the daemon reads it against the event's current time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventProposedTime {
    /// A time (`2026-11-18T14:00`, read in `zone`, or RFC 3339 with an
    /// offset), or a bare date (`2026-11-18`) for whole days.
    pub start: String,
    /// The same form as `start`, a bare date the last day, included.
    /// Exclusive with `duration`; neither keeps the event's length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub end: Option<String>,
    /// `1h30m` or ISO 8601 `PT1H30M`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub duration: Option<String>,
    /// The IANA zone a time without an offset is read in; the event's own
    /// zone when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub zone: Option<String>,
}

/// What a settled `event.respond` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventRespondResult {
    pub account: String,
    pub event: String,
    pub response: String,
    pub comment: Option<String>,
    pub send_response: bool,
    /// `server` when the calendar backend answered, `imip` when a reply mail
    /// went out.
    pub via: String,
    /// Whether the reply reached the organizer: `true` once the server took
    /// the answer with `send_response`, the delivery of the iMIP reply
    /// otherwise, `false` when nothing was sent.
    pub delivered: bool,
    /// The time the answer proposed, as UTC instants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub proposed_time: Option<CalendarTimeSlot>,
}

/// One person or room a new event invites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventAttendeeInput {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub name: Option<String>,
    /// `required`, `optional`, or `resource` for a room mailbox; `required`
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub role: Option<String>,
}

/// A new event as a client types it; the daemon resolves the times and
/// checks every field before an operation starts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventInputParams {
    pub summary: String,
    /// A time (`2026-11-18T14:00`, read in `zone`, or RFC 3339 with an
    /// offset), or a bare date (`2026-11-18`) for an all-day event.
    pub start: String,
    /// The same form as `start`; on an all-day event the last day,
    /// included. Exclusive with `duration`; neither on an all-day event is
    /// the one day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub end: Option<String>,
    /// `1h30m` or ISO 8601 `PT1H30M`; whole days on an all-day event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub duration: Option<String>,
    /// The IANA zone the event is held in, which reads a time without an
    /// offset and places an all-day event's midnights; the daemon's own zone
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub zone: Option<String>,
    /// Asks for an all-day event, which then needs bare dates; absent, a
    /// bare-date `start` makes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub all_day: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub location: Option<String>,
    /// The description as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub body: Option<String>,
    /// Who the server invites; none for an event that tells nobody.
    #[serde(default)]
    pub attendees: Vec<EventAttendeeInput>,
    /// Give the event an online meeting (a Teams link on Microsoft 365).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub online_meeting: Option<bool>,
    /// Minutes before the start the reminder fires: absent for the mailbox
    /// default, `null` for none.
    #[serde(default, with = "present", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Option<u32>>"))]
    pub reminder: Option<Option<u32>>,
    /// `free`, `tentative`, `busy`, `oof` or `working-elsewhere`; busy when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub show_as: Option<String>,
    /// `normal`, `personal`, `private` or `confidential`; normal when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub sensitivity: Option<String>,
    /// Whether attendees may propose a new time; yes when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub allow_new_time_proposals: Option<bool>,
    /// Repeat the event: a series by this rule. What the pattern leaves out
    /// comes from the event's first day (the weekday of a weekly rule, the
    /// day or the weekday of a monthly one, the month of a yearly one), and
    /// the range starts on that day whatever `start` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recurrence: Option<CalendarRecurrence>,
}

/// A key that is present, `null` included, as `Some`, so the three states of
/// a clearable field survive the wire: absent, `null`, and a value.
mod present {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

/// The `params` of `event.create`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventCreateParams {
    pub account: String,
    /// A [`CalendarInfo::id`] of the account; its default calendar when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub calendar: Option<String>,
    /// The id a retry of the same create sends again, so the server makes
    /// one event however often it arrives; the daemon mints one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub transaction_id: Option<String>,
    pub event: EventInputParams,
}

/// What `event.create` answers at once, before any server call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventCreateStarted {
    pub operation_id: String,
    /// The transaction id the create carries, which a retry sends again.
    pub transaction_id: String,
}

/// What a settled `event.create` reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventWriteResult {
    pub account: String,
    /// The event as the server holds it, now in the cache.
    pub event: CalendarEvent,
    pub transaction_id: String,
    /// The addresses the server sent the invitation to, rooms included;
    /// empty for an event that tells nobody.
    pub invited: Vec<String>,
}

/// The `params` of `calendar.freebusy` (PERSO-255): when the people and the
/// rooms of `addresses` are busy, as the server's scheduling assistant
/// shows it. The account's own address is always asked too, first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarFreeBusyParams {
    pub account: String,
    /// The people and room mailboxes to ask about; may be empty, which asks
    /// about the account alone.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// A bare date (`2026-11-18`), its midnight in `zone`; a time read in
    /// `zone` (`2026-11-18T07:00`); or RFC 3339 with an offset.
    pub start: String,
    /// The same forms as `start`; a bare date is the last day, included.
    pub end: String,
    /// The IANA zone a date or a time without an offset is read in; the
    /// daemon's own zone when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub zone: Option<String>,
    /// The length of one slot of a schedule's `view`, 5 to 1440 minutes;
    /// 30 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub interval_minutes: Option<u32>,
}

/// One busy block of a [`CalendarSchedule`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarScheduleItem {
    /// RFC 3339, UTC.
    pub start: String,
    /// RFC 3339, UTC, exclusive.
    pub end: String,
    /// `free`, `tentative`, `busy`, `oof`, `working-elsewhere` or `unknown`.
    pub status: String,
    /// What the block is, when the person shares their details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub location: Option<String>,
}

/// One person's or room's free/busy over the asked range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarSchedule {
    pub address: String,
    /// One digit per slot of `interval_minutes` from the range's start: `0`
    /// free, `1` tentative, `2` busy, `3` away (out of office), `4` working
    /// elsewhere, `?` unknown; empty when `error` is set.
    pub view: String,
    /// The blocks that are not free, in start order.
    pub items: Vec<CalendarScheduleItem>,
    /// Why the server has no free/busy for the address (one outside the
    /// organisation, a mailbox it does not know); the other schedules of the
    /// answer stand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub error: Option<String>,
}

/// What a settled `calendar.freebusy` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarFreeBusyResult {
    pub account: String,
    /// The range asked for, as RFC 3339 UTC instants, the end exclusive; slot
    /// `i` of a view starts `i * interval_minutes` after `start`.
    pub start: String,
    pub end: String,
    pub interval_minutes: u32,
    /// The account's own schedule first, then one per address asked, in
    /// order, each address once.
    pub schedules: Vec<CalendarSchedule>,
}

/// A change to an event as a client types it: the keys of
/// [`EventInputParams`] with patch semantics, an absent key left alone, a
/// present one changed, and `null` clearing `location`, `body` or
/// `reminder`. The daemon reads a new time against the event's current one
/// and checks every field before an operation starts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventPatchParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub summary: Option<String>,
    /// The new start, in the forms of [`EventInputParams::start`]; absent,
    /// the event's own start when `end` or `duration` moves its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub start: Option<String>,
    /// The new end; absent with no `duration`, a new start keeps the
    /// event's length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub duration: Option<String>,
    /// The IANA zone the new times are read in; the event's own zone when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub all_day: Option<bool>,
    /// `null` or an empty text clears it.
    #[serde(default, with = "present", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Option<String>>"))]
    pub location: Option<Option<String>>,
    /// The description as plain text; `null` or an empty text clears it.
    #[serde(default, with = "present", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Option<String>>"))]
    pub body: Option<Option<String>>,
    /// The whole new list, which the server turns into updates for the
    /// attendees that changed only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub attendees: Option<Vec<EventAttendeeInput>>,
    /// `true` adds an online meeting; a meeting once online stays online.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub online_meeting: Option<bool>,
    /// Minutes before the start, `null` for none.
    #[serde(default, with = "present", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<Option<u32>>"))]
    pub reminder: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub show_as: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub sensitivity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub allow_new_time_proposals: Option<bool>,
    /// The series' new rule, read as [`EventInputParams::recurrence`]
    /// against the series' first day; only with the `series` scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub recurrence: Option<CalendarRecurrence>,
}

/// The `params` of `event.update`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventUpdateParams {
    pub account: String,
    /// A [`CalendarEvent::id`] of a server event, or a series master's id.
    pub event: String,
    /// `occurrence` or `series`, which an occurrence's id needs: the change
    /// reaches that occurrence alone, or the whole series through its
    /// master. Absent on a single event; a series master's id is the
    /// series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
    pub fields: EventPatchParams,
}

/// What a settled `event.update` reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventUpdateResult {
    pub account: String,
    /// The event as the server holds it after the change, now in the cache:
    /// the occurrence for the `occurrence` scope, the series master for the
    /// `series` one.
    pub event: CalendarEvent,
    /// The scope the change was made in; absent on a single event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
}

/// The `params` of `event.cancel`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventCancelParams {
    pub account: String,
    /// A [`CalendarEvent::id`] of a meeting the user organizes, or a series
    /// master's id.
    pub event: String,
    /// `occurrence` or `series`, as on [`EventUpdateParams::scope`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
    /// A message for the attendees, sent with the cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub comment: Option<String>,
    /// The start of the one occurrence of an invitation mail's series
    /// (`imip:<row_id>`) to cancel, in the forms of
    /// [`EventInputParams::start`] read in the series' own zone, or a bare
    /// date for an all-day series; with no scope or the `occurrence` one.
    /// A server occurrence is named by its own id instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub occurrence: Option<String>,
}

/// What a settled `event.cancel` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventCancelResult {
    pub account: String,
    pub event: String,
    /// The addresses the server sent the cancellation to, rooms included.
    pub notified: Vec<String>,
    /// The scope the cancel was made in; absent on a single event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
}

/// The `params` of `event.delete`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventDeleteParams {
    pub account: String,
    /// A [`CalendarEvent::id`] of an event that tells nobody, or a series
    /// master's id.
    pub event: String,
    /// `occurrence` or `series`, as on [`EventUpdateParams::scope`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
}

/// What a settled `event.delete` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EventDeleteResult {
    pub account: String,
    pub event: String,
    /// The scope the delete was made in; absent on a single event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
}

/// The payload of a `calendar.changed` event: an account's calendars or
/// events moved (a sync, a respond, a visibility change), and a client that
/// shows them reads `calendar.list` or `calendar.view` again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CalendarChanged {
    pub account: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed fixtures of the server-calendar methods decode into
    /// their wire types whole.
    #[test]
    fn the_server_calendar_fixtures_decode() {
        let result = |raw: &str| -> serde_json::Value {
            let frame: serde_json::Value = serde_json::from_str(raw).expect("a JSON frame");
            frame["result"].clone()
        };
        let params = |raw: &str| -> serde_json::Value {
            let frame: serde_json::Value = serde_json::from_str(raw).expect("a JSON frame");
            frame["params"].clone()
        };

        let list: CalendarListResult = serde_json::from_value(result(include_str!(
            "../fixtures/calendar.list.response.json"
        )))
        .expect("calendar.list");
        assert_eq!(list.calendars.len(), 3);
        assert_eq!(list.calendars[2].id, IMIP_CALENDAR_ID);
        let _: serde_json::Value = params(include_str!("../fixtures/calendar.list.request.json"));

        let view_params: CalendarViewParams = serde_json::from_value(params(include_str!(
            "../fixtures/calendar.view.request.json"
        )))
        .expect("calendar.view params");
        assert_eq!(view_params.include_hidden, Some(false));
        let view: CalendarViewResult = serde_json::from_value(result(include_str!(
            "../fixtures/calendar.view.response.json"
        )))
        .expect("calendar.view");
        assert_eq!(view.events.len(), 2);
        assert!(matches!(
            view.events[0].time,
            CalendarEventTime::Timed { .. }
        ));
        let imip = view.events[1].source.as_ref().expect("an iMIP source");
        assert!(view.events[1].id.starts_with(IMIP_EVENT_PREFIX));
        assert_eq!(imip.kind, "imip");
        // The wire form is the daemon's: re-encoding gives the same object.
        let raw = result(include_str!("../fixtures/calendar.view.response.json"));
        assert_eq!(serde_json::to_value(&view).unwrap(), raw);

        assert!(list.calendars.iter().all(|c| c.features.is_empty()));
        let teams = &view.events[0];
        assert!(teams.is_online_meeting);
        assert_eq!(
            teams.online_meeting_provider.as_deref(),
            Some("teams-for-business")
        );
        assert_eq!(teams.kind, "exception");
        let proposal = teams.attendees[1]
            .proposed_time
            .as_ref()
            .expect("a proposal");
        assert_eq!(proposal.start, "2026-11-04T09:00:00Z");

        let raw = result(include_str!("../fixtures/event.get.response.json"));
        let get: EventGetResult = serde_json::from_value(raw.clone()).expect("event.get");
        assert!(matches!(get.event.time, CalendarEventTime::Timed { .. }));
        let series = get.series.as_ref().expect("an occurrence's series");
        let rule = series.recurrence.as_ref().expect("its rule");
        assert_eq!(rule.frequency, "weekly");
        assert_eq!(rule.weekdays, ["tuesday"]);
        assert_eq!(serde_json::to_value(&get).unwrap(), raw);
        let _: serde_json::Value = params(include_str!("../fixtures/event.get.request.json"));

        let respond: EventRespondParams = serde_json::from_value(params(include_str!(
            "../fixtures/event.respond.request.json"
        )))
        .expect("event.respond params");
        assert_eq!(respond.send_response, Some(true));
        let proposed = respond.proposed_time.expect("a proposed time");
        assert_eq!(proposed.start, "2026-11-18T15:00");
        assert_eq!(proposed.zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(proposed.duration, None);
        let started = result(include_str!("../fixtures/event.respond.response.json"));
        assert!(started["operation_id"].is_string());

        let raw = params(include_str!("../fixtures/event.create.request.json"));
        let create: EventCreateParams =
            serde_json::from_value(raw.clone()).expect("event.create params");
        assert_eq!(create.event.reminder, Some(Some(10)));
        assert_eq!(create.event.attendees[1].role.as_deref(), Some("resource"));
        let rule = create
            .event
            .recurrence
            .as_ref()
            .expect("a recurring create");
        assert_eq!(rule.weekdays, ["monday", "wednesday"]);
        assert_eq!(rule.until.as_deref(), Some("2026-12-16"));
        assert_eq!(serde_json::to_value(&create).unwrap(), raw);
        let started: EventCreateStarted = serde_json::from_value(result(include_str!(
            "../fixtures/event.create.response.json"
        )))
        .expect("event.create's answer");
        assert_eq!(started.transaction_id, create.transaction_id.unwrap());

        let raw = params(include_str!("../fixtures/event.update.request.json"));
        let update: EventUpdateParams =
            serde_json::from_value(raw.clone()).expect("event.update params");
        assert_eq!(update.fields.location, Some(None));
        assert_eq!(update.fields.reminder, Some(Some(5)));
        assert_eq!(update.fields.body, None);
        assert_eq!(update.scope.as_deref(), Some("occurrence"));
        assert_eq!(serde_json::to_value(&update).unwrap(), raw);
        let raw = params(include_str!("../fixtures/event.cancel.request.json"));
        let cancel: EventCancelParams =
            serde_json::from_value(raw.clone()).expect("event.cancel params");
        assert_eq!(cancel.scope.as_deref(), Some("series"));
        assert_eq!(serde_json::to_value(&cancel).unwrap(), raw);
        let raw = params(include_str!("../fixtures/event.delete.request.json"));
        let delete: EventDeleteParams =
            serde_json::from_value(raw.clone()).expect("event.delete params");
        assert_eq!(serde_json::to_value(&delete).unwrap(), raw);
        for answer in [
            include_str!("../fixtures/event.update.response.json"),
            include_str!("../fixtures/event.cancel.response.json"),
            include_str!("../fixtures/event.delete.response.json"),
        ] {
            assert!(result(answer)["operation_id"].is_string());
        }

        let visibility = params(include_str!(
            "../fixtures/calendar.set_visibility.request.json"
        ));
        assert_eq!(visibility["visible"], false);
        let set = result(include_str!(
            "../fixtures/calendar.set_visibility.response.json"
        ));
        assert_eq!(set["visible"], false);

        let note: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/notification.calendar_changed.json"
        ))
        .expect("a JSON frame");
        assert_eq!(note["params"]["kind"], crate::events::KIND_CALENDAR_CHANGED);
        let changed: CalendarChanged =
            serde_json::from_value(note["params"]["payload"].clone()).expect("the payload");
        assert_eq!(changed.account, "evoqs");
    }

    /// The free/busy fixtures decode whole and re-encode to the same
    /// objects: the params, and the settled schedules, an outside address
    /// with its error and no view.
    #[test]
    fn the_free_busy_fixtures_round_trip() {
        let frame =
            |raw: &str| -> serde_json::Value { serde_json::from_str(raw).expect("a JSON frame") };
        let raw =
            frame(include_str!("../fixtures/calendar.freebusy.request.json"))["params"].clone();
        let params: CalendarFreeBusyParams =
            serde_json::from_value(raw.clone()).expect("calendar.freebusy params");
        assert_eq!(params.addresses.len(), 3);
        assert_eq!(params.interval_minutes, Some(30));
        assert_eq!(serde_json::to_value(&params).unwrap(), raw);
        let started = frame(include_str!("../fixtures/calendar.freebusy.response.json"));
        assert!(started["result"]["operation_id"].is_string());

        let note = frame(include_str!(
            "../fixtures/notification.calendar_freebusy_finished.json"
        ));
        let raw = note["params"]["payload"]["result"].clone();
        let settled: CalendarFreeBusyResult =
            serde_json::from_value(raw.clone()).expect("the settled free/busy");
        assert_eq!(settled.interval_minutes, 30);
        assert_eq!(settled.schedules[0].address, "s.hellin@evoqs.com");
        assert_eq!(settled.schedules[0].view.len(), 48);
        assert_eq!(settled.schedules[1].items[1].status, "oof");
        assert_eq!(settled.schedules[1].items[1].subject, None);
        let outside = &settled.schedules[3];
        assert!(outside.view.is_empty() && outside.error.is_some());
        assert_eq!(serde_json::to_value(&settled).unwrap(), raw);
    }

    /// A new event's reminder is the mailbox default when absent, none when
    /// `null` and minutes otherwise, and each reads back as it was sent.
    #[test]
    fn an_event_input_tells_an_absent_reminder_from_a_null_one() {
        let read = |value: serde_json::Value| -> EventInputParams {
            serde_json::from_value(value).expect("an event input")
        };
        let base = serde_json::json!({"summary": "Review", "start": "2026-11-18"});
        let absent = read(base.clone());
        assert_eq!(absent.reminder, None);
        assert!(absent.attendees.is_empty());
        let mut none = base.clone();
        none["reminder"] = serde_json::Value::Null;
        assert_eq!(read(none.clone()).reminder, Some(None));
        assert_eq!(
            serde_json::to_value(read(none)).unwrap()["reminder"],
            serde_json::Value::Null
        );
        let mut minutes = base;
        minutes["reminder"] = 5.into();
        assert_eq!(read(minutes).reminder, Some(Some(5)));
    }

    /// The frontmatter block round-trips through JSON with its skip rules
    /// intact: an empty event is `{"sequence":0,"rsvp":"","recurrence":""}`
    /// and nothing else, which is what keeps a bodyless `event:` out of a
    /// message's YAML.
    #[test]
    fn an_empty_event_serialises_to_its_three_defaulted_fields() {
        let value = serde_json::to_value(EventFrontmatter::default()).expect("serialise");
        let object = value.as_object().expect("an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["recurrence", "rsvp", "sequence"]);
    }

    /// An agenda row decodes from the daemon's own rendering, keys and all.
    #[test]
    fn an_agenda_row_round_trips() {
        let row = AgendaEvent {
            row_id: 42,
            event: EventFrontmatter {
                uid: Some("uid-1".into()),
                summary: Some("Review".into()),
                attendees: vec![EventAttendee {
                    address: "ada@example.com".into(),
                    status: "accepted".into(),
                }],
                ..EventFrontmatter::default()
            },
            subject: "Invitation: Review".into(),
            start_sort: "2026-01-02T09:00:00".into(),
            end_sort: "2026-01-02T10:00:00".into(),
            start_display: "2026-01-02 10:00".into(),
            is_organizer: false,
            cancelled: false,
        };
        let json = serde_json::to_value(&row).expect("serialise");
        assert_eq!(json["row_id"], 42);
        let back: AgendaEvent = serde_json::from_value(json).expect("decode");
        assert_eq!(back, row);
    }

    /// Every key is present even when the value is empty, so a client can
    /// decode an agenda row without a `#[serde(default)]` on every field.
    #[test]
    fn an_agenda_row_carries_its_whole_key_set() {
        let value = serde_json::to_value(AgendaEvent::default()).expect("serialise");
        let object = value.as_object().expect("an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "cancelled",
                "end_sort",
                "event",
                "is_organizer",
                "row_id",
                "start_display",
                "start_sort",
                "subject",
            ]
        );
    }
}
