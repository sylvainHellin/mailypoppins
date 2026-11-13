//! The organizer's half of iMIP after the first send (PERSO-204): an
//! invitation we sent, read back from its sent copy's `invite.ics`, and the
//! update and the cancellation built from it.
//!
//! What goes out here is exactly what the receive half accepts
//! ([`crate::reconcile::fold_status`]): an update keeps the `UID` and raises
//! the `SEQUENCE` above every version the account holds
//! ([`crate::reconcile::next_sequence`]), and a cancellation of one
//! occurrence names it by a `RECURRENCE-ID` written in the form of the
//! series' own `DTSTART`, so it normalises to the identity an override of
//! that occurrence would carry.

use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use icalendar::{Calendar, CalendarComponent, Component, Event, EventLike, EventStatus, Property};

use super::{
    cn_param, itip_calendar, parse_date, parse_datetime_in, timezones_named, with_timezones,
    EventSpan, InviteSpec,
};
use crate::calendar_model::EventTime;
use crate::calendar_write::EventPatch;

/// An invitation the account organised, as its sent copy's `invite.ics`
/// states it: the source of truth for the `UID`, the `SEQUENCE` and every
/// field an update keeps, never the agenda's rendering of it.
#[derive(Debug, Clone)]
pub struct OrganizedInvite {
    pub uid: String,
    pub sequence: u32,
    /// `ORGANIZER`, bare, no `mailto:`.
    pub organizer: String,
    pub organizer_cn: Option<String>,
    /// Each `ATTENDEE`, bare, deduplicated without regard to case, the
    /// organizer left out.
    pub attendees: Vec<String>,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    /// The `RRULE`, whose `FREQ` says whether every occurrence starts at
    /// the series' time of day.
    pub rrule: Option<String>,
    /// A series (`RRULE` or `RDATE`).
    pub recurring: bool,
    /// The payload's own `RECURRENCE-ID`, verbatim: it is about one
    /// occurrence of a series.
    pub recurrence_id: Option<Property>,
    /// `DTSTART`, `DTEND` and `DURATION`, verbatim, value and parameters.
    pub dtstart: Option<Property>,
    pub dtend: Option<Property>,
    pub duration: Option<Property>,
    /// The `VTIMEZONE`s those name by `TZID`, as the source text.
    pub timezones: Vec<String>,
}

impl OrganizedInvite {
    /// The summary a sentence names the invitation by.
    pub fn title(&self) -> &str {
        self.summary.as_deref().unwrap_or("The invitation")
    }
}

/// Read the `invite.ics` of an invitation we sent.
///
/// Refused when the payload is not a `METHOD:REQUEST` (a reply or a
/// cancellation has nothing left to change), or names no `UID` (nothing
/// could address the change) or no `ORGANIZER`. Whether the organizer is
/// the account is the caller's to check: it owns the account.
pub fn organized_from_ics(bytes: &[u8]) -> Result<OrganizedInvite> {
    let text = std::str::from_utf8(bytes).context("The invitation's .ics is not valid UTF-8")?;
    let calendar = Calendar::from_str(text)
        .map_err(|e| anyhow!("Failed to parse the invitation's .ics: {e}"))?;
    let method = calendar
        .property_value("METHOD")
        .map(|m| m.trim().to_uppercase());
    if method.as_deref() != Some("REQUEST") {
        return Err(anyhow!(
            "This calendar item is not an invitation (METHOD:{}), so there is nothing of it to \
             change or cancel",
            method.as_deref().unwrap_or("none")
        ));
    }
    let event = calendar
        .components
        .iter()
        .find_map(|c| match c {
            CalendarComponent::Event(e) => Some(e),
            _ => None,
        })
        .ok_or_else(|| anyhow!("The invitation's .ics has no VEVENT"))?;

    let uid = event
        .get_uid()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!("The invitation has no UID, so no update or cancellation could name it")
        })?;
    let organizer = event
        .property_value("ORGANIZER")
        .map(crate::calendar::strip_mailto)
        .filter(|o| !o.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("The invitation has no ORGANIZER"))?;
    let organizer_cn = event
        .properties()
        .get("ORGANIZER")
        .and_then(|p| p.params().get("CN"))
        .map(|cn| cn.value().to_string());
    let mut attendees: Vec<String> = Vec::new();
    for attendee in event.get_attendees() {
        let address = crate::calendar::strip_mailto(&attendee.cal_address);
        let known = |a: &String| a.eq_ignore_ascii_case(address);
        if !address.is_empty()
            && !address.eq_ignore_ascii_case(&organizer)
            && !attendees.iter().any(known)
        {
            attendees.push(address.to_string());
        }
    }
    let text_of = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let rrule = text_of(event.property_value("RRULE"));
    let recurring = rrule.is_some()
        || event.properties().contains_key("RDATE")
        || event.multi_properties().contains_key("RDATE");
    let property = |key: &str| event.properties().get(key).cloned();
    let (recurrence_id, dtstart, dtend, duration) = (
        property("RECURRENCE-ID"),
        property("DTSTART"),
        property("DTEND"),
        property("DURATION"),
    );
    let timezones = timezones_named(text, &[&dtstart, &dtend, &recurrence_id]);

    Ok(OrganizedInvite {
        uid,
        sequence: event.get_sequence().unwrap_or(0),
        organizer,
        organizer_cn,
        attendees,
        summary: text_of(event.get_summary()),
        location: text_of(event.get_location()),
        description: text_of(event.get_description()),
        rrule,
        recurring,
        recurrence_id,
        dtstart,
        dtend,
        duration,
        timezones,
    })
}

/// Build the `METHOD:CANCEL` of `invite` at `sequence`, told to `attendees`.
///
/// `occurrence` is the `RECURRENCE-ID` of the one occurrence cancelled
/// ([`occurrence_recurrence_id`], or an override's own), which the
/// cancellation's `DTSTART` repeats; `None` cancels the whole event, a
/// series with every occurrence, and echoes the invitation's
/// `DTSTART` and `DTEND` (or `DURATION`), which Exchange wants on every
/// iTIP message. `STATUS:CANCELLED` and the `ORGANIZER` with its `CN` go
/// with it, and the `VTIMEZONE`s the times name.
pub fn build_cancel_ics(
    invite: &OrganizedInvite,
    sequence: u32,
    occurrence: Option<&Property>,
    attendees: &[String],
) -> Result<String> {
    if attendees.is_empty() {
        return Err(anyhow!(
            "A cancellation needs at least one attendee to tell"
        ));
    }
    let mut event = Event::new();
    event
        .uid(&invite.uid)
        .sequence(sequence)
        .timestamp(Utc::now()) // DTSTAMP
        .status(EventStatus::Cancelled);
    if let Some(summary) = invite.summary.as_deref() {
        event.summary(summary);
    }
    let mut organizer = Property::new("ORGANIZER", format!("mailto:{}", invite.organizer));
    if let Some(cn) = invite.organizer_cn.as_deref().and_then(cn_param) {
        organizer.add_parameter("CN", &cn);
    }
    event.append_property(organizer);
    match occurrence {
        Some(rid) => {
            event.append_property(renamed(rid, "RECURRENCE-ID"));
            event.append_property(renamed(rid, "DTSTART"));
        }
        None => {
            let end = invite.dtend.as_ref().or(invite.duration.as_ref());
            for property in [invite.dtstart.as_ref(), end].into_iter().flatten() {
                event.append_property(property.clone());
            }
        }
    }
    for address in attendees {
        let mut attendee = Property::new("ATTENDEE", format!("mailto:{address}"));
        attendee.add_parameter("ROLE", "REQ-PARTICIPANT");
        event.append_multi_property(attendee);
    }
    let mut calendar = itip_calendar("CANCEL");
    calendar.push(event.done());
    with_timezones(calendar.to_string(), &invite.timezones)
}

/// `property` under another name, its value and parameters kept.
fn renamed(property: &Property, key: &str) -> Property {
    let mut renamed = Property::new(key, property.value());
    for (name, parameter) in property.params() {
        renamed.add_parameter(name, parameter.value());
    }
    renamed
}

/// The `RECURRENCE-ID` of the occurrence of `invite`'s series that starts
/// at `when`, written in the form of the series' `DTSTART`: a date for an
/// all-day series, the series' `TZID` and wall-clock time for a zoned one,
/// UTC for a UTC one, and a floating time read in `host` for a floating one.
///
/// `when` takes the forms of `--start`: a time read in the series' own zone
/// (in `host` for a UTC or a floating series), a time with an offset, or a
/// bare date for an all-day series. Without expanding the rule, two things
/// are still checked: the occurrence is not before the series starts, and it
/// starts at the series' time of day (a rule repeating within a day aside).
pub fn occurrence_recurrence_id(
    invite: &OrganizedInvite,
    when: &str,
    host: Tz,
) -> Result<Property> {
    let title = invite.title();
    if !invite.recurring {
        return Err(anyhow!(
            "{title} does not repeat, so it has no occurrence to cancel on its own"
        ));
    }
    let dtstart = invite
        .dtstart
        .as_ref()
        .ok_or_else(|| anyhow!("{title} has no DTSTART to name an occurrence against"))?;
    let raw = dtstart.value().trim();
    let is_date = dtstart
        .params()
        .get("VALUE")
        .is_some_and(|v| v.value().eq_ignore_ascii_case("DATE"))
        || raw.len() == 8;
    if is_date {
        let first = NaiveDate::parse_from_str(raw, "%Y%m%d")
            .with_context(|| format!("{title} starts on an unreadable date {raw:?}"))?;
        let day = parse_date(when).ok_or_else(|| {
            anyhow!(
                "{title} is an all-day series: name the occurrence by its day (2026-12-21), not \
                 {when:?}"
            )
        })?;
        if day < first {
            return Err(anyhow!(
                "{when} is before the series starts on {}",
                first.format("%Y-%m-%d")
            ));
        }
        let mut rid = Property::new("RECURRENCE-ID", day.format("%Y%m%d").to_string());
        rid.add_parameter("VALUE", "DATE");
        return Ok(rid);
    }
    if parse_date(when).is_some() {
        return Err(anyhow!(
            "{title} starts at a time of day: name the occurrence by its start time \
             (2026-12-21T09:00), not its day alone"
        ));
    }

    let utc = raw.ends_with(['Z', 'z']);
    let tzid = dtstart.params().get("TZID").map(|p| p.value().to_string());
    let zone = match (&tzid, utc) {
        (Some(tzid), false) => crate::calendar::zone_of_tzid(tzid)
            .ok_or_else(|| anyhow!("{title} names a time zone mp does not know ({tzid})"))?,
        (_, true) => chrono_tz::UTC,
        (None, false) => host,
    };
    let first = NaiveDateTime::parse_from_str(raw.trim_end_matches(['Z', 'z']), "%Y%m%dT%H%M%S")
        .with_context(|| format!("{title} starts at an unreadable time {raw:?}"))?;
    // A zoned series is read in its own zone; a UTC or floating one names
    // none a person thinks in, and is read where the user is.
    let reading = if zone == chrono_tz::UTC || tzid.is_none() {
        host
    } else {
        zone
    };
    let local = parse_datetime_in(when, reading)?
        .with_timezone(&zone)
        .naive_local();
    if local < first {
        return Err(anyhow!(
            "{when} is before the series starts ({})",
            first.format("%Y-%m-%d %H:%M")
        ));
    }
    let sub_daily = invite.rrule.as_deref().is_some_and(|rule| {
        rule.to_uppercase().split(';').any(|part| {
            matches!(
                part.trim(),
                "FREQ=HOURLY" | "FREQ=MINUTELY" | "FREQ=SECONDLY"
            )
        })
    });
    if !sub_daily && local.time() != first.time() {
        return Err(anyhow!(
            "The occurrences of {title} start at {}; {when} is not one of them",
            first.format("%H:%M")
        ));
    }
    let mut stamp = local.format("%Y%m%dT%H%M%S").to_string();
    if utc {
        stamp.push('Z');
    }
    let mut rid = Property::new("RECURRENCE-ID", stamp);
    if let (Some(tzid), false) = (&tzid, utc) {
        rid.add_parameter("TZID", tzid);
    }
    Ok(rid)
}

/// An update of an invitation we organised: the `VEVENT` it goes out as, and
/// the attendees it no longer invites, who are told with a cancellation.
#[derive(Debug, Clone)]
pub struct InviteUpdate {
    pub spec: InviteSpec,
    pub removed: Vec<String>,
}

/// The next version of `invite` at `sequence`: `patch` (read by
/// [`crate::calendar_write::resolve_patch`] against the agenda's event, whose
/// time is `current`) over what the sent copy says.
///
/// What an invitation mail cannot carry (a reminder, a show-as, an online
/// meeting, a sensitivity, the choice about new time proposals, a rule) is
/// refused rather than dropped, and so is an update of a series or of one
/// of its occurrences: the `VEVENT` is rebuilt from an [`InviteSpec`], which
/// holds no rule.
pub fn updated_invite(
    invite: &OrganizedInvite,
    current: &EventTime,
    patch: &EventPatch,
    sequence: u32,
) -> Result<InviteUpdate, String> {
    let title = invite.title();
    let unsent: Vec<&str> = [
        (patch.online_meeting.is_some(), "an online meeting"),
        (patch.reminder.is_some(), "a reminder"),
        (patch.show_as.is_some(), "a show-as"),
        (patch.sensitivity.is_some(), "a sensitivity"),
        (
            patch.allow_new_time_proposals.is_some(),
            "a choice about new time proposals",
        ),
        (patch.recurrence.is_some(), "a repeat rule"),
    ]
    .into_iter()
    .filter_map(|(named, what)| named.then_some(what))
    .collect();
    if !unsent.is_empty() {
        return Err(format!(
            "{title}: an invitation mail carries its summary, time, location, description and \
             attendees, not {}",
            unsent.join(", ")
        ));
    }
    if patch.is_empty() {
        return Err("Nothing to change: name at least one field of the event".to_string());
    }
    if invite.recurring || invite.recurrence_id.is_some() {
        return Err(format!(
            "mp cannot change a repeating invitation yet: {title} repeats; cancel it, or one \
             occurrence of it, instead"
        ));
    }

    let span = span_of(patch.time.as_ref().unwrap_or(current))?;
    let attendees = match &patch.attendees {
        Some(list) => {
            let mut kept: Vec<String> = Vec::new();
            for address in list.iter().map(|a| a.email.trim()) {
                if !address.is_empty() && !kept.iter().any(|k| k.eq_ignore_ascii_case(address)) {
                    kept.push(address.to_string());
                }
            }
            kept
        }
        None => invite.attendees.clone(),
    };
    if attendees.is_empty() {
        return Err(format!(
            "An invitation needs at least one attendee; cancel {title} instead"
        ));
    }
    let removed = invite
        .attendees
        .iter()
        .filter(|old| !attendees.iter().any(|new| new.eq_ignore_ascii_case(old)))
        .cloned()
        .collect();
    let summary = patch
        .summary
        .clone()
        .or_else(|| invite.summary.clone())
        .unwrap_or_else(|| title.to_string());
    let spec = InviteSpec {
        uid: invite.uid.clone(),
        sequence,
        organizer: invite.organizer.clone(),
        attendees,
        summary,
        span,
        location: match &patch.location {
            Some(location) => location.clone(),
            None => invite.location.clone(),
        },
        description: match &patch.body {
            Some(body) => body.clone(),
            None => invite.description.clone(),
        },
    };
    spec.span.validate().map_err(|e| format!("{e:#}"))?;
    Ok(InviteUpdate { spec, removed })
}

/// An agenda time as the span an [`InviteSpec`] holds.
fn span_of(time: &EventTime) -> Result<EventSpan, String> {
    match time {
        EventTime::Timed { start, end, .. } => Ok(EventSpan::Timed {
            start: *start,
            end: *end,
        }),
        EventTime::AllDay { start, end } => Ok(EventSpan::AllDay {
            first: *start,
            last: end
                .pred_opt()
                .ok_or_else(|| "The event's last day is out of range".to_string())?,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::parse_ics;
    use crate::calendar_model::EventTime;
    use crate::calendar_write::{AttendeeInput, EventPatch};
    use crate::invite::{build_invite_ics, EventSpan, InviteSpec};
    use crate::reconcile::{fold_status, InviteMessage};
    use chrono::{NaiveDate, TimeZone, Utc};

    fn spec() -> InviteSpec {
        InviteSpec {
            uid: "review-1@example.com".to_string(),
            sequence: 0,
            organizer: "me@example.com".to_string(),
            attendees: vec!["ada@example.com".to_string(), "bob@example.com".to_string()],
            summary: "Budget review".to_string(),
            span: EventSpan::Timed {
                start: Utc.with_ymd_and_hms(2026, 12, 14, 9, 0, 0).unwrap(),
                end: Utc.with_ymd_and_hms(2026, 12, 14, 10, 0, 0).unwrap(),
            },
            location: Some("Room 4.12".to_string()),
            description: Some("Agenda:\n- numbers".to_string()),
        }
    }

    fn sent() -> OrganizedInvite {
        organized_from_ics(build_invite_ics(&spec()).unwrap().as_bytes()).unwrap()
    }

    /// A weekly series Outlook sent from this account, in a Windows zone.
    const OUTLOOK_SERIES: &str = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nPRODID:Microsoft Exchange Server 2010\r\n\
        VERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:W. Europe Standard Time\r\nBEGIN:STANDARD\r\n\
        DTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
        RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\n\
        DTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
        RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\nORGANIZER;CN=Me:mailto:me@example.com\r\n\
        ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Ada:mailto:ada@example.com\r\n\
        RRULE:FREQ=WEEKLY;UNTIL=20270131T080000Z;INTERVAL=1;BYDAY=MO;WKST=MO\r\n\
        UID:weekly-1@example.com\r\nSUMMARY:Weekly sync\r\n\
        DTSTART;TZID=W. Europe Standard Time:20261207T090000\r\n\
        DTEND;TZID=W. Europe Standard Time:20261207T093000\r\n\
        SEQUENCE:2\r\nDTSTAMP:20261201T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn message(row_id: i64, ics: &str) -> InviteMessage {
        InviteMessage {
            row_id,
            mailbox: "inbox".to_string(),
            uid: row_id,
            subject: None,
            parsed: parse_ics(ics.as_bytes()).expect("the payload parses"),
        }
    }

    /// The attendee's view of `ics` once the account also holds `others`.
    fn as_attendee_sees(ics: &str, others: &[&str]) -> crate::types::EventFrontmatter {
        let mut all = vec![message(1, ics)];
        all.extend(
            others
                .iter()
                .enumerate()
                .map(|(i, other)| message(i as i64 + 2, other)),
        );
        let status = fold_status(&all);
        let mut event = crate::calendar::event_frontmatter(&all[0].parsed);
        status.apply(&mut event, all[0].dtstamp());
        event
    }

    #[test]
    fn an_invitation_mp_sent_reads_back_as_its_organizer_holds_it() {
        let invite = sent();
        assert_eq!(invite.uid, "review-1@example.com");
        assert_eq!(invite.sequence, 0);
        assert_eq!(invite.organizer, "me@example.com");
        assert_eq!(invite.attendees, ["ada@example.com", "bob@example.com"]);
        assert_eq!(invite.summary.as_deref(), Some("Budget review"));
        assert_eq!(invite.location.as_deref(), Some("Room 4.12"));
        assert_eq!(invite.description.as_deref(), Some("Agenda:\n- numbers"));
        assert!(!invite.recurring);
        assert!(invite.recurrence_id.is_none());
    }

    #[test]
    fn only_a_request_with_a_uid_and_an_organizer_is_one() {
        let request = build_invite_ics(&spec()).unwrap();
        let err = |ics: String| format!("{:#}", organized_from_ics(ics.as_bytes()).unwrap_err());
        assert!(
            err(request.replace("METHOD:REQUEST", "METHOD:REPLY")).contains("not an invitation"),
        );
        assert!(err(request.replace("UID:review-1@example.com\r\n", "")).contains("no UID"));
        assert!(
            err(request.replace("ORGANIZER:mailto:me@example.com\r\n", ""))
                .contains("no ORGANIZER")
        );
    }

    #[test]
    fn a_cancel_tombstones_the_attendees_copy() {
        let request = build_invite_ics(&spec()).unwrap();
        let cancel = build_cancel_ics(&sent(), 1, None, &sent().attendees).unwrap();
        assert!(cancel.contains("METHOD:CANCEL"), "{cancel}");
        assert!(cancel.contains("STATUS:CANCELLED"), "{cancel}");
        assert!(cancel.contains("SEQUENCE:1"), "{cancel}");
        assert!(!cancel.contains("RECURRENCE-ID"), "{cancel}");
        assert!(cancel.contains("ATTENDEE;ROLE=REQ-PARTICIPANT:mailto:bob@example.com"));

        let before = as_attendee_sees(&request, &[]);
        assert!(!before.cancelled);
        let after = as_attendee_sees(&request, &[&cancel]);
        assert!(after.cancelled, "the CANCEL lands on the REQUEST it names");
    }

    #[test]
    fn an_update_supersedes_the_attendees_copy() {
        let request = build_invite_ics(&spec()).unwrap();
        let patch = EventPatch {
            location: Some(Some("Room 1.01".to_string())),
            time: Some(
                EventTime::timed(
                    Utc.with_ymd_and_hms(2026, 12, 15, 9, 0, 0).unwrap(),
                    Utc.with_ymd_and_hms(2026, 12, 15, 10, 30, 0).unwrap(),
                    "UTC",
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        let current = EventTime::timed(
            Utc.with_ymd_and_hms(2026, 12, 14, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 12, 14, 10, 0, 0).unwrap(),
            "UTC",
        )
        .unwrap();
        let update = updated_invite(&sent(), &current, &patch, 1).unwrap();
        assert!(update.removed.is_empty());
        assert_eq!(update.spec.uid, "review-1@example.com");
        assert_eq!(update.spec.sequence, 1);
        assert_eq!(update.spec.summary, "Budget review");
        assert_eq!(
            update.spec.description.as_deref(),
            Some("Agenda:\n- numbers")
        );
        let update = build_invite_ics(&update.spec).unwrap();

        let old = as_attendee_sees(&request, &[&update]);
        assert!(old.superseded, "the old copy gives way");
        let new = as_attendee_sees(&update, &[&request]);
        assert!(!new.superseded && !new.cancelled);
        assert_eq!(new.location.as_deref(), Some("Room 1.01"));
        assert_eq!(new.start.as_deref(), Some("2026-12-15T09:00:00Z"));
        assert_eq!(new.end.as_deref(), Some("2026-12-15T10:30:00Z"));
    }

    #[test]
    fn an_update_names_the_attendees_it_drops_and_refuses_what_mail_cannot_carry() {
        let current = EventTime::timed(
            Utc.with_ymd_and_hms(2026, 12, 14, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 12, 14, 10, 0, 0).unwrap(),
            "UTC",
        )
        .unwrap();
        let attendees = |emails: &[&str]| EventPatch {
            attendees: Some(
                emails
                    .iter()
                    .map(|email| AttendeeInput {
                        email: email.to_string(),
                        name: None,
                        role: crate::calendar_model::AttendeeRole::Required,
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        let update = updated_invite(
            &sent(),
            &current,
            &attendees(&["ADA@example.com", "cy@x.org"]),
            1,
        )
        .unwrap();
        assert_eq!(update.spec.attendees, ["ADA@example.com", "cy@x.org"]);
        assert_eq!(update.removed, ["bob@example.com"]);

        let refused = |patch: EventPatch| updated_invite(&sent(), &current, &patch, 1).unwrap_err();
        assert!(refused(attendees(&[])).contains("at least one attendee"));
        assert!(refused(EventPatch::default()).contains("Nothing to change"));
        assert!(refused(EventPatch {
            reminder: Some(Some(15)),
            ..Default::default()
        })
        .contains("an invitation mail carries"));
    }

    #[test]
    fn an_all_day_update_keeps_its_last_day() {
        let current = EventTime::all_day(
            NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 25).unwrap(),
        )
        .unwrap();
        let patch = EventPatch {
            summary: Some("Closed".to_string()),
            ..Default::default()
        };
        let update = updated_invite(&sent(), &current, &patch, 3).unwrap();
        assert_eq!(
            update.spec.span,
            EventSpan::AllDay {
                first: NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
                last: NaiveDate::from_ymd_opt(2026, 12, 24).unwrap(),
            }
        );
    }

    #[test]
    fn an_occurrence_cancel_is_written_in_the_form_of_the_series_start() {
        let series = organized_from_ics(OUTLOOK_SERIES.as_bytes()).unwrap();
        assert!(series.recurring);
        assert_eq!(series.organizer_cn.as_deref(), Some("Me"));
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        let rid = occurrence_recurrence_id(&series, "2026-12-21T09:00", berlin).unwrap();
        assert_eq!(rid.key(), "RECURRENCE-ID");
        assert_eq!(rid.value(), "20261221T090000");
        // An instant with an offset names the same occurrence.
        let same = occurrence_recurrence_id(&series, "2026-12-21T08:00:00Z", chrono_tz::UTC);
        assert_eq!(same.unwrap().value(), "20261221T090000");

        let cancel = build_cancel_ics(&series, 3, Some(&rid), &series.attendees).unwrap();
        assert!(
            cancel.contains("RECURRENCE-ID;TZID=W. Europe Standard Time:20261221T090000"),
            "{cancel}"
        );
        assert!(
            cancel.contains("DTSTART;TZID=W. Europe Standard Time:20261221T090000"),
            "{cancel}"
        );
        assert!(
            cancel.contains("BEGIN:VTIMEZONE"),
            "the TZID's zone goes along"
        );
        assert!(
            cancel.contains("ORGANIZER;CN=Me:mailto:me@example.com"),
            "{cancel}"
        );

        // The attendee's series stays live and lists the occurrence, under the
        // identity an Outlook override of that occurrence would carry.
        let seen = as_attendee_sees(OUTLOOK_SERIES, &[&cancel]);
        assert!(!seen.cancelled);
        let override_ics = OUTLOOK_SERIES.replace(
            "SEQUENCE:2\r\n",
            "SEQUENCE:2\r\nRECURRENCE-ID;TZID=W. Europe Standard Time:20261221T090000\r\n",
        );
        let identity = parse_ics(override_ics.as_bytes()).unwrap().recurrence_id;
        assert_eq!(seen.cancelled_instances, vec![identity.unwrap()]);
    }

    #[test]
    fn an_occurrence_is_refused_when_the_series_cannot_have_it() {
        let series = organized_from_ics(OUTLOOK_SERIES.as_bytes()).unwrap();
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        let err = |when: &str| {
            format!(
                "{:#}",
                occurrence_recurrence_id(&series, when, berlin).unwrap_err()
            )
        };
        assert!(
            err("2026-12-21T10:00").contains("start at 09:00"),
            "{}",
            err("2026-12-21T10:00")
        );
        assert!(err("2026-11-30T09:00").contains("before the series starts"));
        assert!(err("2026-12-21").contains("its start time"));
        assert!(err("next monday").contains("Unrecognized datetime"));

        let single = sent();
        assert!(format!(
            "{:#}",
            occurrence_recurrence_id(&single, "2026-12-14T09:00", chrono_tz::UTC).unwrap_err()
        )
        .contains("does not repeat"));
    }

    #[test]
    fn an_all_day_series_names_its_occurrence_by_its_day() {
        let all_day = OUTLOOK_SERIES
            .replace(
                "DTSTART;TZID=W. Europe Standard Time:20261207T090000",
                "DTSTART;VALUE=DATE:20261207",
            )
            .replace(
                "DTEND;TZID=W. Europe Standard Time:20261207T093000",
                "DTEND;VALUE=DATE:20261208",
            );
        let series = organized_from_ics(all_day.as_bytes()).unwrap();
        let rid = occurrence_recurrence_id(&series, "2026-12-14", chrono_tz::UTC).unwrap();
        assert_eq!(rid.value(), "20261214");
        let cancel = build_cancel_ics(&series, 3, Some(&rid), &series.attendees).unwrap();
        assert!(
            cancel.contains("RECURRENCE-ID;VALUE=DATE:20261214"),
            "{cancel}"
        );
        assert!(!cancel.contains("BEGIN:VTIMEZONE"), "a date names no zone");
        assert!(format!(
            "{:#}",
            occurrence_recurrence_id(&series, "2026-12-14T09:00", chrono_tz::UTC).unwrap_err()
        )
        .contains("all-day"));
    }

    #[test]
    fn a_utc_series_names_its_occurrence_in_utc() {
        let utc = OUTLOOK_SERIES
            .replace(
                "DTSTART;TZID=W. Europe Standard Time:20261207T090000",
                "DTSTART:20261207T080000Z",
            )
            .replace(
                "DTEND;TZID=W. Europe Standard Time:20261207T093000",
                "DTEND:20261207T083000Z",
            );
        let series = organized_from_ics(utc.as_bytes()).unwrap();
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        let rid = occurrence_recurrence_id(&series, "2026-12-14T09:00", berlin).unwrap();
        assert_eq!(rid.value(), "20261214T080000Z");
        assert!(rid.params().is_empty());
    }
}
