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

use chrono_tz::Tz;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};

use crate::calendar_model::{parse_zone, AttendeeRole, EventTime, Sensitivity, ShowAs};
use crate::invite::{resolve_span_in, EventSpan};
use mp_protocol::calendar::EventInputParams;

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

    /// The patch that sets every field this input sets and leaves the rest:
    /// what lands a create's fields on an event that already exists (a
    /// create replayed with its transaction id after its body changed).
    pub fn as_patch(&self) -> EventPatch {
        EventPatch {
            summary: Some(self.summary.clone()),
            time: Some(self.time.clone()),
            zone: self.zone.clone(),
            location: self.location.clone().map(Some),
            body: self.body.clone().map(Some),
            attendees: (!self.attendees.is_empty()).then(|| self.attendees.clone()),
            online_meeting: self.online_meeting.then_some(true),
            reminder: self.reminder,
            show_as: self.show_as,
            sensitivity: self.sensitivity,
            allow_new_time_proposals: self.allow_new_time_proposals,
        }
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
    let summary = params.summary.trim();
    if summary.is_empty() {
        return Err("An event needs a summary".into());
    }
    let zone = match params.zone.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => parse_zone(name).ok_or_else(|| {
            format!("unknown time zone {name:?}; use an IANA name like Europe/Berlin")
        })?,
        _ => host,
    };
    let span = resolve_span_in(
        &params.start,
        params.end.as_deref().filter(|e| !e.trim().is_empty()),
        params.duration.as_deref().filter(|d| !d.trim().is_empty()),
        zone,
    )
    .map_err(|e| format!("{e:#}"))?;
    let time = match span {
        EventSpan::Timed { start, end } => {
            if params.all_day == Some(true) {
                return Err("An all-day event takes bare dates (2026-11-18), not times".to_string());
            }
            EventTime::timed(start, end, zone.name())
        }
        EventSpan::AllDay { first, last } => {
            if params.all_day == Some(false) {
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
    .map_err(|e| e.to_string())?;

    let mut attendees: Vec<AttendeeInput> = Vec::new();
    for attendee in &params.attendees {
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
    let show_as = match params.show_as.as_deref() {
        Some(word) => Some(word_as::<ShowAs>(
            word,
            "show-as",
            "free, tentative, busy, oof, working-elsewhere",
        )?)
        .filter(|show_as| *show_as != ShowAs::Unknown),
        None => None,
    };
    let sensitivity = match params.sensitivity.as_deref() {
        Some(word) => Some(word_as(
            word,
            "the sensitivity",
            "normal, personal, private, confidential",
        )?),
        None => None,
    };
    let text = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Ok(EventInput {
        summary: summary.to_string(),
        time,
        zone: Some(zone.name().to_string()),
        location: text(&params.location),
        body: params
            .body
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            .map(str::to_string),
        attendees,
        online_meeting: params.online_meeting.unwrap_or(false),
        reminder: params.reminder,
        show_as,
        sensitivity,
        allow_new_time_proposals: params.allow_new_time_proposals,
    })
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

    /// The patch an input stands for sets what the input sets and leaves
    /// the rest, the defaults included.
    #[test]
    fn an_input_as_a_patch_sets_only_its_fields() {
        let mut input = EventInput::new("Review", time());
        input.location = Some("Room 1".into());
        let patch = input.as_patch();
        assert_eq!(patch.summary.as_deref(), Some("Review"));
        assert_eq!(patch.time, Some(time()));
        assert_eq!(patch.location, Some(Some("Room 1".into())));
        assert_eq!(patch.body, None);
        assert_eq!(patch.reminder, None);
        assert_eq!(patch.attendees, None);
        assert_eq!(patch.online_meeting, None);
    }

    fn value_with(input: &EventInput, key: &str, value: serde_json::Value) -> EventInput {
        let mut raw = serde_json::to_value(input).unwrap();
        raw[key] = value;
        serde_json::from_value(raw).unwrap()
    }
}
