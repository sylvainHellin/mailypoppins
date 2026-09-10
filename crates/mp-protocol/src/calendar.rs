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

use serde::{Deserialize, Serialize};

/// A single attendee within an `event:` frontmatter block.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
