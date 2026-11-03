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
//! A patch changes only what it names, so a concurrent change to another
//! field in Outlook survives: an absent key stays, a present one changes, and
//! for the fields that can be emptied (location, description, reminder) a
//! JSON `null` clears.

use serde::{Deserialize, Deserializer, Serialize};

use crate::calendar_model::{AttendeeRole, EventTime, Sensitivity, ShowAs};

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

    fn value_with(input: &EventInput, key: &str, value: serde_json::Value) -> EventInput {
        let mut raw = serde_json::to_value(input).unwrap();
        raw[key] = value;
        serde_json::from_value(raw).unwrap()
    }
}
