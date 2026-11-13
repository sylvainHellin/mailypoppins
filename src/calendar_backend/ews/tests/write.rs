//! The writes (`write.rs`) through the scripted seam: every envelope the
//! server receives, asserted whole, and the answer read.
//!
//! The answers are built from Microsoft's schema documentation
//! ([`built_answer`]), since the spike ran no write.

use chrono::{NaiveDate, TimeZone, Utc};

use super::super::fetch::envelope;
use super::super::EwsCalendarBackend;
use super::parse::{CALENDAR_ID, OCCURRENCE_ID, SERIES_UID, SINGLE_ID};
use super::sync::{calendar, get_item, MASTER_ID};
use super::{built_answer, error_message, success_message, ScriptedFetch};
use crate::calendar_backend::{CalendarBackend, Response, Written};
use crate::calendar_model::{
    AttendeeRole, Event, EventTime, Frequency, Recurrence, Sensitivity, ShowAs, TimeSlot,
    WeekIndex, Weekday,
};
use crate::calendar_write::{AttendeeInput, EventInput, EventPatch, ProposedTime};
use crate::config::EwsPasswordSource;

const SINGLE: &str = include_str!("../../../../tests/fixtures/ews/getitem_single_attendees.xml");
const OCCURRENCE: &str = include_str!("../../../../tests/fixtures/ews/getitem_occurrence.xml");
const MASTER: &str = include_str!("../../../../tests/fixtures/ews/getitem_recurring_master.xml");

fn backend(fetch: ScriptedFetch) -> EwsCalendarBackend<ScriptedFetch> {
    EwsCalendarBackend::with_fetch("tum", EwsPasswordSource::Imap, fetch)
}

/// What the server answers a response object made through `CreateItem`
/// with: one successful message, its `Items` empty since a sent response is
/// not kept in the calendar.
fn response_made() -> String {
    built_answer("CreateItem", &success_message("CreateItem", "<m:Items/>"))
}

/// 14:00 to 15:00 in Berlin on 20 November 2026, typed in Berlin.
fn proposal() -> ProposedTime {
    ProposedTime {
        slot: TimeSlot {
            start: Utc.with_ymd_and_hms(2026, 11, 20, 13, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 11, 20, 14, 0, 0).unwrap(),
        },
        zone: "Europe/Berlin".into(),
    }
}

/// An accept that tells the organizer is an `AcceptItem` sent and saved,
/// on the event's ItemId, the comment its plain-text body with the user's
/// markup escaped.
#[tokio::test]
async fn an_accept_sends_the_response_with_the_comment_escaped() {
    let backend = backend(ScriptedFetch::default().answer("CreateItem", &response_made()));
    backend
        .respond(
            SINGLE_ID,
            Response::Accept,
            Some("See you <there> & then".into()),
            true,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        backend.fetch().sent(),
        [envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:AcceptItem>\
             <t:Body BodyType=\"Text\">See you &lt;there&gt; &amp; then</t:Body>\
             <t:ReferenceItemId Id=\"{SINGLE_ID}\"/></t:AcceptItem></m:Items></m:CreateItem>"
        ))]
    );
}

/// A tentative that tells nobody is saved only, an empty comment is no body,
/// and the proposed time goes out as its two UTC instants.
#[tokio::test]
async fn a_tentative_saved_only_carries_the_proposed_time() {
    let backend = backend(ScriptedFetch::default().answer("CreateItem", &response_made()));
    backend
        .respond(
            OCCURRENCE_ID,
            Response::Tentative,
            Some("  ".into()),
            false,
            Some(proposal()),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.fetch().sent(),
        [envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SaveOnly\"><m:Items><t:TentativelyAcceptItem>\
             <t:ReferenceItemId Id=\"{OCCURRENCE_ID}\"/>\
             <t:ProposedStart>2026-11-20T13:00:00Z</t:ProposedStart>\
             <t:ProposedEnd>2026-11-20T14:00:00Z</t:ProposedEnd>\
             </t:TentativelyAcceptItem></m:Items></m:CreateItem>"
        ))]
    );
}

/// An answer saved only may leave its unsent response in Drafts: when the
/// server names the item it saved, it is deleted for good; an answer sent
/// leaves nothing to delete, and a draft that cannot be deleted does not
/// fail the answer, which the calendar already holds.
#[tokio::test]
async fn an_answer_saved_only_deletes_the_draft_it_leaves() {
    let draft = |id: &str| {
        built_answer(
            "CreateItem",
            &success_message(
                "CreateItem",
                &format!(
                    "<m:Items><t:MeetingResponse><t:ItemId Id=\"{id}\" ChangeKey=\"k\"/>\
                     </t:MeetingResponse></m:Items>"
                ),
            ),
        )
    };
    let deleted = built_answer("DeleteItem", &success_message("DeleteItem", ""));
    let refused = built_answer(
        "DeleteItem",
        &error_message("DeleteItem", "ErrorAccessDenied", "Access is denied."),
    );
    let backend = backend(
        ScriptedFetch::default()
            .answer("CreateItem", &draft("draft-1"))
            .answer("DeleteItem", &deleted)
            .answer("CreateItem", &draft("draft-2"))
            .answer("DeleteItem", &refused)
            .answer("CreateItem", &response_made()),
    );
    backend
        .respond(SINGLE_ID, Response::Tentative, None, false, None)
        .await
        .unwrap();
    backend
        .respond(SINGLE_ID, Response::Decline, None, false, None)
        .await
        .unwrap();
    backend
        .respond(SINGLE_ID, Response::Accept, None, false, None)
        .await
        .unwrap();
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 5);
    assert_eq!(
        sent[1],
        envelope(
            "<m:DeleteItem DeleteType=\"HardDelete\"><m:ItemIds><t:ItemId Id=\"draft-1\"/>\
             </m:ItemIds></m:DeleteItem>"
        )
    );
    assert!(sent[3].contains("<t:ItemId Id=\"draft-2\"/>"));
    assert!(backend.fetch().exhausted());
}

/// Only a response item is a draft to delete: a saved `Message` is one too,
/// but an answer that names the calendar item (or any other kind) leaves
/// everything alone, since a HardDelete aimed at it would delete the event.
#[tokio::test]
async fn an_answer_that_names_the_calendar_item_deletes_nothing() {
    let saved = |element: &str, id: &str| {
        built_answer(
            "CreateItem",
            &success_message(
                "CreateItem",
                &format!(
                    "<m:Items><t:{element}><t:ItemId Id=\"{id}\" ChangeKey=\"k\"/>\
                     </t:{element}></m:Items>"
                ),
            ),
        )
    };
    let deleted = built_answer("DeleteItem", &success_message("DeleteItem", ""));
    let backend = backend(
        ScriptedFetch::default()
            .answer("CreateItem", &saved("CalendarItem", SINGLE_ID))
            .answer("CreateItem", &saved("MeetingRequest", "request"))
            .answer("CreateItem", &saved("Message", "draft-3"))
            .answer("DeleteItem", &deleted),
    );
    for _ in 0..3 {
        backend
            .respond(SINGLE_ID, Response::Accept, None, false, None)
            .await
            .unwrap();
    }
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 4, "only the Message is deleted");
    assert!(!sent.iter().any(|sent| sent.contains("<m:DeleteItem")
        && sent.contains(&format!("<t:ItemId Id=\"{SINGLE_ID}\"/>"))));
    assert!(sent[3].contains("<t:ItemId Id=\"draft-3\"/>"));
    assert!(backend.fetch().exhausted());
}

/// A response item whose id is the answered event's own is never deleted,
/// whatever kind the CreateItem answer names it as: a HardDelete on that id
/// would delete the event.
#[tokio::test]
async fn a_response_item_named_by_the_event_id_deletes_nothing() {
    let backend = backend(ScriptedFetch::default().answer(
        "CreateItem",
        &built_answer(
            "CreateItem",
            &success_message(
                "CreateItem",
                &format!(
                    "<m:Items><t:MeetingResponse><t:ItemId Id=\"{SINGLE_ID}\" \
                     ChangeKey=\"k\"/></t:MeetingResponse></m:Items>"
                ),
            ),
        ),
    ));
    backend
        .respond(SINGLE_ID, Response::Accept, None, false, None)
        .await
        .unwrap();
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 1, "the CreateItem alone: {sent:?}");
    assert!(backend.fetch().exhausted());
}

/// A decline carries its comment before the reference and the proposal
/// after it, the schema's order; an accept never carries a proposal.
#[tokio::test]
async fn a_decline_proposes_and_an_accept_never_does() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("CreateItem", &response_made())
            .answer("CreateItem", &response_made()),
    );
    backend
        .respond(
            SINGLE_ID,
            Response::Decline,
            Some("Clash".into()),
            true,
            Some(proposal()),
        )
        .await
        .unwrap();
    backend
        .respond(SINGLE_ID, Response::Accept, None, true, Some(proposal()))
        .await
        .unwrap();
    let sent = backend.fetch().sent();
    assert_eq!(
        sent[0],
        envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:DeclineItem>\
             <t:Body BodyType=\"Text\">Clash</t:Body><t:ReferenceItemId Id=\"{SINGLE_ID}\"/>\
             <t:ProposedStart>2026-11-20T13:00:00Z</t:ProposedStart>\
             <t:ProposedEnd>2026-11-20T14:00:00Z</t:ProposedEnd>\
             </t:DeclineItem></m:Items></m:CreateItem>"
        ))
    );
    assert_eq!(
        sent[1],
        envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:AcceptItem>\
             <t:ReferenceItemId Id=\"{SINGLE_ID}\"/></t:AcceptItem></m:Items></m:CreateItem>"
        ))
    );
}

/// A response the server refuses (an invitation the organizer cancelled) is
/// the server's code and sentence, naming the operation.
#[tokio::test]
async fn a_refused_response_is_the_servers_sentence() {
    let refused = built_answer(
        "CreateItem",
        &error_message(
            "CreateItem",
            "ErrorCalendarIsCancelledForAccept",
            "Calendar item is canceled and can&apos;t be accepted.",
        ),
    );
    let backend = backend(ScriptedFetch::default().answer("CreateItem", &refused));
    let err = backend
        .respond(SINGLE_ID, Response::Accept, None, true, None)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Exchange server refused CreateItem: ErrorCalendarIsCancelledForAccept: Calendar \
         item is canceled and can't be accepted."
    );
}

// ---------------------------------------------------------------------------
// Create, update, cancel, delete
// ---------------------------------------------------------------------------

/// The extended property a create stamps with its transaction id, as every
/// envelope names it.
const TX_FIELD: &str = "<t:ExtendedFieldURI DistinguishedPropertySetId=\"PublicStrings\" \
                        PropertyName=\"MailypoppinsTransactionId\" PropertyType=\"String\"/>";

/// The GetItem a write reads its item back with: the full item and the
/// transaction property.
fn read_back(id: &str) -> String {
    envelope(&format!(
        "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape>\
         <t:BodyType>Text</t:BodyType><t:AdditionalProperties>{TX_FIELD}\
         </t:AdditionalProperties></m:ItemShape><m:ItemIds><t:ItemId Id=\"{id}\"/>\
         </m:ItemIds></m:GetItem>"
    ))
}

/// The FindItem a create with transaction `tx` looks for an earlier try
/// with.
fn earlier_try(tx: &str) -> String {
    envelope(&format!(
        "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape>\
         </m:ItemShape><m:Restriction><t:IsEqualTo>{TX_FIELD}<t:FieldURIOrConstant>\
         <t:Constant Value=\"{tx}\"/></t:FieldURIOrConstant></t:IsEqualTo></m:Restriction>\
         <m:ParentFolderIds><t:FolderId Id=\"{CALENDAR_ID}\"/></m:ParentFolderIds></m:FindItem>"
    ))
}

/// A FindItem answer naming the calendar items `ids`.
fn found(ids: &[&str]) -> String {
    let items: String = ids
        .iter()
        .map(|id| {
            format!("<t:CalendarItem><t:ItemId Id=\"{id}\" ChangeKey=\"k\"/></t:CalendarItem>")
        })
        .collect();
    built_answer(
        "FindItem",
        &success_message(
            "FindItem",
            &format!(
                "<m:RootFolder TotalItemsInView=\"{}\" IncludesLastItemInRange=\"true\">\
                 <t:Items>{items}</t:Items></m:RootFolder>",
                ids.len()
            ),
        ),
    )
}

/// What the server answers a CreateItem or an UpdateItem of a calendar item
/// with: the item's id and change key.
fn saved(operation: &str, id: &str) -> String {
    let conflicts = if operation == "UpdateItem" {
        "<m:ConflictResults><t:Count>0</t:Count></m:ConflictResults>"
    } else {
        ""
    };
    built_answer(
        operation,
        &success_message(
            operation,
            &format!(
                "<m:Items><t:CalendarItem><t:ItemId Id=\"{id}\" ChangeKey=\"k2\"/>\
                 </t:CalendarItem></m:Items>{conflicts}"
            ),
        ),
    )
}

/// The single-event fixture as the server reads it back after a create
/// stamped it with transaction `tx`.
fn single_with_tx(tx: &str) -> String {
    SINGLE.replace(
        "</t:HasAttachments>",
        &format!(
            "</t:HasAttachments><t:ExtendedProperty>{TX_FIELD}<t:Value>{tx}</t:Value>\
             </t:ExtendedProperty>"
        ),
    )
}

/// 20 November 2026, `start_hour` to `end_hour` UTC, held in Berlin.
fn berlin(start_hour: u32, end_hour: u32) -> EventTime {
    EventTime::timed(
        Utc.with_ymd_and_hms(2026, 11, 20, start_hour, 0, 0)
            .unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 20, end_hour, 0, 0).unwrap(),
        "Europe/Berlin",
    )
    .unwrap()
}

fn person(email: &str, name: Option<&str>, role: AttendeeRole) -> AttendeeInput {
    AttendeeInput {
        email: email.into(),
        name: name.map(str::to_string),
        role,
    }
}

fn written_event(written: Written) -> Event {
    match written {
        Written::Event(event) => *event,
        Written::Series(series) => panic!("expected an event, got the series {series:?}"),
    }
}

/// A create first looks for an earlier try under its transaction id, then
/// sends every field in the schema's order with the user's text escaped,
/// the times as UTC instants under the zone's Windows id, the attendees by
/// role with a room as a resource, and the transaction id as an extended
/// property; it invites the attendees and reads the event back.
#[tokio::test]
async fn a_create_sends_every_field_in_the_schemas_order() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &found(&[]))
            .answer("CreateItem", &saved("CreateItem", SINGLE_ID))
            .answer("GetItem", &get_item(&[&single_with_tx("tx-1")])),
    );
    let mut input = EventInput::new("Review <budget> & plan", berlin(8, 9));
    input.location = Some("Room <1>".into());
    input.body = Some("Agenda: budget & staff".into());
    input.attendees = vec![
        person(
            "ada@example.com",
            Some("Ada <Lovelace>"),
            AttendeeRole::Required,
        ),
        person("bob@example.com", None, AttendeeRole::Optional),
        person("room-1@example.com", Some("Room 1"), AttendeeRole::Resource),
    ];
    input.reminder = Some(Some(10));
    input.show_as = Some(ShowAs::Tentative);
    input.sensitivity = Some(Sensitivity::Private);
    input.allow_new_time_proposals = Some(false);
    let written = backend
        .create_event(&calendar(), &input, "tx-1")
        .await
        .unwrap();
    let event = written_event(written);
    assert_eq!(event.id, SINGLE_ID);
    assert_eq!(event.transaction_id.as_deref(), Some("tx-1"));
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0], earlier_try("tx-1"));
    assert_eq!(
        sent[1],
        envelope(&format!(
            "<m:CreateItem SendMeetingInvitations=\"SendToAllAndSaveCopy\">\
             <m:SavedItemFolderId><t:FolderId Id=\"{CALENDAR_ID}\"/></m:SavedItemFolderId>\
             <m:Items><t:CalendarItem>\
             <t:Subject>Review &lt;budget&gt; &amp; plan</t:Subject>\
             <t:Sensitivity>Private</t:Sensitivity>\
             <t:Body BodyType=\"Text\">Agenda: budget &amp; staff</t:Body>\
             <t:ReminderIsSet>true</t:ReminderIsSet>\
             <t:ReminderMinutesBeforeStart>10</t:ReminderMinutesBeforeStart>\
             <t:ExtendedProperty>{TX_FIELD}<t:Value>tx-1</t:Value></t:ExtendedProperty>\
             <t:Start>2026-11-20T08:00:00Z</t:Start><t:End>2026-11-20T09:00:00Z</t:End>\
             <t:IsAllDayEvent>false</t:IsAllDayEvent>\
             <t:LegacyFreeBusyStatus>Tentative</t:LegacyFreeBusyStatus>\
             <t:Location>Room &lt;1&gt;</t:Location>\
             <t:RequiredAttendees><t:Attendee><t:Mailbox><t:Name>Ada &lt;Lovelace&gt;</t:Name>\
             <t:EmailAddress>ada@example.com</t:EmailAddress></t:Mailbox></t:Attendee>\
             </t:RequiredAttendees>\
             <t:OptionalAttendees><t:Attendee><t:Mailbox>\
             <t:EmailAddress>bob@example.com</t:EmailAddress></t:Mailbox></t:Attendee>\
             </t:OptionalAttendees>\
             <t:Resources><t:Attendee><t:Mailbox><t:Name>Room 1</t:Name>\
             <t:EmailAddress>room-1@example.com</t:EmailAddress></t:Mailbox></t:Attendee>\
             </t:Resources>\
             <t:StartTimeZone Id=\"W. Europe Standard Time\"/>\
             <t:EndTimeZone Id=\"W. Europe Standard Time\"/>\
             <t:AllowNewTimeProposal>false</t:AllowNewTimeProposal>\
             </t:CalendarItem></m:Items></m:CreateItem>"
        ))
    );
    assert_eq!(sent[2], read_back(SINGLE_ID));
    assert!(backend.fetch().exhausted());
}

/// An all-day series with no attendees invites nobody: its midnights go out
/// as the instants they are in its zone, the rule as EWS's weekly pattern
/// and end date, a reminder turned off as such, and the answer is the
/// series.
#[tokio::test]
async fn an_all_day_series_goes_out_in_its_zone_and_comes_back_as_the_series() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &found(&[]))
            .answer("CreateItem", &saved("CreateItem", MASTER_ID))
            .answer("GetItem", &get_item(&[MASTER])),
    );
    let mut input = EventInput::new(
        "Workshop",
        EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
        },
    );
    input.zone = Some("Europe/Berlin".into());
    input.reminder = Some(None);
    input.recurrence = Some(Recurrence {
        frequency: Frequency::Weekly,
        interval: 2,
        weekdays: vec![Weekday::Monday, Weekday::Wednesday],
        day_of_month: None,
        week_index: None,
        month: None,
        first_day_of_week: Some(Weekday::Monday),
        start: NaiveDate::from_ymd_opt(2026, 11, 16),
        until: NaiveDate::from_ymd_opt(2027, 1, 31),
        count: None,
    });
    let written = backend
        .create_event(&calendar(), &input, "tx-2")
        .await
        .unwrap();
    match written {
        Written::Series(series) => {
            assert_eq!(series.master_id, MASTER_ID);
            assert_eq!(series.uid, SERIES_UID);
        }
        Written::Event(event) => panic!("expected the series, got {event:?}"),
    }
    let sent = backend.fetch().sent();
    assert_eq!(
        sent[1],
        envelope(&format!(
            "<m:CreateItem SendMeetingInvitations=\"SendToNone\">\
             <m:SavedItemFolderId><t:FolderId Id=\"{CALENDAR_ID}\"/></m:SavedItemFolderId>\
             <m:Items><t:CalendarItem><t:Subject>Workshop</t:Subject>\
             <t:ReminderIsSet>false</t:ReminderIsSet>\
             <t:ExtendedProperty>{TX_FIELD}<t:Value>tx-2</t:Value></t:ExtendedProperty>\
             <t:Start>2026-11-15T23:00:00Z</t:Start><t:End>2026-11-16T23:00:00Z</t:End>\
             <t:IsAllDayEvent>true</t:IsAllDayEvent>\
             <t:Recurrence><t:WeeklyRecurrence><t:Interval>2</t:Interval>\
             <t:DaysOfWeek>Monday Wednesday</t:DaysOfWeek>\
             <t:FirstDayOfWeek>Monday</t:FirstDayOfWeek></t:WeeklyRecurrence>\
             <t:EndDateRecurrence><t:StartDate>2026-11-16</t:StartDate>\
             <t:EndDate>2027-01-31</t:EndDate></t:EndDateRecurrence></t:Recurrence>\
             <t:StartTimeZone Id=\"W. Europe Standard Time\"/>\
             <t:EndTimeZone Id=\"W. Europe Standard Time\"/>\
             </t:CalendarItem></m:Items></m:CreateItem>"
        ))
    );
    assert_eq!(sent[2], read_back(MASTER_ID));
}

/// The input a create of `event` would carry: every field it holds.
fn input_of(event: &Event) -> EventInput {
    EventInput {
        location: event.location.clone(),
        body: event.body.clone(),
        attendees: event
            .attendees
            .iter()
            .map(|attendee| AttendeeInput {
                email: attendee.email.clone(),
                name: attendee.name.clone(),
                role: attendee.role,
            })
            .collect(),
        reminder: Some(event.reminder),
        show_as: Some(event.show_as),
        sensitivity: Some(event.sensitivity),
        allow_new_time_proposals: Some(event.allow_new_time_proposals),
        ..EventInput::new(event.summary.clone(), event.time.clone())
    }
}

/// A create whose first try made the event (its answer lost) finds that
/// event by the transaction property, reads it, and updates it with its own
/// fields where they differ instead of making a second one.
#[tokio::test]
async fn a_retried_create_updates_the_event_its_first_try_made() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &found(&[SINGLE_ID]))
            .answer("GetItem", &get_item(&[&single_with_tx("tx-1")]))
            .answer("UpdateItem", &saved("UpdateItem", SINGLE_ID))
            .answer("GetItem", &get_item(&[&single_with_tx("tx-1")])),
    );
    let input = EventInput::new("Review", berlin(8, 9));
    let event = written_event(
        backend
            .create_event(&calendar(), &input, "tx-1")
            .await
            .unwrap(),
    );
    assert_eq!(event.id, SINGLE_ID);
    let sent = backend.fetch().sent();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0], earlier_try("tx-1"));
    assert_eq!(sent[1], read_back(SINGLE_ID));
    assert!(sent[2].contains(&format!("<t:ItemChange><t:ItemId Id=\"{SINGLE_ID}\"/>")));
    assert!(sent[2].contains(
        "<t:SetItemField><t:FieldURI FieldURI=\"item:Subject\"/><t:CalendarItem>\
         <t:Subject>Review</t:Subject></t:CalendarItem></t:SetItemField>"
    ));
    assert!(!sent.iter().any(|sent| sent.contains("<m:CreateItem")));
    assert_eq!(sent[3], read_back(SINGLE_ID));
}

/// A retried create whose first try made the event as asked answers that
/// event as it is, so no attendee is sent an update for a change that is
/// none.
#[tokio::test]
async fn a_retried_create_that_made_the_event_as_asked_changes_nothing() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &found(&[SINGLE_ID]))
            .answer("GetItem", &get_item(&[&single_with_tx("tx-1")])),
    );
    let held = written_event(
        super::super::parse::items(&single_with_tx("tx-1"))
            .unwrap()
            .remove(0)
            .map(|item| {
                Written::Event(Box::new(
                    super::super::parse::calendar_event("tum", CALENDAR_ID, &item).unwrap(),
                ))
            })
            .unwrap(),
    );
    let event = written_event(
        backend
            .create_event(&calendar(), &input_of(&held), "tx-1")
            .await
            .unwrap(),
    );
    assert_eq!(event, held);
    assert_eq!(event.transaction_id.as_deref(), Some("tx-1"));
    let sent = backend.fetch().sent();
    assert_eq!(sent, [earlier_try("tx-1"), read_back(SINGLE_ID)]);
    assert!(backend.fetch().exhausted());
}

/// A zone with no Windows id, a yearly rule every other year and an online
/// meeting are refused with a sentence before any request: Exchange would
/// read the first as UTC and has no way to hold the others.
#[tokio::test]
async fn what_exchange_cannot_hold_is_refused_before_any_request() {
    let backend = backend(ScriptedFetch::default());
    let troll = EventTime::timed(
        Utc.with_ymd_and_hms(2026, 11, 20, 8, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 20, 9, 0, 0).unwrap(),
        "Antarctica/Troll",
    )
    .unwrap();
    let err = backend
        .create_event(&calendar(), &EventInput::new("Station", troll), "tx-3")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Exchange holds an event's time zone by its Windows name, and Antarctica/Troll has \
         none; give the event another zone"
    );
    let mut all_day = EventInput::new(
        "Station day",
        EventTime::AllDay {
            start: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
        },
    );
    all_day.zone = Some("Antarctica/Troll".into());
    let patch = EventPatch {
        time: Some(all_day.time.clone()),
        zone: all_day.zone.clone(),
        ..Default::default()
    };
    let err = backend
        .update_event(CALENDAR_ID, SINGLE_ID, &patch)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("Antarctica/Troll has none"),
        "{err}"
    );
    let mut yearly = EventInput::new("Anniversary", berlin(8, 9));
    yearly.recurrence = Some(Recurrence {
        frequency: Frequency::Yearly,
        interval: 2,
        weekdays: Vec::new(),
        day_of_month: Some(20),
        week_index: None,
        month: Some(11),
        first_day_of_week: None,
        start: NaiveDate::from_ymd_opt(2026, 11, 20),
        until: None,
        count: None,
    });
    let err = backend
        .create_event(&calendar(), &yearly, "tx-4")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Exchange repeats a yearly series every year only, not every 2 years"
    );
    let mut online = EventInput::new("Call", berlin(8, 9));
    online.online_meeting = true;
    let err = backend
        .create_event(&calendar(), &online, "tx-5")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the EWS calendar of tum cannot add an online meeting: on-premises Exchange makes no \
         Teams link"
    );
    assert!(backend.fetch().sent().is_empty());
}

/// An update sets each field the patch names and deletes the ones it
/// empties, in one UpdateItem that overwrites whatever changed meanwhile
/// and tells the attendees, then reads the event back.
#[tokio::test]
async fn an_update_sets_and_deletes_each_named_field() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("UpdateItem", &saved("UpdateItem", SINGLE_ID))
            .answer("GetItem", &get_item(&[SINGLE])),
    );
    let patch = EventPatch {
        summary: Some("Moved & renamed".into()),
        time: Some(berlin(10, 11)),
        location: Some(None),
        body: Some(None),
        attendees: Some(vec![person(
            "ada@example.com",
            None,
            AttendeeRole::Required,
        )]),
        reminder: Some(Some(5)),
        show_as: Some(ShowAs::Free),
        sensitivity: Some(Sensitivity::Normal),
        allow_new_time_proposals: Some(true),
        ..Default::default()
    };
    let event = written_event(
        backend
            .update_event(CALENDAR_ID, SINGLE_ID, &patch)
            .await
            .unwrap(),
    );
    assert_eq!(event.id, SINGLE_ID);
    let set = |uri: &str, element: &str| {
        format!(
            "<t:SetItemField><t:FieldURI FieldURI=\"{uri}\"/><t:CalendarItem>{element}\
             </t:CalendarItem></t:SetItemField>"
        )
    };
    let delete = |uri: &str| {
        format!("<t:DeleteItemField><t:FieldURI FieldURI=\"{uri}\"/></t:DeleteItemField>")
    };
    let updates = [
        set("item:Subject", "<t:Subject>Moved &amp; renamed</t:Subject>"),
        set("item:Sensitivity", "<t:Sensitivity>Normal</t:Sensitivity>"),
        set("item:Body", "<t:Body BodyType=\"Text\"></t:Body>"),
        set(
            "item:ReminderIsSet",
            "<t:ReminderIsSet>true</t:ReminderIsSet>",
        ),
        set(
            "item:ReminderMinutesBeforeStart",
            "<t:ReminderMinutesBeforeStart>5</t:ReminderMinutesBeforeStart>",
        ),
        set(
            "calendar:StartTimeZone",
            "<t:StartTimeZone Id=\"W. Europe Standard Time\"/>",
        ),
        set(
            "calendar:EndTimeZone",
            "<t:EndTimeZone Id=\"W. Europe Standard Time\"/>",
        ),
        set("calendar:Start", "<t:Start>2026-11-20T10:00:00Z</t:Start>"),
        set("calendar:End", "<t:End>2026-11-20T11:00:00Z</t:End>"),
        set(
            "calendar:IsAllDayEvent",
            "<t:IsAllDayEvent>false</t:IsAllDayEvent>",
        ),
        set(
            "calendar:LegacyFreeBusyStatus",
            "<t:LegacyFreeBusyStatus>Free</t:LegacyFreeBusyStatus>",
        ),
        delete("calendar:Location"),
        set(
            "calendar:RequiredAttendees",
            "<t:RequiredAttendees><t:Attendee><t:Mailbox>\
             <t:EmailAddress>ada@example.com</t:EmailAddress></t:Mailbox></t:Attendee>\
             </t:RequiredAttendees>",
        ),
        delete("calendar:OptionalAttendees"),
        delete("calendar:Resources"),
        set(
            "calendar:AllowNewTimeProposal",
            "<t:AllowNewTimeProposal>true</t:AllowNewTimeProposal>",
        ),
    ]
    .concat();
    assert_eq!(
        backend.fetch().sent(),
        [
            envelope(&format!(
                "<m:UpdateItem MessageDisposition=\"SaveOnly\" \
                 ConflictResolution=\"AlwaysOverwrite\" \
                 SendMeetingInvitationsOrCancellations=\"SendToAllAndSaveCopy\">\
                 <m:ItemChanges><t:ItemChange><t:ItemId Id=\"{SINGLE_ID}\"/>\
                 <t:Updates>{updates}</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"
            )),
            read_back(SINGLE_ID),
        ]
    );
}

/// A change to one occurrence goes to the occurrence's own ItemId, and the
/// answer is that occurrence hung from its series, whose master is read for
/// it.
#[tokio::test]
async fn an_update_of_one_occurrence_answers_it_with_its_series() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("UpdateItem", &saved("UpdateItem", OCCURRENCE_ID))
            .answer("GetItem", &get_item(&[OCCURRENCE]))
            .answer("GetItem", &get_item(&[MASTER])),
    );
    let patch = EventPatch {
        summary: Some("Just this once".into()),
        ..Default::default()
    };
    let event = written_event(
        backend
            .update_event(CALENDAR_ID, OCCURRENCE_ID, &patch)
            .await
            .unwrap(),
    );
    assert_eq!(event.id, OCCURRENCE_ID);
    assert_eq!(event.series_master_id.as_deref(), Some(MASTER_ID));
    assert_eq!(event.uid, SERIES_UID);
    let sent = backend.fetch().sent();
    assert!(sent[0].contains(&format!("<t:ItemChange><t:ItemId Id=\"{OCCURRENCE_ID}\"/>")));
    assert!(sent[2].contains(&format!(
        "<t:RecurringMasterItemId OccurrenceId=\"{OCCURRENCE_ID}\"/>"
    )));
}

/// A new rule on a series goes out as EWS's pattern and range: the last
/// Friday of every month, five times, from the rule's first day.
#[tokio::test]
async fn a_new_rule_goes_out_as_the_pattern_and_the_range() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("UpdateItem", &saved("UpdateItem", MASTER_ID))
            .answer("GetItem", &get_item(&[MASTER])),
    );
    let patch = EventPatch {
        recurrence: Some(Recurrence {
            frequency: Frequency::Monthly,
            interval: 1,
            weekdays: vec![Weekday::Friday],
            day_of_month: None,
            week_index: Some(WeekIndex::Last),
            month: None,
            first_day_of_week: None,
            start: NaiveDate::from_ymd_opt(2026, 11, 27),
            until: None,
            count: Some(5),
        }),
        ..Default::default()
    };
    let written = backend
        .update_event(CALENDAR_ID, MASTER_ID, &patch)
        .await
        .unwrap();
    assert!(matches!(written, Written::Series(_)));
    assert!(backend.fetch().sent()[0].contains(
        "<t:SetItemField><t:FieldURI FieldURI=\"calendar:Recurrence\"/><t:CalendarItem>\
         <t:Recurrence><t:RelativeMonthlyRecurrence><t:Interval>1</t:Interval>\
         <t:DaysOfWeek>Friday</t:DaysOfWeek><t:DayOfWeekIndex>Last</t:DayOfWeekIndex>\
         </t:RelativeMonthlyRecurrence><t:NumberedRecurrence>\
         <t:StartDate>2026-11-27</t:StartDate><t:NumberOfOccurrences>5</t:NumberOfOccurrences>\
         </t:NumberedRecurrence></t:Recurrence></t:CalendarItem></t:SetItemField>"
    ));
}

/// A cancel sends the cancellation with the comment, escaped, to every
/// attendee; a meeting already gone counts as cancelled.
#[tokio::test]
async fn a_cancel_tells_every_attendee_and_a_gone_meeting_counts_as_cancelled() {
    let gone = built_answer(
        "CreateItem",
        &error_message(
            "CreateItem",
            "ErrorItemNotFound",
            "The specified object was not found in the store.",
        ),
    );
    let backend = backend(
        ScriptedFetch::default()
            .answer("CreateItem", &response_made())
            .answer("CreateItem", &gone),
    );
    backend
        .cancel_event(SINGLE_ID, Some("Sick <again>".into()))
        .await
        .unwrap();
    backend.cancel_event(SINGLE_ID, None).await.unwrap();
    let sent = backend.fetch().sent();
    assert_eq!(
        sent[0],
        envelope(&format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items>\
             <t:CancelCalendarItem><t:Body BodyType=\"Text\">Sick &lt;again&gt;</t:Body>\
             <t:ReferenceItemId Id=\"{SINGLE_ID}\"/></t:CancelCalendarItem></m:Items>\
             </m:CreateItem>"
        ))
    );
    assert!(!sent[1].contains("<t:Body"));
}

/// A delete moves the item to Deleted Items and tells nobody; one already
/// gone counts as deleted, any other refusal is the server's sentence.
#[tokio::test]
async fn a_delete_tells_nobody_and_a_gone_event_counts_as_deleted() {
    let answer = |message: String| built_answer("DeleteItem", &message);
    let backend = backend(
        ScriptedFetch::default()
            .answer("DeleteItem", &answer(success_message("DeleteItem", "")))
            .answer(
                "DeleteItem",
                &answer(error_message(
                    "DeleteItem",
                    "ErrorItemNotFound",
                    "The specified object was not found in the store.",
                )),
            )
            .answer(
                "DeleteItem",
                &answer(error_message(
                    "DeleteItem",
                    "ErrorAccessDenied",
                    "Access is denied.",
                )),
            ),
    );
    backend.delete_event(SINGLE_ID).await.unwrap();
    backend.delete_event(SINGLE_ID).await.unwrap();
    let err = backend.delete_event(SINGLE_ID).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Exchange server refused DeleteItem: ErrorAccessDenied: Access is denied."
    );
    assert_eq!(
        backend.fetch().sent()[0],
        envelope(&format!(
            "<m:DeleteItem DeleteType=\"MoveToDeletedItems\" SendMeetingCancellations=\"SendToNone\">\
             <m:ItemIds><t:ItemId Id=\"{SINGLE_ID}\"/></m:ItemIds></m:DeleteItem>"
        ))
    );
}

// ---------------------------------------------------------------------------
// Reminders
// ---------------------------------------------------------------------------

/// What the server answers a PerformReminderAction with, in the frame of
/// the spike's GetReminders answer (the response itself carries its class
/// and code): the items it changed, or its error.
fn reminder_answer(class: &str, code: &str, payload: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope \
         xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>\
         <PerformReminderActionResponse ResponseClass=\"{class}\" \
         xmlns=\"http://schemas.microsoft.com/exchange/services/2006/messages\">\
         <ResponseCode>{code}</ResponseCode>{payload}</PerformReminderActionResponse>\
         </s:Body></s:Envelope>"
    )
}

/// A dismissal and a snooze are reminder actions on the event's ItemId, an
/// occurrence's own included, the snooze carrying the new reminder time;
/// an event already gone has none to dismiss.
#[tokio::test]
async fn a_dismissal_and_a_snooze_are_reminder_actions_on_the_item() {
    let updated = format!(
        "<UpdatedItemIds><ItemId xmlns=\"http://schemas.microsoft.com/exchange/services/2006/types\" \
         Id=\"{OCCURRENCE_ID}\" ChangeKey=\"k\"/></UpdatedItemIds>"
    );
    let backend = backend(
        ScriptedFetch::default()
            .answer(
                "PerformReminderAction",
                &reminder_answer("Success", "NoError", &updated),
            )
            .answer(
                "PerformReminderAction",
                &reminder_answer("Success", "NoError", &updated),
            )
            .answer(
                "PerformReminderAction",
                &reminder_answer(
                    "Error",
                    "ErrorItemNotFound",
                    "<MessageText>The specified object was not found in the store.</MessageText>",
                ),
            )
            .answer(
                "PerformReminderAction",
                &reminder_answer(
                    "Error",
                    "ErrorInvalidRequest",
                    "<MessageText>The request is invalid.</MessageText>",
                ),
            ),
    );
    backend.dismiss_reminder(OCCURRENCE_ID).await.unwrap();
    let until = Utc.with_ymd_and_hms(2026, 11, 20, 8, 55, 0).unwrap();
    backend.snooze_reminder(OCCURRENCE_ID, until).await.unwrap();
    backend.dismiss_reminder(SINGLE_ID).await.unwrap();
    let err = backend.dismiss_reminder(SINGLE_ID).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "the Exchange server refused PerformReminderAction: ErrorInvalidRequest: The request is \
         invalid."
    );
    let sent = backend.fetch().sent();
    assert_eq!(
        sent[0],
        envelope(&format!(
            "<m:PerformReminderAction><m:ReminderItemActions><t:ReminderItemAction>\
             <t:ActionType>Dismiss</t:ActionType><t:ItemId Id=\"{OCCURRENCE_ID}\"/>\
             </t:ReminderItemAction></m:ReminderItemActions></m:PerformReminderAction>"
        ))
    );
    assert_eq!(
        sent[1],
        envelope(&format!(
            "<m:PerformReminderAction><m:ReminderItemActions><t:ReminderItemAction>\
             <t:ActionType>Snooze</t:ActionType><t:ItemId Id=\"{OCCURRENCE_ID}\"/>\
             <t:NewReminderTime>2026-11-20T08:55:00Z</t:NewReminderTime>\
             </t:ReminderItemAction></m:ReminderItemActions></m:PerformReminderAction>"
        ))
    );
}

/// Who an update tells: nobody for a change to the user's own reminder or
/// free/busy, only the attendees added or removed for a change to the list
/// alone (the user's own fields beside it telling nobody more), and every
/// attendee for anything else.
#[tokio::test]
async fn an_update_tells_only_whom_the_change_concerns() {
    let mut fetch = ScriptedFetch::default();
    for _ in 0..4 {
        fetch = fetch
            .answer("UpdateItem", &saved("UpdateItem", SINGLE_ID))
            .answer("GetItem", &get_item(&[SINGLE]));
    }
    let backend = backend(fetch);
    let own = EventPatch {
        reminder: Some(None),
        show_as: Some(ShowAs::Free),
        ..Default::default()
    };
    let list = EventPatch {
        attendees: Some(vec![person(
            "ada@example.com",
            None,
            AttendeeRole::Required,
        )]),
        ..Default::default()
    };
    let list_and_own = EventPatch {
        reminder: Some(Some(5)),
        ..list.clone()
    };
    let moved = EventPatch {
        time: Some(berlin(10, 11)),
        reminder: Some(Some(5)),
        ..list.clone()
    };
    for patch in [&own, &list, &list_and_own, &moved] {
        backend
            .update_event(CALENDAR_ID, SINGLE_ID, patch)
            .await
            .unwrap();
    }
    let told: Vec<String> = backend
        .fetch()
        .sent()
        .iter()
        .filter(|sent| sent.contains("<m:UpdateItem "))
        .map(|sent| {
            let key = "SendMeetingInvitationsOrCancellations=\"";
            let start = sent.find(key).unwrap() + key.len();
            sent[start..start + sent[start..].find('"').unwrap()].to_string()
        })
        .collect();
    assert_eq!(
        told,
        [
            "SendToNone",
            "SendToChangedAndSaveCopy",
            "SendToChangedAndSaveCopy",
            "SendToAllAndSaveCopy"
        ]
    );
}
