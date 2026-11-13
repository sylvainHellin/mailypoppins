//! The answer readers (`parse.rs`) on every fixture, and on answers that are
//! cut short or wrong.

use chrono::{NaiveDate, TimeZone, Utc};

use super::super::parse::{self, AnswerError, Change, ItemType, SyncAnswer};
use super::super::xml;
use crate::calendar_model::{
    AttendeeRole, EventKind, EventTime, Frequency, OwnResponse, ResponseStatus, Sensitivity,
    ShowAs, WeekIndex, Weekday,
};

const FIND_FOLDER: &str = include_str!("../../../../tests/fixtures/ews/findfolder_calendars.xml");
const GET_FOLDER: &str = include_str!("../../../../tests/fixtures/ews/getfolder_calendar.xml");
const VIEW: &str = include_str!("../../../../tests/fixtures/ews/finditem_calendarview.xml");
const SINGLE: &str = include_str!("../../../../tests/fixtures/ews/getitem_single_attendees.xml");
const OCCURRENCE: &str = include_str!("../../../../tests/fixtures/ews/getitem_occurrence.xml");
const MASTER: &str = include_str!("../../../../tests/fixtures/ews/getitem_recurring_master.xml");
const EXCEPTION: &str = include_str!("../../../../tests/fixtures/ews/getitem_exception.xml");
const ALL_DAY: &str = include_str!("../../../../tests/fixtures/ews/getitem_allday.xml");
const FIRST_PAGE: &str =
    include_str!("../../../../tests/fixtures/ews/syncfolderitems_first_page.xml");
const LAST_PAGE: &str =
    include_str!("../../../../tests/fixtures/ews/syncfolderitems_last_page.xml");
const FINAL_EMPTY: &str =
    include_str!("../../../../tests/fixtures/ews/syncfolderitems_final_empty.xml");
const INVALID_STATE: &str =
    include_str!("../../../../tests/fixtures/ews/syncfolderitems_invalid_state.xml");
const VERSION_REFUSED: &str =
    include_str!("../../../../tests/fixtures/ews/version_refused_exchange2019.xml");

pub(super) const CALENDAR_ID: &str = "AQMkNLd7YVwoPTPpAGE4GHSzLFClWt80SZY8UZEjKSsaSIG1SyWsLZWdVQQKr8KvLsHjEDcM22LP7WndLpCKugG8DgwLb6sJWD47bsnr9GYXRfIAHICQ";
pub(super) const SINGLE_ID: &str = "AQMkJPm9WMqcEMZhVDR3OGUuKTMxNy30OIV4TMDzZNseTZB4CkPmHNIIHJVHz5AkZaKbTToC73RY8NjtOcdLdrH5FbfUp5zEGF29csgk3GJPTkCIXOOFbfS4YtfHq5pZDI64otjj9TUE6FD6PuVTEZ==";
pub(super) const OCCURRENCE_ID: &str = "AAMkYNv1ZQufOAPcSAN1KPKbQRInQu94AWZ9CSMqVMppYFW1DwSbWATSSNuU11PM72XYZYEOSYMHz0TrNtKcDViU40ON4ZmaLyeZwlL3HtpKk4yHFT97tjwg0NJKBPCMTJBTprB6KmbNr0gPVK42scio7MIS/M5mAYMKTK==";
pub(super) const EXCEPTION_ID: &str = "AAMkADp3XRxtOQSnMTT5EJUuPRMsHa37GSX8ICNrKAaoSGL8BjSaFLYSNRjE73D0qa9XGZBJCWKVv4DrSkWrRCcA55ZD1CkrAluQrfU4QxhFj6xCBM02oefn1GLQQQZXQTAColP3RooQj7hRWO64ujir2FWV5qwxEATKNP==";
pub(super) const SERIES_UID: &str = "576D093Z-985V-949W-NPX2-25871S084083";

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

/// The one calendar item of a GetItem fixture, converted.
fn event_of(body: &str) -> crate::calendar_model::Event {
    let mut items = parse::items(body).unwrap();
    assert_eq!(items.len(), 1);
    let item = items.remove(0).unwrap();
    parse::calendar_event("tum", CALENDAR_ID, &item).unwrap()
}

/// GetFolder and FindFolder name the one calendar by the same id.
#[test]
fn the_folder_answers_name_the_calendar() {
    let found = parse::folders(FIND_FOLDER, "FindFolder").unwrap();
    let default = parse::folders(GET_FOLDER, "GetFolder").unwrap();
    assert_eq!(found, default);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, CALENDAR_ID);
    assert_eq!(found[0].name, "Calendar");
    let calendar = parse::calendar_of("tum", &found[0], Some(CALENDAR_ID));
    assert!(calendar.is_default && calendar.can_edit && calendar.visible);
    assert!(calendar.online_meeting_providers.is_empty());
    assert!(!parse::calendar_of("tum", &found[0], None).is_default);
}

/// The CalendarView holds a single event, an occurrence and an exception,
/// never a series master, each in its Windows zone.
#[test]
fn the_calendar_view_lists_the_windows_items() {
    let view = parse::calendar_view(VIEW).unwrap();
    assert!(view.complete);
    let kinds: Vec<ItemType> = view.items.iter().map(|item| item.kind).collect();
    assert_eq!(
        kinds,
        [ItemType::Single, ItemType::Occurrence, ItemType::Exception]
    );
    let ids: Vec<&str> = view.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, [SINGLE_ID, OCCURRENCE_ID, EXCEPTION_ID]);
    assert_eq!(view.items[0].start, utc(2026, 11, 13, 7, 30));
    assert_eq!(view.items[0].end, utc(2026, 11, 13, 9, 30));
    assert_eq!(view.items[1].uid.as_deref(), Some(SERIES_UID));
    for item in &view.items {
        assert_eq!(item.zone.as_deref(), Some("W. Europe Standard Time"));
    }
    let truncated = VIEW.replace(
        "IncludesLastItemInRange=\"true\"",
        "IncludesLastItemInRange=\"false\"",
    );
    assert!(!parse::calendar_view(&truncated).unwrap().complete);
}

/// A single event the user organizes reads its people with their roles and
/// answers, its body, reminder, zone and free/busy.
#[test]
fn a_single_event_reads_its_attendees_and_body() {
    let event = event_of(SINGLE);
    assert_eq!(event.id, SINGLE_ID);
    assert_eq!(event.calendar, CALENDAR_ID);
    assert_eq!(event.kind, EventKind::Single);
    assert_eq!(event.recurrence_id, None);
    assert!(event
        .uid
        .starts_with("040000008200E00074C5B7101A82E008CDFE09E2"));
    assert_eq!(event.summary, "Subject 001");
    assert_eq!(event.location.as_deref(), Some("Location 01"));
    assert_eq!(
        event.time,
        EventTime::Timed {
            start: utc(2026, 11, 13, 7, 30),
            end: utc(2026, 11, 13, 9, 30),
            zone: "Europe/Berlin".into(),
        }
    );
    let organizer = event.organizer.as_ref().unwrap();
    assert_eq!(organizer.email, "user@example.org");
    assert_eq!(organizer.name.as_deref(), Some("Example User"));
    assert_eq!(event.own_response, OwnResponse::Organizer);
    assert!(event.is_organizer);
    assert_eq!(event.attendees.len(), 13);
    let required = event
        .attendees
        .iter()
        .filter(|a| a.role == AttendeeRole::Required)
        .count();
    assert_eq!(required, 11);
    assert_eq!(event.attendees[0].email, "person01@example.org");
    assert_eq!(event.attendees[0].status, ResponseStatus::Declined);
    assert_eq!(event.attendees[1].status, ResponseStatus::Accepted);
    assert_eq!(event.attendees[7].status, ResponseStatus::NeedsAction);
    assert_eq!(event.attendees[11].role, AttendeeRole::Optional);
    assert_eq!(event.attendees[12].email, "person13@example.org");
    let body = event.body.as_deref().unwrap();
    assert!(
        body.starts_with("Kuwqc Gucgtah-Yswncodwlx,\n\n\njf Kfvlerz"),
        "{body:?}"
    );
    assert!(!body.contains('\r'));
    assert_eq!(event.reminder, Some(15));
    assert_eq!(event.sensitivity, Sensitivity::Normal);
    assert_eq!(event.show_as, ShowAs::Busy);
    assert!(!event.cancelled && !event.is_online_meeting);
    assert!(event.allow_new_time_proposals && event.response_requested);
    assert_eq!(
        event.last_modified,
        Some(Utc.with_ymd_and_hms(2026, 11, 9, 13, 42, 21).unwrap())
    );
    assert!(event
        .web_link
        .as_deref()
        .unwrap()
        .starts_with("https://xmail.mwn.de/owa?itemid="));
    assert_eq!(event.recurrence, None);
}

/// An occurrence carries its series' `UID` and its original start as its
/// `RECURRENCE-ID`; the attendee's own answer is its `MyResponseType`.
#[test]
fn an_occurrence_names_its_series_and_slot() {
    let event = event_of(OCCURRENCE);
    assert_eq!(event.id, OCCURRENCE_ID);
    assert_eq!(event.kind, EventKind::Occurrence);
    assert_eq!(event.uid, SERIES_UID);
    assert_eq!(event.recurrence_id.as_deref(), Some("2026-11-18T08:00:00Z"));
    assert_eq!(event.own_response, OwnResponse::Accepted);
    assert!(!event.is_organizer);
    assert_eq!(
        event.organizer.as_ref().unwrap().email,
        "person06@example.org"
    );
    assert_eq!(event.attendees.len(), 3);
    assert_eq!(event.body, None, "an empty body is none");
    assert_eq!(event.series_master_id, None, "the backend sets it");
}

/// An exception keeps the slot it was moved from as its `RECURRENCE-ID`.
#[test]
fn an_exception_keeps_its_original_slot() {
    let event = event_of(EXCEPTION);
    assert_eq!(event.kind, EventKind::Exception);
    assert_eq!(event.recurrence_id.as_deref(), Some("2026-11-16T08:00:00Z"));
    assert_eq!(
        event.time,
        EventTime::Timed {
            start: utc(2026, 11, 17, 8, 0),
            end: utc(2026, 11, 17, 17, 0),
            zone: "Europe/Berlin".into(),
        }
    );
    assert_eq!(event.reminder, None, "ReminderIsSet false");
}

/// An all-day event's UTC midnights are read back into its zone, an
/// exclusive two-day range.
#[test]
fn an_all_day_event_is_its_local_date_range() {
    let event = event_of(ALL_DAY);
    assert_eq!(
        event.time,
        EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
        }
    );
    assert_eq!(event.show_as, ShowAs::Free);
}

/// A zone the Windows table lacks labels a timed event UTC with its
/// instants exact, and an all-day one falls back to its wall clocks.
#[test]
fn an_unknown_zone_is_utc_and_an_all_day_one_reads_its_wall_clock() {
    let custom = SINGLE.replace("W. Europe Standard Time", "Customized Time Zone");
    let event = event_of(&custom);
    assert_eq!(
        event.time,
        EventTime::Timed {
            start: utc(2026, 11, 13, 7, 30),
            end: utc(2026, 11, 13, 9, 30),
            zone: "UTC".into(),
        }
    );
    let custom_day = ALL_DAY.replace("W. Europe Standard Time", "Customized Time Zone");
    assert!(custom_day.contains("<t:StartWallClock>2026-11-24T00:00:00+01:00"));
    assert_eq!(
        event_of(&custom_day).time,
        EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
        }
    );
}

/// The series master reads its weekly rule, its range and its kind.
#[test]
fn the_series_master_reads_its_weekly_rule() {
    let event = event_of(MASTER);
    assert_eq!(event.kind, EventKind::SeriesMaster);
    assert_eq!(event.uid, SERIES_UID);
    let item = parse::items(MASTER).unwrap().remove(0).unwrap();
    let rule = parse::recurrence_of(&item).unwrap();
    assert_eq!(rule.frequency, Frequency::Weekly);
    assert_eq!(rule.interval, 1);
    assert_eq!(rule.weekdays, [Weekday::Wednesday]);
    assert_eq!(rule.first_day_of_week, Some(Weekday::Sunday));
    assert_eq!(rule.start, NaiveDate::from_ymd_opt(2026, 10, 14));
    assert_eq!(rule.until, NaiveDate::from_ymd_opt(2027, 10, 6));
    assert_eq!(rule.count, None);
    assert_eq!(
        event.recurrence.as_deref(),
        Some("Weekly on Wednesday until 2027-10-06")
    );
    assert_eq!(parse::zone_name(&item).as_deref(), Some("Europe/Berlin"));
}

/// The other patterns and ranges EWS writes, each read into its fields.
#[test]
fn every_recurrence_pattern_reads_its_own_fields() {
    let rule = |pattern: &str, range: &str| {
        let item = xml::parse(&format!(
            "<t:CalendarItem xmlns:t=\"t\"><t:Recurrence>{pattern}{range}</t:Recurrence></t:CalendarItem>"
        ))
        .unwrap();
        parse::recurrence_of(&item)
    };
    let monthly = rule(
        "<t:RelativeMonthlyRecurrence><t:Interval>2</t:Interval><t:DaysOfWeek>Thursday</t:DaysOfWeek>\
         <t:DayOfWeekIndex>Last</t:DayOfWeekIndex></t:RelativeMonthlyRecurrence>",
        "<t:NumberedRecurrence><t:StartDate>2026-01-29Z</t:StartDate>\
         <t:NumberOfOccurrences>6</t:NumberOfOccurrences></t:NumberedRecurrence>",
    )
    .unwrap();
    assert_eq!(monthly.frequency, Frequency::Monthly);
    assert_eq!(monthly.week_index, Some(WeekIndex::Last));
    assert_eq!(monthly.weekdays, [Weekday::Thursday]);
    assert_eq!(monthly.count, Some(6));
    assert_eq!(
        monthly.describe(),
        "Every 2 months on the last Thursday (6 times)"
    );
    let yearly = rule(
        "<t:AbsoluteYearlyRecurrence><t:DayOfMonth>31</t:DayOfMonth><t:Month>October</t:Month>\
         </t:AbsoluteYearlyRecurrence>",
        "<t:NoEndRecurrence><t:StartDate>2026-10-31+01:00</t:StartDate></t:NoEndRecurrence>",
    )
    .unwrap();
    assert_eq!((yearly.day_of_month, yearly.month), (Some(31), Some(10)));
    assert_eq!(yearly.start, NaiveDate::from_ymd_opt(2026, 10, 31));
    assert_eq!(yearly.describe(), "Yearly on 31 October");
    let workdays = rule(
        "<t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Weekday</t:DaysOfWeek>\
         </t:WeeklyRecurrence>",
        "",
    )
    .unwrap();
    assert_eq!(workdays.weekdays, Weekday::ALL[..5]);
    let daily = rule(
        "<t:DailyRecurrence><t:Interval>3</t:Interval></t:DailyRecurrence>",
        "",
    )
    .unwrap();
    assert_eq!((daily.frequency, daily.interval), (Frequency::Daily, 3));
    assert_eq!(
        rule(
            "<t:DailyRegeneration><t:Interval>3</t:Interval></t:DailyRegeneration>",
            ""
        ),
        None
    );
}

/// A first page of SyncFolderItems carries its state, says more follow,
/// and names single events by id and type.
#[test]
fn a_sync_page_reads_its_state_and_changes() {
    let SyncAnswer::Page(page) = parse::sync_page(FIRST_PAGE).unwrap() else {
        panic!("a page");
    };
    assert!(page.sync_state.starts_with("H4sI"));
    assert!(!page.last);
    assert_eq!(page.changes.len(), 20);
    assert!(page.changes.iter().all(|change| matches!(
        change,
        Change::Upsert {
            kind: Some(ItemType::Single),
            ..
        }
    )));
    let SyncAnswer::Page(last) = parse::sync_page(LAST_PAGE).unwrap() else {
        panic!("a page");
    };
    assert!(last.last);
    assert_eq!(last.changes.len(), 3);
    assert!(matches!(
        last.changes[2],
        Change::Upsert {
            kind: Some(ItemType::RecurringMaster),
            ..
        }
    ));
    let SyncAnswer::Page(empty) = parse::sync_page(FINAL_EMPTY).unwrap() else {
        panic!("a page");
    };
    assert!(empty.last && empty.changes.is_empty());
    assert_ne!(empty.sync_state, last.sync_state);
}

/// An update and a deletion read as such; a read flag changes nothing.
#[test]
fn an_update_and_a_deletion_read_as_changes() {
    let body = FINAL_EMPTY.replace(
        "<m:Changes/>",
        &format!(
            "<m:Changes><t:Update><t:CalendarItem><t:ItemId Id=\"{SINGLE_ID}\" ChangeKey=\"k\"/>\
             <t:CalendarItemType>Single</t:CalendarItemType></t:CalendarItem></t:Update>\
             <t:Delete><t:ItemId Id=\"gone\" ChangeKey=\"k\"/></t:Delete>\
             <t:ReadFlagChange><t:ItemId Id=\"read\"/><t:IsRead>true</t:IsRead></t:ReadFlagChange>\
             </m:Changes>"
        ),
    );
    let SyncAnswer::Page(page) = parse::sync_page(&body).unwrap() else {
        panic!("a page");
    };
    assert_eq!(
        page.changes,
        [
            Change::Upsert {
                id: SINGLE_ID.into(),
                kind: Some(ItemType::Single)
            },
            Change::Delete { id: "gone".into() },
        ]
    );
}

/// `ErrorInvalidSyncStateData` is the answer that starts a full resync.
#[test]
fn an_invalid_sync_state_is_its_own_answer() {
    assert_eq!(
        parse::sync_page(INVALID_STATE).unwrap(),
        SyncAnswer::InvalidState
    );
}

/// A SOAP fault and an error response message read as the server's code
/// and sentence, which the error names with the operation.
#[test]
fn a_fault_and_an_error_message_name_the_servers_code() {
    let fault = parse::sync_page(VERSION_REFUSED).unwrap_err();
    assert!(matches!(fault, AnswerError::Fault(ref error) if error.code == "ErrorInvalidRequest"));
    assert_eq!(
        fault.sentence("SyncFolderItems"),
        "the Exchange server refused SyncFolderItems: ErrorInvalidRequest: The request is invalid."
    );
    let busy = INVALID_STATE
        .replace(
            "Synchronization state data is corrupt or otherwise invalid.",
            "The server cannot service this request right now. Try again later.",
        )
        .replace("ErrorInvalidSyncStateData", "ErrorServerBusy");
    let refused = parse::sync_page(&busy).unwrap_err();
    assert_eq!(
        refused.sentence("SyncFolderItems"),
        "the Exchange server refused SyncFolderItems: ErrorServerBusy: The server cannot \
         service this request right now. Try again later."
    );
    let mut items = parse::items(&busy.replace("SyncFolderItems", "GetItem")).unwrap();
    assert_eq!(items.remove(0).unwrap_err().code, "ErrorServerBusy");
}

/// An answer cut short, another operation's answer, one without its payload,
/// and a page that is no XML are errors, never an empty list.
#[test]
fn a_truncated_or_wrong_answer_is_an_error_never_an_empty_list() {
    let cut = &VIEW[..VIEW.len() / 2];
    let err = parse::calendar_view(cut).unwrap_err();
    assert!(matches!(err, AnswerError::Malformed(_)), "{err:?}");
    assert!(err
        .sentence("FindItem")
        .starts_with("unreadable EWS answer to FindItem: "));
    let end = VIEW.rfind("</t:Items>").unwrap();
    assert!(matches!(
        parse::calendar_view(&VIEW[..end]).unwrap_err(),
        AnswerError::Malformed(_)
    ));
    assert!(matches!(
        parse::calendar_view(GET_FOLDER).unwrap_err(),
        AnswerError::Malformed(ref why) if why == "no FindItemResponse"
    ));
    let no_items = VIEW
        .replace("<t:Items>", "<t:Other>")
        .replace("</t:Items>", "</t:Other>");
    assert!(matches!(
        parse::calendar_view(&no_items).unwrap_err(),
        AnswerError::Malformed(ref why) if why == "no Items"
    ));
    for page in [
        "<html><body>Service Unavailable</body></html>",
        "",
        "not xml at all",
    ] {
        assert!(matches!(
            parse::sync_page(page).unwrap_err(),
            AnswerError::Malformed(_)
        ));
    }
    let no_state = FIRST_PAGE
        .replace("<m:SyncState>", "<m:Other>")
        .replace("</m:SyncState>", "</m:Other>");
    assert!(matches!(
        parse::sync_page(&no_state).unwrap_err(),
        AnswerError::Malformed(ref why) if why == "no SyncState"
    ));
    let no_messages = SINGLE.replace("GetItemResponseMessage", "Elsewhere");
    assert!(parse::items(&no_messages).is_err());
    let bad_time = SINGLE.replace("<t:Start>2026-11-13T07:30:00Z", "<t:Start>soon");
    let item = parse::items(&bad_time).unwrap().remove(0).unwrap();
    assert!(parse::calendar_event("tum", CALENDAR_ID, &item).is_err());
}
