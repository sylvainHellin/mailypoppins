//! The merged calendar view (PERSO-200, `docs/plans/calendar.md`, D7 and
//! "Invitations on an account with a backend"): the events `calendar.view`
//! and `event.get` serve.
//!
//! Two sources meet here. An account with a calendar backend has its server
//! events in the store cache ([`crate::store::calendar`]); every account has
//! the iMIP agenda of its invitation mails ([`crate::agenda`]), which
//! [`imip_event`] converts into the neutral [`Event`] under the virtual
//! calendar [`IMIP_CALENDAR_ID`], with the mail as its [`CalendarEventSource`].
//!
//! An invitation mail and the server copy of its event share a `UID` and a
//! `RECURRENCE-ID`, so an iMIP event whose [`EventKey`] any cached server event
//! of the accounts in view carries is dropped: the server copy wins, wherever
//! it sits in time (a rescheduled event leaves its older invitation behind),
//! and whether or not its calendar is shown, so hiding a calendar never brings
//! the mail's copy of its events back. Server events are never deduplicated
//! against one another: each is an entry in a calendar of its own.

use std::collections::HashSet;
use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use log::warn;
use mp_protocol::calendar::{
    AgendaEvent, CalendarEvent, CalendarEventSource, IMIP_CALENDAR_ID, IMIP_EVENT_PREFIX,
};

use crate::calendar_backend::has_backend;
use crate::calendar_model::{
    Attendee, AttendeeRole, CalendarWindow, Event, EventKey, EventTime, OwnResponse, Person,
    ResponseStatus, Sensitivity, ShowAs,
};
use crate::config::AccountConfig;
use crate::store::{calendar as cache, BlobStore, Store};

/// The name `calendar.list` gives the virtual invitation calendar.
pub const IMIP_CALENDAR_NAME: &str = "Invitations";

/// The `kind` of a [`CalendarEventSource`] naming an invitation mail.
pub const IMIP_SOURCE_KIND: &str = "imip";

/// One event with where it came from: `None` for a server event.
#[derive(Debug, Clone, PartialEq)]
pub struct Sourced {
    pub event: Event,
    pub source: Option<CalendarEventSource>,
}

impl Sourced {
    /// The wire form: the model's own serialisation, plus the source.
    pub fn to_wire(&self) -> Result<CalendarEvent> {
        let mut value = serde_json::to_value(&self.event).context("serialising an event")?;
        if let (Some(source), Some(object)) = (&self.source, value.as_object_mut()) {
            object.insert(
                "source".into(),
                serde_json::to_value(source).context("serialising an event source")?,
            );
        }
        serde_json::from_value(value).context("an event in its wire form")
    }
}

/// What a view shows besides its range.
#[derive(Debug, Clone, Default)]
pub struct ViewFilter {
    /// Only these calendar ids, when set.
    pub calendars: Option<Vec<String>>,
    /// The hidden calendars' events too.
    pub include_hidden: bool,
}

/// The daemon's own zone: `TZ` when it names an IANA zone, else the zone
/// `/etc/localtime` links to, else UTC.
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

/// The event id of the invitation mail `row_id`.
pub fn imip_event_id(row_id: i64) -> String {
    format!("{IMIP_EVENT_PREFIX}{row_id}")
}

/// The `messages.id` an `imip:` event id names; `None` for a server id.
pub fn imip_row_id(event_id: &str) -> Option<i64> {
    event_id.strip_prefix(IMIP_EVENT_PREFIX)?.parse().ok()
}

/// The events of `accounts` in `range`, seen from `viewer`, merged, deduped and
/// sorted by start (all-day first on a day, then by summary and id).
///
/// The server events are what the cache holds: a range beyond the cache
/// window answers whatever the cache still has there, which is nothing once a
/// sync has moved the window past it.
pub fn view(
    accounts: &[&AccountConfig],
    range: CalendarWindow,
    viewer: Tz,
    filter: &ViewFilter,
) -> Result<Vec<Sourced>> {
    let mut server: Vec<(Sourced, bool)> = Vec::new();
    let mut imip: Vec<(Sourced, bool)> = Vec::new();
    let mut taken: HashSet<EventKey> = HashSet::new();
    for account in accounts {
        let name = account.name.as_str();
        let store = open_store(name)?;
        if has_backend(account) {
            for key in cache::event_keys(&store, name)? {
                // A series invitation carries no RECURRENCE-ID, and the cache
                // holds a series' occurrences but never its master: any
                // occurrence of the UID takes the series' key too.
                if key.recurrence_id.is_some() {
                    taken.insert(EventKey::new(&key.uid, None));
                }
                taken.insert(key);
            }
            let visible: Vec<(String, bool)> = cache::list_calendars(&store, name)?
                .into_iter()
                .map(|calendar| (calendar.id, calendar.visible))
                .collect();
            for event in cache::events_in_range(&store, &[name], range, viewer, false)? {
                let shown = visible
                    .iter()
                    .find(|(id, _)| *id == event.calendar)
                    .is_none_or(|(_, shown)| *shown);
                server.push((
                    Sourced {
                        event,
                        source: None,
                    },
                    shown,
                ));
            }
        }
        let shown = cache::calendar_visible(&store, name, IMIP_CALENDAR_ID)?;
        for sourced in imip_events(&store, account, viewer) {
            if sourced.event.overlaps(&range, viewer) {
                imip.push((sourced, shown));
            }
        }
    }

    let mut merged = server;
    for (sourced, shown) in imip {
        // An invitation with no UID cannot be matched, and keeps its row.
        let dedupable = !sourced.event.uid.starts_with("row:");
        if dedupable && !taken.insert(sourced.event.dedup_key()) {
            continue;
        }
        merged.push((sourced, shown));
    }

    let mut events: Vec<Sourced> = merged
        .into_iter()
        .filter(|(_, shown)| filter.include_hidden || *shown)
        .map(|(sourced, _)| sourced)
        .filter(|sourced| {
            filter
                .calendars
                .as_ref()
                .is_none_or(|ids| ids.contains(&sourced.event.calendar))
        })
        .collect();
    events.sort_by(|a, b| {
        let key = |e: &Event| (e.utc_interval(viewer).0, !e.is_all_day());
        key(&a.event)
            .cmp(&key(&b.event))
            .then_with(|| a.event.summary.cmp(&b.event.summary))
            .then_with(|| a.event.account.cmp(&b.event.account))
            .then_with(|| a.event.id.cmp(&b.event.id))
    });
    Ok(events)
}

/// One event of `account` by its id: a cached server event, or the event of
/// an invitation mail (`imip:<row_id>`). `None` when neither knows it.
pub fn get(account: &AccountConfig, id: &str, viewer: Tz) -> Result<Option<Sourced>> {
    let store = open_store(&account.name)?;
    if let Some(row_id) = imip_row_id(id) {
        return Ok(imip_events(&store, account, viewer)
            .into_iter()
            .find(|sourced| sourced.source.as_ref().map(|s| s.row_id) == Some(row_id)));
    }
    Ok(
        cache::get_event(&store, &account.name, id)?.map(|event| Sourced {
            event,
            source: None,
        }),
    )
}

/// Whether `account` has any invitation mail, which gives it the virtual
/// invitation calendar in `calendar.list`.
pub fn has_invitations(store: &Store, account: &str) -> bool {
    crate::store::read::list_invites(store, account).is_ok_and(|rows| !rows.is_empty())
}

fn open_store(account: &str) -> Result<Store> {
    Store::open(crate::config::store_path(account))
        .with_context(|| format!("opening the store of {account}"))
}

/// The iMIP agenda of `account`, each row converted and sourced. A row with
/// no usable start is left out: a view has nowhere to place it.
pub fn imip_events(store: &Store, account: &AccountConfig, viewer: Tz) -> Vec<Sourced> {
    let blobs = BlobStore::for_account(&account.name);
    let self_address = crate::parse::extract_email_address(&account.default_from);
    crate::agenda::load_events_for_account(store, &blobs, &account.name, &self_address)
        .iter()
        .filter_map(|row| {
            let event = imip_event(row, &account.name, viewer)?;
            let selector = match crate::store::read::find_by_id(store, row.row_id) {
                Ok(Some(message)) => {
                    crate::selector::Selector::for_message(&account.name, &message).to_string()
                }
                Ok(None) => String::new(),
                Err(err) => {
                    warn!(
                        "[calendar] {}: reading the invitation row {}: {err:#}",
                        account.name, row.row_id
                    );
                    String::new()
                }
            };
            Some(Sourced {
                event,
                source: Some(CalendarEventSource {
                    kind: IMIP_SOURCE_KIND.into(),
                    row_id: row.row_id,
                    selector,
                    subject: row.subject.clone(),
                }),
            })
        })
        .collect()
}

/// One agenda row as a neutral event of the virtual invitation calendar.
///
/// The agenda's start and end strings come in three forms
/// (`crate::calendar::format_date_perhaps_time`): RFC 3339 with an offset,
/// whose zone the mail named but the row no longer carries, so the event is
/// labelled UTC with exact instants; an offset-less wall-clock time, a
/// floating time read in `viewer`'s zone; and a midnight or a bare date, an
/// all-day event whose end is already exclusive (a missing or empty end makes
/// it one day long). `None` when the start is missing or unreadable.
pub fn imip_event(row: &AgendaEvent, account: &str, viewer: Tz) -> Option<Event> {
    let fm = &row.event;
    let start = parse_stamp(fm.start.as_deref()?)?;
    let end = fm.end.as_deref().and_then(parse_stamp);
    let time = match start {
        Stamp::Date(start) => {
            let end = match end {
                Some(Stamp::Date(end)) if end > start => end,
                _ => start.succ_opt()?,
            };
            EventTime::all_day(start, end).ok()?
        }
        Stamp::Instant(start) => {
            let end = end
                .and_then(|end| end.instant(viewer))
                .filter(|end| *end >= start)
                .unwrap_or(start);
            EventTime::timed(start, end, "UTC").ok()?
        }
        Stamp::Floating(local) => {
            let start = Stamp::Floating(local).instant(viewer)?;
            let end = end
                .and_then(|end| end.instant(viewer))
                .filter(|end| *end >= start)
                .unwrap_or(start);
            EventTime::timed(start, end, viewer.name()).ok()?
        }
    };

    let own_response = if row.is_organizer {
        OwnResponse::Organizer
    } else {
        match fm.rsvp.as_str() {
            "accepted" => OwnResponse::Accepted,
            "tentative" => OwnResponse::Tentative,
            "declined" => OwnResponse::Declined,
            _ => OwnResponse::NeedsAction,
        }
    };
    let show_as = match own_response {
        OwnResponse::Declined => ShowAs::Free,
        OwnResponse::Tentative => ShowAs::Tentative,
        _ => ShowAs::Busy,
    };
    let summary = fm
        .summary
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| row.subject.clone());
    Some(Event {
        account: account.to_string(),
        calendar: IMIP_CALENDAR_ID.to_string(),
        id: imip_event_id(row.row_id),
        uid: fm
            .uid
            .clone()
            .filter(|uid| !uid.trim().is_empty())
            .unwrap_or_else(|| format!("row:{}", row.row_id)),
        recurrence_id: fm.recurrence_id.clone(),
        series_master_id: None,
        summary,
        time,
        location: fm.location.clone().filter(|l| !l.is_empty()),
        body: None,
        organizer: fm
            .organizer
            .as_deref()
            .map(strip_mailto)
            .filter(|email| !email.is_empty())
            .map(|email| Person {
                name: None,
                email: email.to_string(),
            }),
        attendees: fm
            .attendees
            .iter()
            .map(|attendee| Attendee {
                name: None,
                email: strip_mailto(&attendee.address).to_string(),
                role: AttendeeRole::Required,
                status: match attendee.status.as_str() {
                    "accepted" => ResponseStatus::Accepted,
                    "tentative" => ResponseStatus::Tentative,
                    "declined" => ResponseStatus::Declined,
                    _ => ResponseStatus::NeedsAction,
                },
            })
            .collect(),
        own_response,
        recurrence: Some(fm.recurrence.clone()).filter(|r| !r.is_empty()),
        reminder: None,
        online_meeting_url: None,
        sensitivity: Sensitivity::Normal,
        show_as,
        cancelled: row.cancelled,
        is_organizer: row.is_organizer,
        last_modified: None,
        web_link: None,
    })
}

fn strip_mailto(address: &str) -> &str {
    let trimmed = address.trim();
    trimmed
        .strip_prefix("mailto:")
        .or_else(|| trimmed.strip_prefix("MAILTO:"))
        .unwrap_or(trimmed)
}

/// One agenda timestamp, in the form it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stamp {
    Instant(DateTime<Utc>),
    Floating(NaiveDateTime),
    Date(NaiveDate),
}

impl Stamp {
    /// The instant it names, a floating time and a date read in `viewer`.
    fn instant(self, viewer: Tz) -> Option<DateTime<Utc>> {
        let local = match self {
            Stamp::Instant(instant) => return Some(instant),
            Stamp::Floating(local) => local,
            Stamp::Date(date) => date.and_time(NaiveTime::MIN),
        };
        crate::calendar::resolve_local(&viewer, &local).map(|zoned| zoned.with_timezone(&Utc))
    }
}

fn parse_stamp(raw: &str) -> Option<Stamp> {
    let raw = raw.trim();
    if let Ok(instant) = DateTime::parse_from_rfc3339(raw) {
        return Some(Stamp::Instant(instant.with_timezone(&Utc)));
    }
    if let Ok(local) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Some(if local.time() == NaiveTime::MIN {
            Stamp::Date(local.date())
        } else {
            Stamp::Floating(local)
        });
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .map(Stamp::Date)
}

/// The window a view of `start..end` covers, refusing an empty or inverted
/// range and one longer than [`MAX_VIEW_DAYS`].
pub fn range(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<CalendarWindow, String> {
    if end <= start {
        return Err("end must come after start".into());
    }
    if end - start > Duration::days(MAX_VIEW_DAYS) {
        return Err(format!("a view spans at most {MAX_VIEW_DAYS} days"));
    }
    Ok(CalendarWindow { start, end })
}

/// The longest range one `calendar.view` answers: a little over the cache
/// window, so a client can ask for all of it at once.
pub const MAX_VIEW_DAYS: i64 = 400;

#[cfg(test)]
mod tests {
    use super::*;
    use mp_protocol::calendar::{EventAttendee, EventFrontmatter};

    fn row(start: Option<&str>, end: Option<&str>) -> AgendaEvent {
        AgendaEvent {
            row_id: 42,
            event: EventFrontmatter {
                uid: Some("uid-1@example.com".into()),
                method: Some("REQUEST".into()),
                summary: Some("LOC Day planning".into()),
                start: start.map(str::to_string),
                end: end.map(str::to_string),
                location: Some("Room 4.12".into()),
                organizer: Some("mailto:chair@tum.de".into()),
                rsvp: "tentative".into(),
                attendees: vec![EventAttendee {
                    address: "alpha@example.com".into(),
                    status: "tentative".into(),
                }],
                ..EventFrontmatter::default()
            },
            subject: "Invitation: LOC Day planning".into(),
            ..AgendaEvent::default()
        }
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, mo, d, h, 0, 0).unwrap()
    }

    #[test]
    fn a_zoned_invitation_keeps_its_instants_labelled_utc() {
        let event = imip_event(
            &row(
                Some("2026-07-20T14:00:00+02:00"),
                Some("2026-07-20T15:00:00+02:00"),
            ),
            "alpha",
            chrono_tz::Europe::Berlin,
        )
        .unwrap();
        assert_eq!(
            event.time,
            EventTime::timed(utc(2026, 7, 20, 12), utc(2026, 7, 20, 13), "UTC").unwrap()
        );
        assert_eq!(event.id, "imip:42");
        assert_eq!(event.calendar, IMIP_CALENDAR_ID);
        assert_eq!(event.uid, "uid-1@example.com");
        assert_eq!(event.summary, "LOC Day planning");
        assert_eq!(event.organizer.unwrap().email, "chair@tum.de");
        assert_eq!(event.own_response, OwnResponse::Tentative);
        assert_eq!(event.show_as, ShowAs::Tentative);
        assert_eq!(event.attendees[0].status, ResponseStatus::Tentative);
    }

    #[test]
    fn a_floating_time_is_read_in_the_viewers_zone() {
        let event = imip_event(
            &row(Some("2026-07-20T09:00:00"), None),
            "alpha",
            chrono_tz::Europe::Berlin,
        )
        .unwrap();
        assert_eq!(
            event.time,
            EventTime::timed(utc(2026, 7, 20, 7), utc(2026, 7, 20, 7), "Europe/Berlin").unwrap()
        );
    }

    #[test]
    fn a_midnight_or_a_date_is_an_all_day_event_with_an_exclusive_end() {
        let day = |d| NaiveDate::from_ymd_opt(2026, 7, d).unwrap();
        let two_days = imip_event(
            &row(Some("2026-07-20T00:00:00"), Some("2026-07-22T00:00:00")),
            "alpha",
            Tz::UTC,
        )
        .unwrap();
        assert_eq!(two_days.time, EventTime::all_day(day(20), day(22)).unwrap());
        let no_end = imip_event(&row(Some("2026-07-20"), None), "alpha", Tz::UTC).unwrap();
        assert_eq!(no_end.time, EventTime::all_day(day(20), day(21)).unwrap());
    }

    #[test]
    fn an_invitation_without_a_start_or_a_uid() {
        assert!(imip_event(&row(None, None), "alpha", Tz::UTC).is_none());
        let mut nameless = row(Some("2026-07-20T09:00:00Z"), None);
        nameless.event.uid = None;
        nameless.event.summary = None;
        nameless.is_organizer = true;
        let event = imip_event(&nameless, "alpha", Tz::UTC).unwrap();
        assert_eq!(event.uid, "row:42");
        assert_eq!(event.summary, "Invitation: LOC Day planning");
        assert_eq!(event.own_response, OwnResponse::Organizer);
    }

    #[test]
    fn an_imip_id_names_its_row() {
        assert_eq!(imip_row_id(&imip_event_id(7)), Some(7));
        assert_eq!(imip_row_id("AAMkADev1"), None);
        assert_eq!(imip_row_id("imip:x"), None);
    }

    #[test]
    fn a_sourced_event_carries_its_source_on_the_wire() {
        let event = imip_event(&row(Some("2026-07-20T09:00:00Z"), None), "alpha", Tz::UTC).unwrap();
        let sourced = Sourced {
            event,
            source: Some(CalendarEventSource {
                kind: IMIP_SOURCE_KIND.into(),
                row_id: 42,
                selector: "mp://alpha/inbox/x@example.com".into(),
                subject: "Invitation".into(),
            }),
        };
        let wire = sourced.to_wire().unwrap();
        assert_eq!(wire.own_response, "tentative");
        assert_eq!(wire.source.unwrap().row_id, 42);
    }

    #[test]
    fn a_range_must_be_ordered_and_bounded() {
        assert!(range(utc(2026, 7, 20, 0), utc(2026, 7, 21, 0)).is_ok());
        assert!(range(utc(2026, 7, 20, 0), utc(2026, 7, 20, 0)).is_err());
        assert!(range(utc(2026, 7, 20, 0), utc(2028, 7, 20, 0)).is_err());
    }
}
