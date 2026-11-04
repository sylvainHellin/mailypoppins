//! What a calendar write sends to a backend (PERSO-255,
//! `docs/plans/calendar.md`, phase 2).
//!
//! [`EventInput`] is a new event and [`EventPatch`] a change to one, both in
//! the neutral model's terms: times as an [`EventTime`] (UTC instants plus
//! the event's IANA zone, or an all-day date range with an exclusive end),
//! attendees with their role. The daemon resolves the forms a client types
//! (`resolve_span`'s `WHEN`, a duration) into these before it calls the
//! backend, and each backend turns them into its own request.
//!
//! An all-day time names dates and no zone, but a server stores its midnights
//! as instants, so a write carries the IANA zone those midnights fall in
//! (`zone`): the user's own, which makes the event cover the user's day
//! exactly. A timed time carries its zone in itself, and `zone` is unread.
//!
//! A patch changes only what it names, so a concurrent change to another
//! field in Outlook survives: an absent key stays, a present one changes, and
//! for the fields that can be emptied (location, description, reminder) a
//! JSON `null` clears.
//!
//! A create replayed with its transaction id lands on the event the first
//! try made: [`EventInput::matches`] tells whether that event already carries
//! the replay's fields, and [`EventInput::as_patch`] is the change that makes
//! it carry them when it does not.

use chrono::NaiveDate;
use chrono_tz::Tz;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};

use crate::calendar_model::{parse_zone, AttendeeRole, Event, EventTime, Sensitivity, ShowAs};
use crate::invite::{resolve_span_in, EventSpan};
use mp_protocol::calendar::{EventAttendeeInput, EventInputParams, EventPatchParams};

/// One person or room invited to an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendeeInput {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// [`AttendeeRole::Resource`] for a room mailbox.
    pub role: AttendeeRole,
}

/// A new event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInput {
    pub summary: String,
    pub time: EventTime,
    /// The IANA zone an all-day `time`'s midnights fall in; absent, the
    /// backend's host zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// The description as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Who the server invites; empty for an event that tells nobody.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<AttendeeInput>,
    /// Provision an online meeting (Teams on Microsoft 365) with its join
    /// link.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub online_meeting: bool,
    /// Minutes before the start the reminder fires: absent for the mailbox
    /// default, `null` for none.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub reminder: Option<Option<u32>>,
    /// Absent for the server's default (busy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_as: Option<ShowAs>,
    /// Absent for the server's default (normal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<Sensitivity>,
    /// Whether attendees may propose a new time; absent for the server's
    /// default, which allows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_new_time_proposals: Option<bool>,
}

impl EventInput {
    /// An event with only a summary and a time, every other field left to
    /// the server's defaults.
    pub fn new(summary: impl Into<String>, time: EventTime) -> Self {
        EventInput {
            summary: summary.into(),
            time,
            zone: None,
            location: None,
            body: None,
            attendees: Vec::new(),
            online_meeting: false,
            reminder: None,
            show_as: None,
            sensitivity: None,
            allow_new_time_proposals: None,
        }
    }

    /// The patch that makes an event that already exists match this input:
    /// what lands a create's fields on the event a first try with the same
    /// transaction id made.
    ///
    /// A field the input leaves out is cleared (location, description,
    /// attendees) or set to the server's default (busy, normal, proposals
    /// allowed), so a field the replay removed does not keep its old value.
    /// Two stay as they are, since a patch cannot say them: a reminder left
    /// to the mailbox default, and an online meeting, whose link no server
    /// takes back.
    pub fn as_patch(&self) -> EventPatch {
        EventPatch {
            summary: Some(self.summary.clone()),
            time: Some(self.time.clone()),
            zone: self.zone.clone(),
            location: Some(self.location.clone()),
            body: Some(self.body.clone()),
            attendees: Some(self.attendees.clone()),
            online_meeting: self.online_meeting.then_some(true),
            reminder: self.reminder,
            show_as: Some(self.show_as.unwrap_or(ShowAs::Busy)),
            sensitivity: Some(self.sensitivity.unwrap_or(Sensitivity::Normal)),
            allow_new_time_proposals: Some(self.allow_new_time_proposals.unwrap_or(true)),
        }
    }

    /// Whether `event` already carries what this input sets: a retried
    /// create settles with the event its transaction id made when it does,
    /// and patches it with [`EventInput::as_patch`] when it does not.
    ///
    /// A field the input leaves to the server matches the server's default
    /// (busy, normal, proposals allowed), and a mailbox-default reminder any
    /// reminder. Timed zones match by their Windows name, the one Graph
    /// keeps; an all-day event's dates are compared and its zone is not, since
    /// the cached event keeps none. Text matches with its outer whitespace
    /// trimmed and an empty text as none, and the description with every run
    /// of whitespace as one space, since the server's copy went through HTML.
    /// Attendees match by address, in any order, and by role.
    pub fn matches(&self, event: &Event) -> bool {
        fn text(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|v| !v.is_empty())
        }
        let words = |value: Option<&str>| {
            value
                .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|v| !v.is_empty())
        };
        let attendees = |list: &mut dyn Iterator<Item = (&str, AttendeeRole)>| {
            let mut keyed: Vec<(String, AttendeeRole)> = list
                .map(|(email, role)| (email.trim().to_ascii_lowercase(), role))
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            keyed
        };
        let wanted = attendees(&mut self.attendees.iter().map(|a| (a.email.as_str(), a.role)));
        let held = attendees(&mut event.attendees.iter().map(|a| (a.email.as_str(), a.role)));
        self.summary.trim() == event.summary.trim()
            && same_time(&self.time, &event.time)
            && text(self.location.as_deref()) == text(event.location.as_deref())
            && words(self.body.as_deref()) == words(event.body.as_deref())
            && wanted == held
            && self.online_meeting == event.is_online_meeting
            && self
                .reminder
                .is_none_or(|reminder| reminder == event.reminder)
            && self.show_as.unwrap_or(ShowAs::Busy) == event.show_as
            && self.sensitivity.unwrap_or(Sensitivity::Normal) == event.sensitivity
            && self.allow_new_time_proposals.unwrap_or(true) == event.allow_new_time_proposals
    }
}

/// Whether two times are the same span: the same instants in zones of one
/// Windows name, or the same dates.
fn same_time(wanted: &EventTime, held: &EventTime) -> bool {
    match (wanted, held) {
        (
            EventTime::Timed { start, end, zone },
            EventTime::Timed {
                start: held_start,
                end: held_end,
                zone: held_zone,
            },
        ) => {
            let windows = crate::windows_zones::iana_to_windows_zone;
            start == held_start
                && end == held_end
                && (zone == held_zone
                    || windows(zone).is_some_and(|name| windows(held_zone) == Some(name)))
        }
        (EventTime::AllDay { .. }, EventTime::AllDay { .. }) => wanted == held,
        _ => false,
    }
}

/// A change to an event: the keys of [`EventInput`], each `None` when the
/// patch leaves it alone. On the clearable fields, `Some(None)` (a JSON
/// `null`) clears.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The new time; switching between timed and all-day is a time too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<EventTime>,
    /// The IANA zone an all-day `time`'s midnights fall in; absent, the
    /// backend's host zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub location: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub body: Option<Option<String>>,
    /// The whole new list, which the server turns into updates for the
    /// attendees that changed only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<AttendeeInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online_meeting: Option<bool>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub reminder: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_as: Option<ShowAs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<Sensitivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_new_time_proposals: Option<bool>,
}

impl EventPatch {
    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self == &EventPatch::default()
    }
}

/// The [`EventInput`] a client's `event.create` names, or the sentence that
/// says what is wrong with it.
///
/// The times take the forms `mp send --invite` reads
/// ([`crate::invite::resolve_span`]): a time with or without an offset, read
/// in the event's zone (`params.zone`, else `host`), or a bare date for an
/// all-day event whose `end` is its last day, included. The input carries
/// the zone, which places an all-day event's midnights.
pub fn resolve_input(params: &EventInputParams, host: Tz) -> Result<EventInput, String> {
    let summary = resolve_summary(&params.summary)?;
    let zone = resolve_zone(params.zone.as_deref(), host)?;
    let time = resolve_time(
        &params.start,
        params.end.as_deref(),
        params.duration.as_deref(),
        params.all_day,
        zone,
    )?;
    let show_as = resolve_show_as(params.show_as.as_deref())?;
    let sensitivity = resolve_sensitivity(params.sensitivity.as_deref())?;
    Ok(EventInput {
        summary,
        time,
        zone: Some(zone.name().to_string()),
        location: text(params.location.as_deref()),
        body: description(params.body.as_deref()),
        attendees: resolve_attendees(&params.attendees)?,
        online_meeting: params.online_meeting.unwrap_or(false),
        reminder: params.reminder,
        show_as,
        sensitivity,
        allow_new_time_proposals: params.allow_new_time_proposals,
    })
}

/// The [`EventPatch`] a client's `event.update` names for `current`, or the
/// sentence that says what is wrong with it.
///
/// A key the client left out stays out of the patch. A new time is read
/// against the event's current one: in the event's own zone unless
/// `params.zone` names another (`host` for an all-day event), a new start
/// with no end or duration keeping the event's length, and an end or a
/// duration with no start keeping its start. `null` or an empty text clears
/// the location and the description.
pub fn resolve_patch(
    params: &EventPatchParams,
    current: &Event,
    host: Tz,
) -> Result<EventPatch, String> {
    let mut patch = EventPatch::default();
    if let Some(summary) = &params.summary {
        patch.summary = Some(resolve_summary(summary)?);
    }
    let named = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let (start, end, duration) = (
        named(&params.start),
        named(&params.end),
        named(&params.duration),
    );
    let zone_named = named(&params.zone);
    if start.is_some()
        || end.is_some()
        || duration.is_some()
        || zone_named.is_some()
        || params
            .all_day
            .is_some_and(|all_day| all_day != current.is_all_day())
    {
        let own = current.time.zone().unwrap_or(host);
        let zone = resolve_zone(zone_named.as_deref(), own)?;
        let wants_all_day = match (&start, params.all_day) {
            (_, Some(all_day)) => all_day,
            (Some(start), None) => is_date(start),
            (None, None) => current.is_all_day(),
        };
        let start = match start {
            Some(start) => start,
            None => current_start(&current.time, own, wants_all_day),
        };
        let keeps_length = end.is_none() && duration.is_none();
        let duration = match duration {
            Some(duration) => Some(duration),
            None if keeps_length && wants_all_day == current.is_all_day() => {
                Some(current_length(&current.time))
            }
            None => None,
        };
        patch.time = Some(resolve_time(
            &start,
            end.as_deref(),
            duration.as_deref(),
            Some(wants_all_day),
            zone,
        )?);
        patch.zone = Some(zone.name().to_string());
    }
    if let Some(location) = &params.location {
        patch.location = Some(text(location.as_deref()));
    }
    if let Some(body) = &params.body {
        patch.body = Some(description(body.as_deref()));
    }
    if let Some(attendees) = &params.attendees {
        patch.attendees = Some(resolve_attendees(attendees)?);
    }
    patch.online_meeting = params.online_meeting;
    patch.reminder = params.reminder;
    if params.show_as.is_some() {
        patch.show_as = Some(
            resolve_show_as(params.show_as.as_deref())?
                .ok_or_else(|| "show-as \"unknown\" cannot be set".to_string())?,
        );
    }
    patch.sensitivity = resolve_sensitivity(params.sensitivity.as_deref())?;
    patch.allow_new_time_proposals = params.allow_new_time_proposals;
    Ok(patch)
}

/// Whether `value` is a bare date (`2026-11-18`), the form of an all-day
/// time.
fn is_date(value: &str) -> bool {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").is_ok()
}

/// The current start of `time` in the form a new time is read in: its wall
/// time in `zone` (or its date, for an all-day time asked for), else its
/// first day.
fn current_start(time: &EventTime, zone: Tz, all_day: bool) -> String {
    match time {
        EventTime::Timed { start, .. } => {
            let local = start.with_timezone(&zone).naive_local();
            if all_day {
                local.date().to_string()
            } else {
                local.format("%Y-%m-%dT%H:%M:%S").to_string()
            }
        }
        EventTime::AllDay { start, .. } => start.to_string(),
    }
}

/// The length of `time` as a duration a new time takes: seconds for a
/// timed time, whole days for an all-day one.
fn current_length(time: &EventTime) -> String {
    match time {
        EventTime::Timed { start, end, .. } => {
            format!("PT{}S", (*end - *start).num_seconds().max(60))
        }
        EventTime::AllDay { start, end } => {
            format!("P{}D", (*end - *start).num_days().max(1))
        }
    }
}

fn resolve_summary(summary: &str) -> Result<String, String> {
    let summary = summary.trim();
    if summary.is_empty() {
        return Err("An event needs a summary".into());
    }
    Ok(summary.to_string())
}

/// The zone `name` names, else `fallback`.
fn resolve_zone(name: Option<&str>, fallback: Tz) -> Result<Tz, String> {
    match name.map(str::trim) {
        Some(name) if !name.is_empty() => parse_zone(name).ok_or_else(|| {
            format!("unknown time zone {name:?}; use an IANA name like Europe/Berlin")
        }),
        _ => Ok(fallback),
    }
}

/// The time a start, an end or a duration name in `zone`; `all_day`
/// insists on the form.
fn resolve_time(
    start: &str,
    end: Option<&str>,
    duration: Option<&str>,
    all_day: Option<bool>,
    zone: Tz,
) -> Result<EventTime, String> {
    let span = resolve_span_in(
        start,
        end.filter(|e| !e.trim().is_empty()),
        duration.filter(|d| !d.trim().is_empty()),
        zone,
    )
    .map_err(|e| format!("{e:#}"))?;
    match span {
        EventSpan::Timed { start, end } => {
            if all_day == Some(true) {
                return Err("An all-day event takes bare dates (2026-11-18), not times".to_string());
            }
            EventTime::timed(start, end, zone.name())
        }
        EventSpan::AllDay { first, last } => {
            if all_day == Some(false) {
                return Err(
                    "A bare date makes an all-day event; give a time (2026-11-18T14:00) for a \
                     timed one"
                        .to_string(),
                );
            }
            let end = last
                .succ_opt()
                .ok_or_else(|| "the last day runs past the calendar".to_string())?;
            EventTime::all_day(first, end)
        }
    }
    .map_err(|e| e.to_string())
}

/// The attendees, each address once, with their role word, required by
/// default.
fn resolve_attendees(listed: &[EventAttendeeInput]) -> Result<Vec<AttendeeInput>, String> {
    let mut attendees: Vec<AttendeeInput> = Vec::new();
    for attendee in listed {
        let email = attendee.email.trim();
        if !email.contains('@') || email.contains(char::is_whitespace) {
            return Err(format!("{email:?} is not an email address"));
        }
        let role = match attendee.role.as_deref().map(str::trim) {
            None | Some("") => AttendeeRole::Required,
            Some(word) => word_as(word, "an attendee's role", "required, optional, resource")?,
        };
        if attendees
            .iter()
            .any(|known| known.email.eq_ignore_ascii_case(email))
        {
            continue;
        }
        attendees.push(AttendeeInput {
            email: email.to_string(),
            name: attendee
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_string),
            role,
        });
    }
    Ok(attendees)
}

fn resolve_show_as(word: Option<&str>) -> Result<Option<ShowAs>, String> {
    Ok(match word {
        Some(word) => Some(word_as::<ShowAs>(
            word,
            "show-as",
            "free, tentative, busy, oof, working-elsewhere",
        )?)
        .filter(|show_as| *show_as != ShowAs::Unknown),
        None => None,
    })
}

fn resolve_sensitivity(word: Option<&str>) -> Result<Option<Sensitivity>, String> {
    match word {
        Some(word) => Some(word_as(
            word,
            "the sensitivity",
            "normal, personal, private, confidential",
        ))
        .transpose(),
        None => Ok(None),
    }
}

/// A text trimmed, an empty one as none.
fn text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// A description as typed, a blank one as none.
fn description(value: Option<&str>) -> Option<String> {
    value.filter(|b| !b.trim().is_empty()).map(str::to_string)
}

/// One of a model enum's words, or a sentence naming the ones it takes.
fn word_as<T: DeserializeOwned>(word: &str, what: &str, words: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(word.trim().to_string()))
        .map_err(|_| format!("{word:?} is not {what}; use one of {words}"))
}

/// A key that is present, `null` included, as `Some`: with `default`, an
/// absent key stays `None`, so the three states of a clearable field survive
/// the wire.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn time() -> EventTime {
        EventTime::timed(
            Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap(),
            "Europe/Berlin",
        )
        .unwrap()
    }

    /// Absent, `null` and a value are three different patches of a clearable
    /// field, and each serialises back to what it was read from.
    #[test]
    fn a_patch_tells_an_absent_key_from_a_null_one() {
        let absent: EventPatch = serde_json::from_value(json!({})).unwrap();
        assert!(absent.is_empty());
        assert_eq!(serde_json::to_value(&absent).unwrap(), json!({}));

        let cleared: EventPatch =
            serde_json::from_value(json!({"location": null, "reminder": null})).unwrap();
        assert_eq!(cleared.location, Some(None));
        assert_eq!(cleared.reminder, Some(None));
        assert_eq!(cleared.body, None);
        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            json!({"location": null, "reminder": null})
        );

        let set: EventPatch =
            serde_json::from_value(json!({"location": "Room 2", "reminder": 10})).unwrap();
        assert_eq!(set.location, Some(Some("Room 2".into())));
        assert_eq!(set.reminder, Some(Some(10)));
    }

    /// A new event's reminder is the mailbox default when absent and none
    /// when `null`; the defaults stay off the wire.
    #[test]
    fn an_input_round_trips_with_its_defaults_left_out() {
        let input = EventInput::new("Review", time());
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(
            value,
            json!({
                "summary": "Review",
                "time": {
                    "kind": "timed",
                    "start": "2026-11-03T09:00:00Z",
                    "end": "2026-11-03T10:00:00Z",
                    "zone": "Europe/Berlin",
                },
            })
        );
        assert_eq!(serde_json::from_value::<EventInput>(value).unwrap(), input);

        let mut no_reminder = value_with(&input, "reminder", json!(null));
        assert_eq!(no_reminder.reminder, Some(None));
        no_reminder.attendees.push(AttendeeInput {
            email: "room@example.com".into(),
            name: None,
            role: AttendeeRole::Resource,
        });
        let back = serde_json::to_value(&no_reminder).unwrap();
        assert_eq!(back["reminder"], json!(null));
        assert_eq!(
            back["attendees"],
            json!([{"email": "room@example.com", "role": "resource"}])
        );
    }

    fn params(start: &str) -> EventInputParams {
        EventInputParams {
            summary: " Review ".into(),
            start: start.into(),
            ..Default::default()
        }
    }

    /// A wall-clock time is read in the event's zone, else in the host's,
    /// and the input carries that zone; an offset wins over both.
    #[test]
    fn a_timed_input_is_read_in_its_zone() {
        let mut berlin = params("2026-11-03T10:00");
        berlin.duration = Some("1h".into());
        berlin.zone = Some("Europe/Berlin".into());
        let input = resolve_input(&berlin, chrono_tz::UTC).unwrap();
        assert_eq!(input.summary, "Review");
        assert_eq!(input.time, time());
        assert_eq!(input.zone.as_deref(), Some("Europe/Berlin"));

        let mut host = params("2026-11-03T10:00");
        host.end = Some("2026-11-03T11:00".into());
        let input = resolve_input(&host, chrono_tz::Europe::Berlin).unwrap();
        assert_eq!(input.time, time());

        let mut offset = params("2026-11-03T09:00:00Z");
        offset.duration = Some("PT1H".into());
        offset.zone = Some("Europe/Berlin".into());
        assert_eq!(resolve_input(&offset, chrono_tz::UTC).unwrap().time, time());
    }

    /// A bare date is an all-day event whose `end` is its last day, the
    /// model's end the day after; `all_day` insists on the form.
    #[test]
    fn a_bare_date_is_an_all_day_input() {
        let mut holiday = params("2026-11-18");
        holiday.end = Some("2026-11-19".into());
        holiday.zone = Some("America/New_York".into());
        let input = resolve_input(&holiday, chrono_tz::UTC).unwrap();
        let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(input.time, EventTime::all_day(day(18), day(20)).unwrap());
        assert_eq!(input.zone.as_deref(), Some("America/New_York"));
        let mut one = params("2026-11-18");
        one.all_day = Some(true);
        assert_eq!(
            resolve_input(&one, chrono_tz::UTC).unwrap().time,
            EventTime::all_day(day(18), day(19)).unwrap()
        );

        let mut timed = params("2026-11-18T09:00");
        timed.duration = Some("1h".into());
        timed.all_day = Some(true);
        assert!(resolve_input(&timed, chrono_tz::UTC).is_err());
        one.all_day = Some(false);
        assert!(resolve_input(&one, chrono_tz::UTC).is_err());
    }

    /// Attendees take their role word, required by default, once per
    /// address; a field the input names in a word outside its set is
    /// refused with the words it takes, as is an empty summary or an
    /// unknown zone.
    #[test]
    fn an_input_checks_its_words_and_addresses() {
        let attendee =
            |email: &str, role: Option<&str>| mp_protocol::calendar::EventAttendeeInput {
                email: email.into(),
                name: None,
                role: role.map(str::to_string),
            };
        let mut meeting = params("2026-11-18");
        meeting.attendees = vec![
            attendee("ada@example.com", None),
            attendee("ADA@example.com", Some("optional")),
            attendee("room-1@example.com", Some("resource")),
        ];
        meeting.show_as = Some("working-elsewhere".into());
        meeting.reminder = Some(None);
        meeting.location = Some("  ".into());
        let input = resolve_input(&meeting, chrono_tz::UTC).unwrap();
        let roles: Vec<AttendeeRole> = input.attendees.iter().map(|a| a.role).collect();
        assert_eq!(roles, [AttendeeRole::Required, AttendeeRole::Resource]);
        assert_eq!(input.show_as, Some(ShowAs::WorkingElsewhere));
        assert_eq!(input.reminder, Some(None));
        assert_eq!(input.location, None);

        let refused = |change: &dyn Fn(&mut EventInputParams)| {
            let mut bad = params("2026-11-18");
            change(&mut bad);
            resolve_input(&bad, chrono_tz::UTC).unwrap_err()
        };
        assert_eq!(
            refused(&|p| p.summary = " ".into()),
            "An event needs a summary"
        );
        assert!(refused(&|p| p.zone = Some("Mars/Base".into())).contains("unknown time zone"));
        assert!(refused(&|p| p.show_as = Some("away".into())).contains("free, tentative"));
        assert!(refused(&|p| p.attendees = vec![attendee("ada", None)]).contains("not an email"));
        assert!(
            refused(&|p| p.attendees = vec![attendee("a@b.c", Some("chair"))])
                .contains("required, optional, resource")
        );
    }

    /// The patch an input stands for sets what the input sets, clears what
    /// it leaves out and puts the server's defaults back, so a replay that
    /// removed a field removes it on the event; a mailbox-default reminder
    /// and a meeting the input does not ask for stay off the patch.
    #[test]
    fn an_input_as_a_patch_makes_the_event_match_it() {
        let mut input = EventInput::new("Review", time());
        input.location = Some("Room 1".into());
        let patch = input.as_patch();
        assert_eq!(patch.summary.as_deref(), Some("Review"));
        assert_eq!(patch.time, Some(time()));
        assert_eq!(patch.location, Some(Some("Room 1".into())));
        assert_eq!(patch.body, Some(None));
        assert_eq!(patch.attendees, Some(Vec::new()));
        assert_eq!(patch.show_as, Some(ShowAs::Busy));
        assert_eq!(patch.sensitivity, Some(Sensitivity::Normal));
        assert_eq!(patch.allow_new_time_proposals, Some(true));
        assert_eq!(patch.reminder, None);
        assert_eq!(patch.online_meeting, None);

        input.reminder = Some(None);
        input.online_meeting = true;
        let patch = input.as_patch();
        assert_eq!(patch.reminder, Some(None));
        assert_eq!(patch.online_meeting, Some(true));
    }

    /// The event a create made, in the server's terms.
    fn made(input: &EventInput) -> Event {
        let mut event: Event = serde_json::from_value(json!({
            "account": "evoqs",
            "calendar": "cal",
            "id": "ev-1",
            "uid": "UID-1",
            "summary": input.summary,
            "time": input.time,
            "own_response": "organizer",
            "sensitivity": "normal",
            "show_as": "busy",
            "reminder": 15,
        }))
        .unwrap();
        event.location = input.location.clone();
        event.body = input.body.clone();
        event.attendees = input
            .attendees
            .iter()
            .map(|a| crate::calendar_model::Attendee {
                name: None,
                email: a.email.clone(),
                role: a.role,
                status: crate::calendar_model::ResponseStatus::NeedsAction,
                proposed_time: None,
            })
            .collect();
        event
    }

    /// The event a create made matches the create, its defaults (a reminder
    /// left to the mailbox's matches the 15 minutes it got), a zone of the
    /// same Windows name and a description whose lines the server
    /// rewrapped included; each field a replay changed is a mismatch.
    #[test]
    fn an_input_matches_the_event_it_made_until_a_field_changes() {
        let mut input = EventInput::new("Review", time());
        input.location = Some("Room 1".into());
        input.body = Some("Agenda:\nbudget".into());
        input.attendees = vec![
            AttendeeInput {
                email: "ada@example.com".into(),
                name: Some("Ada".into()),
                role: AttendeeRole::Required,
            },
            AttendeeInput {
                email: "room@example.com".into(),
                name: None,
                role: AttendeeRole::Resource,
            },
        ];
        let mut event = made(&input);
        assert!(input.matches(&event));
        event.body = Some("Agenda:\r\n budget\n".into());
        event.attendees.reverse();
        event.attendees[0].email = "ROOM@example.com".into();
        event.time = EventTime::timed(
            Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap(),
            "Europe/Amsterdam",
        )
        .unwrap();
        assert!(input.matches(&event), "the server's spellings match");

        let changed: [&dyn Fn(&mut EventInput); 9] = [
            &|i| i.summary = "Review, moved".into(),
            &|i| {
                i.time = EventTime::all_day(
                    chrono::NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
                    chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
                )
                .unwrap()
            },
            &|i| i.location = None,
            &|i| i.body = None,
            &|i| i.attendees[1].role = AttendeeRole::Optional,
            &|i| i.attendees.truncate(1),
            &|i| i.reminder = Some(Some(5)),
            &|i| i.online_meeting = true,
            &|i| i.show_as = Some(ShowAs::Free),
        ];
        for change in changed {
            let mut replay = input.clone();
            change(&mut replay);
            assert!(!replay.matches(&event), "{replay:?} matched");
        }
        let mut other_zone = input.clone();
        other_zone.time = EventTime::timed(
            Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 3, 10, 0, 0).unwrap(),
            "Europe/London",
        )
        .unwrap();
        assert!(!other_zone.matches(&event));
    }

    /// A patch names only what the client sent: a new start keeps the
    /// event's length and is read in the event's own zone, an end alone
    /// keeps the start, a zone alone keeps the wall time, and `null` or an
    /// empty text clears; nothing sent is an empty patch.
    #[test]
    fn a_patch_reads_a_new_time_against_the_current_one() {
        let mut current = made(&EventInput::new("Review", time()));
        current.location = Some("Room 1".into());
        let patch = |value: serde_json::Value| {
            let params: EventPatchParams = serde_json::from_value(value).unwrap();
            resolve_patch(&params, &current, chrono_tz::UTC)
        };
        assert!(patch(json!({})).unwrap().is_empty());

        let at = |h, m| Utc.with_ymd_and_hms(2026, 11, 3, h, m, 0).unwrap();
        let moved = patch(json!({"start": "2026-11-03T15:30"})).unwrap();
        assert_eq!(
            moved.time,
            Some(EventTime::timed(at(14, 30), at(15, 30), "Europe/Berlin").unwrap())
        );
        assert_eq!(moved.zone.as_deref(), Some("Europe/Berlin"));
        assert_eq!(moved.summary, None);
        assert_eq!(moved.location, None);

        let longer = patch(json!({"end": "2026-11-03T12:00"})).unwrap();
        assert_eq!(
            longer.time,
            Some(EventTime::timed(at(9, 0), at(11, 0), "Europe/Berlin").unwrap())
        );
        let shorter = patch(json!({"duration": "30m"})).unwrap();
        assert_eq!(
            shorter.time,
            Some(EventTime::timed(at(9, 0), at(9, 30), "Europe/Berlin").unwrap())
        );
        let elsewhere = patch(json!({"zone": "Europe/London"})).unwrap();
        assert_eq!(
            elsewhere.time,
            Some(EventTime::timed(at(10, 0), at(11, 0), "Europe/London").unwrap())
        );

        let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        let all_day = patch(json!({"all_day": true})).unwrap();
        assert_eq!(
            all_day.time,
            Some(EventTime::all_day(day(3), day(4)).unwrap())
        );
        assert!(patch(json!({"start": "2026-11-05", "all_day": false})).is_err());

        let cleared = patch(json!({"location": null, "body": " ", "reminder": null})).unwrap();
        assert_eq!(cleared.location, Some(None));
        assert_eq!(cleared.body, Some(None));
        assert_eq!(cleared.reminder, Some(None));
        assert_eq!(cleared.time, None);

        assert_eq!(
            patch(json!({"summary": "  "})).unwrap_err(),
            "An event needs a summary"
        );
        assert!(patch(json!({"attendees": [{"email": "ada"}]}))
            .unwrap_err()
            .contains("not an email"));
        assert!(patch(json!({"show_as": "unknown"})).is_err());

        let mut holiday = current.clone();
        holiday.time = EventTime::all_day(day(5), day(7)).unwrap();
        let params: EventPatchParams =
            serde_json::from_value(json!({"start": "2026-11-10"})).unwrap();
        let shifted = resolve_patch(&params, &holiday, chrono_tz::UTC).unwrap();
        assert_eq!(
            shifted.time,
            Some(EventTime::all_day(day(10), day(12)).unwrap())
        );
    }

    fn value_with(input: &EventInput, key: &str, value: serde_json::Value) -> EventInput {
        let mut raw = serde_json::to_value(input).unwrap();
        raw[key] = value;
        serde_json::from_value(raw).unwrap()
    }
}
