//! Free/busy (`freebusy.rs`): the GetUserAvailability request the server
//! receives, and its answer read from the spike's answers for the user and
//! a room.

use chrono::{TimeZone, Utc};

use super::super::fetch::{envelope, EwsFetchError};
use super::super::EwsCalendarBackend;
use super::ScriptedFetch;
use crate::calendar_backend::CalendarBackend;
use crate::calendar_freebusy::{FreeBusyQuery, Schedule};
use crate::calendar_model::{CalendarWindow, ShowAs};
use crate::config::EwsPasswordSource;

const SELF: &str = include_str!("../../../../tests/fixtures/ews/getuseravailability_self.xml");
const ROOM: &str = include_str!("../../../../tests/fixtures/ews/getuseravailability_room.xml");
const VERSION_REFUSED: &str =
    include_str!("../../../../tests/fixtures/ews/version_refused_exchange2019.xml");
/// TUM's answer of 2026-11-18 to a UTC zone whose standard and daylight
/// transitions were the same day (1 January).
const UTC_REFUSED: &str =
    include_str!("../../../../tests/fixtures/ews/getuseravailability_utc_refused.xml");

fn backend(fetch: ScriptedFetch) -> EwsCalendarBackend<ScriptedFetch> {
    EwsCalendarBackend::with_fetch("tum", EwsPasswordSource::Imap, fetch)
}

/// The Berlin day of 20 November 2026 at hour slots: the user, a room and
/// an address outside the organisation.
fn query() -> FreeBusyQuery {
    FreeBusyQuery {
        addresses: vec![
            "user@tum.de".into(),
            "room-1@tum.de".into(),
            "someone@gmail.com".into(),
        ],
        window: CalendarWindow {
            start: Utc.with_ymd_and_hms(2026, 11, 19, 23, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 11, 20, 23, 0, 0).unwrap(),
        },
        interval: 60,
    }
}

/// The `FreeBusyResponse` of a spike answer.
fn response_of(answer: &str) -> &str {
    let start = answer.find("<FreeBusyResponse>").unwrap();
    let close = "</FreeBusyResponse>";
    &answer[start..answer.find(close).unwrap() + close.len()]
}

/// One answer holding `responses`, in the spike's frame.
fn answer_with(responses: &[&str]) -> String {
    let head = SELF.find("<FreeBusyResponse>").unwrap();
    let tail = SELF.find("</FreeBusyResponseArray>").unwrap();
    format!("{}{}{}", &SELF[..head], responses.concat(), &SELF[tail..])
}

/// The user's busy morning: the spike's day was free, so the
/// `CalendarEventArray` is built from the schema documentation, its times
/// in the request's zone (UTC) without an offset.
fn busy_self() -> String {
    response_of(SELF)
        .replace("000000000000000000000000", "000000000221000000000005")
        .replace(
            "</MergedFreeBusy>",
            "</MergedFreeBusy><CalendarEventArray \
             xmlns=\"http://schemas.microsoft.com/exchange/services/2006/types\">\
             <CalendarEvent><StartTime>2026-11-20T10:00:00</StartTime>\
             <EndTime>2026-11-20T11:00:00</EndTime><BusyType>Tentative</BusyType>\
             <CalendarEventDetails><ID>x</ID><Subject></Subject><Location>Room 2</Location>\
             </CalendarEventDetails></CalendarEvent>\
             <CalendarEvent><StartTime>2026-11-20T08:00:00</StartTime>\
             <EndTime>2026-11-20T10:00:00</EndTime><BusyType>Busy</BusyType>\
             <CalendarEventDetails><ID>y</ID><Subject>Team &amp; budget</Subject>\
             </CalendarEventDetails></CalendarEvent>\
             <CalendarEvent><StartTime>2026-11-20T12:00:00</StartTime>\
             <EndTime>2026-11-20T13:00:00</EndTime><BusyType>Free</BusyType></CalendarEvent>\
             </CalendarEventArray>",
        )
}

/// An address the server cannot read, as the schema documentation gives
/// the per-mailbox error.
const OUTSIDE: &str = "<FreeBusyResponse><ResponseMessage ResponseClass=\"Error\">\
    <MessageText>No mailbox was found for this address.</MessageText>\
    <ResponseCode>ErrorMailRecipientNotFound</ResponseCode>\
    <DescriptiveLinkKey>0</DescriptiveLinkKey></ResponseMessage>\
    <FreeBusyView><FreeBusyViewType \
    xmlns=\"http://schemas.microsoft.com/exchange/services/2006/types\">None</FreeBusyViewType>\
    </FreeBusyView></FreeBusyResponse>";

/// The query goes out as one GetUserAvailability in UTC: a zone whose two
/// transitions change nothing, one mailbox per address in order, the window
/// as UTC wall times and the slot length, merged and detailed.
#[tokio::test]
async fn a_free_busy_query_names_each_address_and_the_window_in_utc() {
    let backend = backend(ScriptedFetch::default().answer(
        "GetUserAvailabilityRequest",
        &answer_with(&[response_of(SELF), response_of(ROOM), OUTSIDE]),
    ));
    backend.free_busy(&query()).await.unwrap();
    let rule = |month: u32| {
        format!(
            "<t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>1</t:DayOrder>\
             <t:Month>{month}</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek>"
        )
    };
    let mailbox = |address: &str| {
        format!(
            "<t:MailboxData><t:Email><t:Address>{address}</t:Address></t:Email>\
             <t:AttendeeType>Required</t:AttendeeType>\
             <t:ExcludeConflicts>false</t:ExcludeConflicts></t:MailboxData>"
        )
    };
    assert_eq!(
        backend.fetch().sent(),
        [envelope(&format!(
            "<m:GetUserAvailabilityRequest><t:TimeZone><t:Bias>0</t:Bias>\
             <t:StandardTime>{}</t:StandardTime><t:DaylightTime>{}</t:DaylightTime>\
             </t:TimeZone><m:MailboxDataArray>{}{}{}</m:MailboxDataArray>\
             <t:FreeBusyViewOptions><t:TimeWindow><t:StartTime>2026-11-19T23:00:00</t:StartTime>\
             <t:EndTime>2026-11-20T23:00:00</t:EndTime></t:TimeWindow>\
             <t:MergedFreeBusyIntervalInMinutes>60</t:MergedFreeBusyIntervalInMinutes>\
             <t:RequestedView>DetailedMerged</t:RequestedView></t:FreeBusyViewOptions>\
             </m:GetUserAvailabilityRequest>",
            rule(3),
            rule(10),
            mailbox("user@tum.de"),
            mailbox("room-1@tum.de"),
            mailbox("someone@gmail.com"),
        ))]
    );
}

/// TUM's answer of 2026-11-18 for the user's 11 December in UTC at half-hour
/// slots: one working-elsewhere block from 02:00 to 02:30, which the merged
/// view leaves free.
const WORKING_ELSEWHERE: &str =
    include_str!("../../../../tests/fixtures/ews/getuseravailability_working_elsewhere.xml");

/// The merged view Exchange answers counts a working-elsewhere block as
/// free (the live answer), where Graph's view and the fake's show it, so
/// each slot shows the heavier of the merged digit and the blocks over it.
#[tokio::test]
async fn a_working_elsewhere_block_shows_in_the_view_the_server_left_free() {
    let backend =
        backend(ScriptedFetch::default().answer("GetUserAvailabilityRequest", WORKING_ELSEWHERE));
    let query = FreeBusyQuery {
        addresses: vec!["user@tum.de".into()],
        window: CalendarWindow {
            start: Utc.with_ymd_and_hms(2026, 12, 11, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 12, 12, 0, 0, 0).unwrap(),
        },
        interval: 30,
    };
    let schedules = backend.free_busy(&query).await.unwrap();
    let own = &schedules[0];
    assert_eq!(own.items.len(), 1);
    assert_eq!(own.items[0].status, ShowAs::WorkingElsewhere);
    assert_eq!(
        own.items[0].start,
        Utc.with_ymd_and_hms(2026, 12, 11, 2, 0, 0).unwrap()
    );
    assert_eq!(own.view, format!("{}4{}", "0".repeat(4), "0".repeat(43)));
}

/// The text of every `name` element of `xml`.
fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<t:{name}>"), format!("</t:{name}>"));
    xml.match_indices(&open)
        .map(|(at, _)| {
            let start = at + open.len();
            &xml[start..start + xml[start..].find(&close).unwrap()]
        })
        .collect()
}

/// Exchange refuses a time zone whose standard and daylight transitions
/// fall on the same day as invalid (the live answer, read as the server's
/// sentence), so the UTC of the request changes twice a year by nothing,
/// the rule Microsoft's own client sends for a zone without daylight
/// saving: daylight from the first Sunday of October, standard from the
/// first Sunday of March, both at 02:00 with no bias.
#[tokio::test]
async fn the_utc_zone_has_two_distinct_transitions_since_exchange_refuses_one_day() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("GetUserAvailabilityRequest", UTC_REFUSED)
            .answer(
                "GetUserAvailabilityRequest",
                &answer_with(&[response_of(SELF), response_of(ROOM), OUTSIDE]),
            ),
    );
    let err = backend.free_busy(&query()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Exchange server refused GetUserAvailability: -2146233088: The specified time zone \
         isn't valid."
    );
    backend.free_busy(&query()).await.unwrap();
    let sent = &backend.fetch().sent()[1];
    let rule = |month: u32| {
        format!(
            "<t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>1</t:DayOrder>\
             <t:Month>{month}</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek>"
        )
    };
    assert_eq!(elements(sent, "StandardTime"), [rule(3)]);
    assert_eq!(elements(sent, "DaylightTime"), [rule(10)]);
    assert!(sent.contains("<t:TimeZone><t:Bias>0</t:Bias><t:StandardTime>"));
}

/// Each address has its schedule in the order asked: the merged view with
/// Exchange's no-data digit as unknown, the blocks that are not free in
/// start order with an empty subject dropped, the room's free day, and the
/// outside address its error while the others stand.
#[tokio::test]
async fn the_answer_reads_one_schedule_per_address_an_unknown_one_its_error() {
    let busy = busy_self();
    let backend = backend(ScriptedFetch::default().answer(
        "GetUserAvailabilityRequest",
        &answer_with(&[&busy, response_of(ROOM), OUTSIDE]),
    ));
    let schedules = backend.free_busy(&query()).await.unwrap();
    let at = |hour| Utc.with_ymd_and_hms(2026, 11, 20, hour, 0, 0).unwrap();
    let own = &schedules[0];
    assert_eq!(own.address, "user@tum.de");
    assert_eq!(own.view, "00000000022100000000000?");
    assert_eq!(own.error, None);
    assert_eq!(own.items.len(), 2, "the free block is left out");
    assert_eq!((own.items[0].start, own.items[0].end), (at(8), at(10)));
    assert_eq!(own.items[0].status, ShowAs::Busy);
    assert_eq!(own.items[0].subject.as_deref(), Some("Team & budget"));
    assert_eq!(own.items[0].location, None);
    assert_eq!(own.items[1].status, ShowAs::Tentative);
    assert_eq!(own.items[1].subject, None, "an empty subject is none");
    assert_eq!(own.items[1].location.as_deref(), Some("Room 2"));
    assert_eq!(
        schedules[1],
        Schedule {
            address: "room-1@tum.de".into(),
            view: "0".repeat(24),
            items: Vec::new(),
            error: None,
        }
    );
    assert_eq!(
        schedules[2],
        Schedule::unavailable(
            "someone@gmail.com",
            "No mailbox was found for this address (ErrorMailRecipientNotFound)"
        )
    );
}

/// An answer with fewer responses than addresses leaves the rest without a
/// schedule, a SOAP fault is the server's sentence, and a refused login is
/// the pass's refusal.
#[tokio::test]
async fn a_short_answer_a_fault_and_a_refused_login() {
    let backend = backend(
        ScriptedFetch::default()
            .answer(
                "GetUserAvailabilityRequest",
                &answer_with(&[response_of(SELF)]),
            )
            .answer("GetUserAvailabilityRequest", VERSION_REFUSED)
            .fail(
                "GetUserAvailabilityRequest",
                EwsFetchError::LoginRefused("HTTP 401".into()),
            ),
    );
    let schedules = backend.free_busy(&query()).await.unwrap();
    assert_eq!(schedules.len(), 3);
    assert_eq!(schedules[0].view, "0".repeat(24));
    assert_eq!(
        schedules[2].error.as_deref(),
        Some("the server answered nothing for it")
    );
    let err = backend.free_busy(&query()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Exchange server refused GetUserAvailability: ErrorInvalidRequest: The request is \
         invalid."
    );
    let err = backend.free_busy(&query()).await.unwrap_err();
    assert!(matches!(
        err,
        crate::calendar_backend::CalendarBackendError::LoginRefused(_)
    ));
}
