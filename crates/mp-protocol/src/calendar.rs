//! The `calendar.*` and invite-shaped wire types (P5-U10).
//!
//! [`EventFrontmatter`] and [`EventAttendee`] were `mailypoppins::types`'
//! until this unit; they are pure serde structs with no engine dependency, and
//! both ends of the socket now need them, so they live here and the root crate
//! re-exports them under their old paths. Nothing about the YAML frontmatter
//! they also serialise changed: the field names, the `#[serde]` attributes and
//! the skip rules are the ones `docs/plans/calendar-invites.md` D2 fixed.
//!
//! [`AgendaEvent`] is new, and is the wire form of the TUI's Calendar row: one
//! `messages.id`, the frontmatter block the shared event card renders, and the
//! five derived columns the agenda list sorts and paints by.
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
/// block is a render/query cache (see `docs/plans/calendar-invites.md`, D2).
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

/// One row of `calendar.events`: an agenda entry, already deduped, folded and
/// sorted by the daemon.
///
/// The derived columns travel rather than being recomputed client-side because
/// every one of them is a fold over the account's *other* rows (the reply
/// statuses, the cancellation chain, the sent-copy tiebreak), which is exactly
/// what a client without a store cannot do.
///
/// No field is skipped when empty: an agenda row is one object per event and a
/// client decodes it whole, so a stable key set is worth more than the bytes.
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
    /// The `RRULE` of a series master; on an invitation mail's event, the
    /// human-readable summary the mail's agenda row carried.
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
    /// The invitation mail an `imip:` event was derived from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<CalendarEventSource>,
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

        let get: EventGetResult =
            serde_json::from_value(result(include_str!("../fixtures/event.get.response.json")))
                .expect("event.get");
        assert!(matches!(get.event.time, CalendarEventTime::AllDay { .. }));
        let _: serde_json::Value = params(include_str!("../fixtures/event.get.request.json"));

        let respond: EventRespondParams = serde_json::from_value(params(include_str!(
            "../fixtures/event.respond.request.json"
        )))
        .expect("event.respond params");
        assert_eq!(respond.send_response, Some(false));
        let started = result(include_str!("../fixtures/event.respond.response.json"));
        assert!(started["operation_id"].is_string());

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
