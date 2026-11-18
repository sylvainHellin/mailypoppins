//! Free/busy through EWS (PERSO-256, `.agents/plans/calendar.md`, phase 3,
//! unit 8): `GetUserAvailability`, which the spike read for the user and for
//! a room (`.agents/research/2026-11-13-ews-spike.md`, "GetUserAvailability").
//!
//! The request names a time zone, one mailbox per address and the window
//! with the slot length; the server answers each address's merged view (one
//! digit per slot, `5` for no data) and, in the detailed view, the blocks of
//! its calendar with their times in the request's zone. The request names
//! UTC, a zone with neither offset nor daylight saving, so the window goes
//! out and the blocks come back as UTC wall times and no zone rule is
//! rebuilt on either side. Exchange refuses a zone whose standard and
//! daylight transitions fall on the same day ("The specified time zone isn't
//! valid", TUM's server on 2026-11-18), so UTC goes out as the rule
//! Microsoft's own client sends for a zone without daylight saving: two
//! transitions on distinct days that change the offset by nothing. The
//! `CalendarEventArray` of a busy day, which the spike's free day did not
//! carry, is built from the schema documentation and unverified live.
//!
//! The answer is in default namespaces without prefixes, one
//! `FreeBusyResponse` per address in the order asked. An address the server
//! cannot read answers its own error, which becomes that schedule's error
//! while the others stand.

use chrono::{DateTime, NaiveDateTime, Utc};

use super::fetch::EwsFetch;
use super::parse::{fault_of, malformed, AnswerError};
use super::xml::{self, Element};
use super::{answer_error, EwsCalendarBackend};
use crate::calendar_backend::BackendResult;
use crate::calendar_freebusy::{slot_digit, FreeBusyQuery, Schedule, ScheduleItem};
use crate::calendar_model::ShowAs;

/// The wall-time form of the window: whole seconds, no offset.
const WALL_TIME: &str = "%Y-%m-%dT%H:%M:%S";

/// The operation's element, which its answer is named after.
const OPERATION: &str = "GetUserAvailability";

/// The `GetUserAvailabilityRequest` for `query`, in UTC: daylight from the
/// first Sunday of October and standard from the first Sunday of March, both
/// at 02:00 with no bias, a year of one offset that Exchange takes.
pub(super) fn availability_body(query: &FreeBusyQuery) -> String {
    let rule = |month: u32| {
        format!(
            "<t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>1</t:DayOrder>\
             <t:Month>{month}</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek>"
        )
    };
    let mailboxes: String = query
        .addresses
        .iter()
        .map(|address| {
            format!(
                "<t:MailboxData><t:Email><t:Address>{}</t:Address></t:Email>\
                 <t:AttendeeType>Required</t:AttendeeType>\
                 <t:ExcludeConflicts>false</t:ExcludeConflicts></t:MailboxData>",
                xml::escape(address.trim())
            )
        })
        .collect();
    let wall = |at: DateTime<Utc>| at.naive_utc().format(WALL_TIME).to_string();
    format!(
        "<m:{OPERATION}Request><t:TimeZone><t:Bias>0</t:Bias>\
         <t:StandardTime>{}</t:StandardTime><t:DaylightTime>{}</t:DaylightTime>\
         </t:TimeZone><m:MailboxDataArray>{mailboxes}</m:MailboxDataArray>\
         <t:FreeBusyViewOptions><t:TimeWindow><t:StartTime>{}</t:StartTime>\
         <t:EndTime>{}</t:EndTime></t:TimeWindow>\
         <t:MergedFreeBusyIntervalInMinutes>{}</t:MergedFreeBusyIntervalInMinutes>\
         <t:RequestedView>DetailedMerged</t:RequestedView></t:FreeBusyViewOptions>\
         </m:{OPERATION}Request>",
        rule(3),
        rule(10),
        wall(query.window.start),
        wall(query.window.end),
        query.interval.max(1)
    )
}

/// A `BusyType` as the model's status; `NoData` and anything else is
/// unknown.
fn busy_type(raw: Option<&str>) -> ShowAs {
    match raw {
        Some("Free") => ShowAs::Free,
        Some("Tentative") => ShowAs::Tentative,
        Some("Busy") => ShowAs::Busy,
        Some("OOF") => ShowAs::Oof,
        Some("WorkingElsewhere") => ShowAs::WorkingElsewhere,
        _ => ShowAs::Unknown,
    }
}

/// A merged view in the model's digits: Exchange's `5` (no data) is
/// unknown, the others are Graph's.
fn view_of(merged: &str) -> String {
    merged
        .chars()
        .map(|digit| match digit {
            '0'..='4' => digit,
            _ => slot_digit(ShowAs::Unknown),
        })
        .collect()
}

/// A block's time: a UTC wall time as the request's zone gives it, or an
/// instant with its offset.
fn block_time(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?.trim();
    NaiveDateTime::parse_from_str(raw, WALL_TIME)
        .map(|at| at.and_utc())
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|at| at.with_timezone(&Utc))
        })
}

/// The blocks of a view that are not free, in start order; a block whose
/// times do not read is dropped, the view still showing its slots.
fn items_of(view: &Element) -> Vec<ScheduleItem> {
    let mut items: Vec<ScheduleItem> = view
        .child("CalendarEventArray")
        .into_iter()
        .flat_map(|array| array.children_named("CalendarEvent"))
        .filter_map(|event| {
            let status = busy_type(event.text_of("BusyType"));
            if status == ShowAs::Free {
                return None;
            }
            let details = event.child("CalendarEventDetails");
            let detail = |name: &str| {
                details
                    .and_then(|details| details.text_of(name))
                    .map(str::to_string)
            };
            Some(ScheduleItem {
                start: block_time(event.text_of("StartTime"))?,
                end: block_time(event.text_of("EndTime"))?,
                status,
                subject: detail("Subject"),
                location: detail("Location"),
            })
        })
        .collect();
    items.sort_by_key(|item| item.start);
    items
}

/// One `FreeBusyResponse` as the schedule of `address`.
fn schedule_of(address: &str, response: &Element) -> Schedule {
    let message = response.child("ResponseMessage");
    if message.and_then(|message| message.attr("ResponseClass")) != Some("Success") {
        let text = message
            .and_then(|message| message.text_of("MessageText"))
            .unwrap_or("no free/busy")
            .trim_end_matches('.');
        let why = match message.and_then(|message| message.text_of("ResponseCode")) {
            Some(code) => format!("{text} ({code})"),
            None => text.to_string(),
        };
        return Schedule::unavailable(address, why);
    }
    let Some(view) = response.child("FreeBusyView") else {
        return Schedule::unavailable(address, "the server answered no free/busy view");
    };
    Schedule {
        address: address.to_string(),
        view: view_of(view.text_of("MergedFreeBusy").unwrap_or_default()),
        items: items_of(view),
        error: None,
    }
}

/// The schedules of `body` in the order `query` asked; an address the
/// answer leaves out has none.
pub(super) fn parse_schedules(
    query: &FreeBusyQuery,
    body: &str,
) -> Result<Vec<Schedule>, AnswerError> {
    let root = xml::parse(body).map_err(|err| malformed(format!("{err:#}")))?;
    let body = root
        .child("Body")
        .ok_or_else(|| malformed("no SOAP body"))?;
    if let Some(fault) = fault_of(body) {
        return Err(fault);
    }
    let responses = body
        .at(&[&format!("{OPERATION}Response"), "FreeBusyResponseArray"])
        .ok_or_else(|| malformed("no FreeBusyResponseArray"))?;
    let mut responses = responses.children_named("FreeBusyResponse");
    Ok(query
        .addresses
        .iter()
        .map(|address| match responses.next() {
            Some(response) => schedule_of(address, response),
            None => Schedule::unavailable(address, "the server answered nothing for it"),
        })
        .collect())
}

impl<F: EwsFetch> EwsCalendarBackend<F> {
    pub(super) async fn availability(&self, query: &FreeBusyQuery) -> BackendResult<Vec<Schedule>> {
        let answer = self
            .call(&format!("{OPERATION}Request"), &availability_body(query))
            .await?;
        parse_schedules(query, &answer).map_err(|error| answer_error(&error, OPERATION))
    }
}
