//! The EWS calendar writes (PERSO-256, `.agents/plans/calendar.md`, phase 3,
//! units 6 and 7).
//!
//! The spike ran no write against the server
//! (`.agents/research/2026-11-13-ews-spike.md`, "Still unknown"): every
//! envelope here is built from Microsoft's EWS schema documentation and is
//! unverified live, each marked so where it is built.
//!
//! - An answer to an invitation is a response object (`AcceptItem`,
//!   `TentativelyAcceptItem`, `DeclineItem`) made through `CreateItem` on the
//!   event's `ItemId`, with the comment as its body. `SendAndSaveCopy` tells
//!   the organizer, `SaveOnly` records the answer without telling anybody.
//!   A tentative or a decline may carry `ProposedStart` and `ProposedEnd`, a
//!   new time the organizer receives as a proposal (`Exchange2013` and
//!   later). A `SaveOnly` answer may leave the unsent response in Drafts:
//!   when the CreateItem answer names the item it saved, it is deleted for
//!   good (`HardDelete`); whether the server names it is unverified live.
//!
//! - A create is a `CalendarItem` through `CreateItem` into the calendar's
//!   folder, every field in the schema's order, which invites the attendees
//!   (`SendMeetingInvitations`); a room is a `Resources` attendee.
//! - An update is one `UpdateItem` with a `SetItemField` per field the patch
//!   sets and a `DeleteItemField` per field it empties, overwriting a change
//!   made meanwhile as Graph's PATCH does (`AlwaysOverwrite`), and telling
//!   whom the change concerns ([`update_notice`]). A change to one occurrence goes to the
//!   occurrence's own ItemId, which makes it an exception; a change to the
//!   series goes to the master's.
//! - A reminder dismissed or snoozed is a `PerformReminderAction`
//!   (`Exchange2013` and later) on the event's ItemId, an occurrence's own
//!   included, the snooze with its new reminder time. Turning
//!   `ReminderIsSet` off through UpdateItem would dismiss every later
//!   occurrence of a series and make an exception of an occurrence, where
//!   the reminder action dismisses the one reminder, as Outlook does. An
//!   event already gone has no reminder left, which counts as done.
//! - A cancel is a `CancelCalendarItem` response object on a meeting the
//!   user organizes, with the comment as its body; a delete is a
//!   `DeleteItem` into Deleted Items that tells nobody. An event already
//!   gone (`ErrorItemNotFound`) counts as cancelled or deleted.
//!
//! A create or an update reads the item back with GetItem and answers it
//! converted: a series master as its series, an occurrence hung from its
//! master.
//!
//! EWS has no transaction id. A create stamps its own as an extended
//! property ([`parse::TRANSACTION_PROPERTY`]), which every GetItem reads
//! back into [`Event::transaction_id`], so the store finds the event as it
//! finds a Graph one. Before it creates, a create looks for an item of the
//! calendar carrying its transaction id (FindItem on the property): one is
//! the event a first try made whose answer was lost, which the create reads
//! and answers as it is when it already carries the create's fields
//! ([`EventInput::matches`], and the rule of a series), and otherwise
//! updates with them instead of making a second one, as Graph's
//! `ErrorDuplicateTransactionId` adoption does; an unchanged retry so mails
//! nobody an update.
//!
//! Times go out as UTC instants with the event's zone as its Windows id
//! (`StartTimeZone`, `EndTimeZone`), which Exchange keeps the meeting in: an
//! all-day event's midnights are those of its zone. A zone with no Windows
//! id is refused with a sentence, never sent as UTC, since Exchange would
//! then move a recurring or all-day event across a daylight-saving change.
//! An online meeting is refused: on-premises Exchange makes no Teams link.
//!
//! Every text the user typed goes out escaped ([`xml::escape`]).
//!
//! [`Event::transaction_id`]: crate::calendar_model::Event::transaction_id

use std::str::FromStr;

use anyhow::anyhow;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use log::{debug, warn};

use super::fetch::EwsFetch;
use super::parse::ItemType;
use super::{
    answer_error, folder_id, parse, utc_text, xml, EwsCalendarBackend, ItemRef, ITEM_NOT_FOUND,
};
use crate::calendar_backend::{BackendResult, CalendarBackendError, Response, Written};
use crate::calendar_model::{
    AttendeeRole, Calendar, EventTime, Frequency, Recurrence, Sensitivity, ShowAs, WeekIndex,
    Weekday,
};
use crate::calendar_write::{AttendeeInput, EventInput, EventPatch, ProposedTime};

/// The `ExtendedFieldURI` of the transaction property, as a GetItem asks for
/// it and a FindItem restricts on it.
pub(super) fn transaction_field() -> String {
    format!(
        "<t:ExtendedFieldURI DistinguishedPropertySetId=\"PublicStrings\" \
         PropertyName=\"{}\" PropertyType=\"String\"/>",
        parse::TRANSACTION_PROPERTY
    )
}

/// A plain-text `Body` holding `text`, escaped.
fn body_element(text: &str) -> String {
    format!("<t:Body BodyType=\"Text\">{}</t:Body>", xml::escape(text))
}

/// The `CreateItem` that answers the invitation `event`.
///
/// Built from the schema documentation, unverified live: the response
/// object's children go in the schema's order (`Body`, `ReferenceItemId`,
/// then `ProposedStart` and `ProposedEnd`), and a proposal rides on a
/// tentative or a decline only, as the schema puts it on
/// `MeetingRegistrationResponseObjectType` and Graph takes it.
pub(super) fn response_body(
    event: &str,
    response: Response,
    comment: Option<&str>,
    send_response: bool,
    proposed: Option<&ProposedTime>,
) -> String {
    let element = match response {
        Response::Accept => "AcceptItem",
        Response::Tentative => "TentativelyAcceptItem",
        Response::Decline => "DeclineItem",
    };
    let disposition = if send_response {
        "SendAndSaveCopy"
    } else {
        "SaveOnly"
    };
    let body = comment
        .filter(|comment| !comment.trim().is_empty())
        .map(body_element)
        .unwrap_or_default();
    let proposal = proposed
        .filter(|_| response != Response::Accept)
        .map(|proposed| {
            format!(
                "<t:ProposedStart>{}</t:ProposedStart><t:ProposedEnd>{}</t:ProposedEnd>",
                utc_text(proposed.slot.start),
                utc_text(proposed.slot.end)
            )
        })
        .unwrap_or_default();
    format!(
        "<m:CreateItem MessageDisposition=\"{disposition}\"><m:Items><t:{element}>{body}\
         <t:ReferenceItemId Id=\"{}\"/>{proposal}</t:{element}></m:Items></m:CreateItem>",
        xml::escape(event)
    )
}

/// `<t:{name}>{text}</t:{name}>`, the text escaped.
fn text_element(name: &str, text: &str) -> String {
    format!("<t:{name}>{}</t:{name}>", xml::escape(text))
}

/// The refusal of a write Exchange cannot hold, as one sentence.
fn refusal(sentence: String) -> CalendarBackendError {
    CalendarBackendError::Other(anyhow!(sentence))
}

/// The Windows id of the IANA zone `zone`, the form EWS holds a meeting's
/// zone in; a zone without one is refused rather than sent as UTC.
fn windows_zone(zone: &str) -> BackendResult<&'static str> {
    crate::windows_zones::iana_to_windows_zone(zone).ok_or_else(|| {
        refusal(format!(
            "Exchange holds an event's time zone by its Windows name, and {} has none; give the \
             event another zone",
            zone.trim()
        ))
    })
}

/// An event's time as EWS takes it: its bounds as instants, whether it is
/// all-day, and the Windows id of its zone.
struct Span {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    all_day: bool,
    windows: &'static str,
}

/// The IANA zone an all-day write's midnights fall in: the write's own,
/// else the host's.
fn all_day_zone(zone: Option<&str>) -> String {
    match zone.map(str::trim).filter(|zone| !zone.is_empty()) {
        Some(zone) => zone.to_string(),
        None => crate::calendar_model::local_zone().name().to_string(),
    }
}

/// `time` as a [`Span`]; `zone` places an all-day time's midnights.
fn span_of(time: &EventTime, zone: Option<&str>) -> BackendResult<Span> {
    match time {
        EventTime::Timed { start, end, zone } => Ok(Span {
            start: *start,
            end: *end,
            all_day: false,
            windows: windows_zone(zone)?,
        }),
        EventTime::AllDay { .. } => {
            let zone = all_day_zone(zone);
            let windows = windows_zone(&zone)?;
            let tz = Tz::from_str(&zone)
                .map_err(|_| refusal(format!("{zone} is not a time zone this build knows")))?;
            let (first, after) = time
                .all_day_midnights()
                .expect("an all-day time has its midnights");
            let instant = |local: chrono::NaiveDateTime| {
                local
                    .and_local_timezone(tz)
                    .earliest()
                    .map(|at| at.with_timezone(&Utc))
                    .ok_or_else(|| refusal(format!("midnight of {local} does not exist in {zone}")))
            };
            Ok(Span {
                start: instant(first)?,
                end: instant(after)?,
                all_day: true,
                windows,
            })
        }
    }
}

/// The first day a rule without its own start counts from: the event's
/// first day in its zone.
fn first_day_of(time: &EventTime, zone: Option<&str>) -> NaiveDate {
    let tz = time
        .zone()
        .or_else(|| Tz::from_str(&all_day_zone(zone)).ok())
        .unwrap_or(chrono_tz::UTC);
    crate::calendar_write::first_day(time, tz)
}

fn sensitivity_name(sensitivity: Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Normal => "Normal",
        Sensitivity::Personal => "Personal",
        Sensitivity::Private => "Private",
        Sensitivity::Confidential => "Confidential",
    }
}

fn free_busy_name(show_as: ShowAs) -> &'static str {
    match show_as {
        ShowAs::Free => "Free",
        ShowAs::Tentative => "Tentative",
        ShowAs::Busy => "Busy",
        ShowAs::Oof => "OOF",
        ShowAs::WorkingElsewhere => "WorkingElsewhere",
        ShowAs::Unknown => "NoData",
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn week_index_name(index: WeekIndex) -> &'static str {
    match index {
        WeekIndex::First => "First",
        WeekIndex::Second => "Second",
        WeekIndex::Third => "Third",
        WeekIndex::Fourth => "Fourth",
        WeekIndex::Last => "Last",
    }
}

/// The one `DaysOfWeek` value a relative pattern takes: a day, or the group
/// (`Day`, `Weekday`, `WeekendDay`) the rule's days make up.
fn relative_day(days: &[Weekday]) -> BackendResult<&'static str> {
    let mut sorted = days.to_vec();
    sorted.sort_by_key(|day| Weekday::ALL.iter().position(|d| d == day));
    sorted.dedup();
    match sorted.as_slice() {
        [day] => Ok(day.name()),
        all if all == Weekday::ALL => Ok("Day"),
        weekdays if weekdays == &Weekday::ALL[..5] => Ok("Weekday"),
        weekend if weekend == &Weekday::ALL[5..] => Ok("WeekendDay"),
        _ => Err(refusal(
            "Exchange repeats a monthly or yearly series on one weekday, on every day, on \
             weekdays or on weekend days, not on another set of days"
                .to_string(),
        )),
    }
}

/// The `Recurrence` of `rule`, counted from `first` when the rule names no
/// start. Built from the schema documentation, unverified live: each
/// pattern names only the children its type has, in the schema's order,
/// and a yearly pattern has no interval.
fn recurrence_element(rule: &Recurrence, first: NaiveDate) -> BackendResult<String> {
    let interval = format!("<t:Interval>{}</t:Interval>", rule.interval.max(1));
    let month = || -> BackendResult<String> {
        let month = rule
            .month
            .and_then(|month| MONTHS.get(month.checked_sub(1)? as usize))
            .ok_or_else(|| refusal("a yearly series names its month".to_string()))?;
        Ok(format!("<t:Month>{month}</t:Month>"))
    };
    let day_of_month = || {
        format!(
            "<t:DayOfMonth>{}</t:DayOfMonth>",
            rule.day_of_month
                .unwrap_or_else(|| chrono::Datelike::day(&first))
        )
    };
    let relative = |index: WeekIndex| -> BackendResult<String> {
        Ok(format!(
            "<t:DaysOfWeek>{}</t:DaysOfWeek><t:DayOfWeekIndex>{}</t:DayOfWeekIndex>",
            relative_day(&rule.weekdays)?,
            week_index_name(index)
        ))
    };
    let pattern = match (rule.frequency, rule.week_index) {
        (Frequency::Daily, _) => format!("<t:DailyRecurrence>{interval}</t:DailyRecurrence>"),
        (Frequency::Weekly, _) => {
            let days: Vec<&str> = if rule.weekdays.is_empty() {
                vec![Weekday::of_date(first).name()]
            } else {
                rule.weekdays.iter().map(|day| day.name()).collect()
            };
            format!(
                "<t:WeeklyRecurrence>{interval}<t:DaysOfWeek>{}</t:DaysOfWeek>\
                 <t:FirstDayOfWeek>{}</t:FirstDayOfWeek></t:WeeklyRecurrence>",
                days.join(" "),
                rule.first_day_of_week.unwrap_or(Weekday::Monday).name()
            )
        }
        (Frequency::Monthly, Some(index)) => format!(
            "<t:RelativeMonthlyRecurrence>{interval}{}</t:RelativeMonthlyRecurrence>",
            relative(index)?
        ),
        (Frequency::Monthly, None) => format!(
            "<t:AbsoluteMonthlyRecurrence>{interval}{}</t:AbsoluteMonthlyRecurrence>",
            day_of_month()
        ),
        (Frequency::Yearly, _) if rule.interval > 1 => {
            return Err(refusal(format!(
                "Exchange repeats a yearly series every year only, not every {} years",
                rule.interval
            )))
        }
        (Frequency::Yearly, Some(index)) => format!(
            "<t:RelativeYearlyRecurrence>{}{}</t:RelativeYearlyRecurrence>",
            relative(index)?,
            month()?
        ),
        (Frequency::Yearly, None) => format!(
            "<t:AbsoluteYearlyRecurrence>{}{}</t:AbsoluteYearlyRecurrence>",
            day_of_month(),
            month()?
        ),
    };
    let start = format!("<t:StartDate>{}</t:StartDate>", rule.start.unwrap_or(first));
    let range = match (rule.until, rule.count) {
        (Some(until), _) => format!(
            "<t:EndDateRecurrence>{start}<t:EndDate>{until}</t:EndDate></t:EndDateRecurrence>"
        ),
        (None, Some(count)) => format!(
            "<t:NumberedRecurrence>{start}<t:NumberOfOccurrences>{count}\
             </t:NumberOfOccurrences></t:NumberedRecurrence>"
        ),
        (None, None) => format!("<t:NoEndRecurrence>{start}</t:NoEndRecurrence>"),
    };
    Ok(format!("<t:Recurrence>{pattern}{range}</t:Recurrence>"))
}

/// One `Attendee`: its mailbox's name, when it has one, and address.
fn attendee_element(attendee: &AttendeeInput) -> String {
    let name = attendee
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| text_element("Name", name))
        .unwrap_or_default();
    format!(
        "<t:Attendee><t:Mailbox>{name}{}</t:Mailbox></t:Attendee>",
        text_element("EmailAddress", attendee.email.trim())
    )
}

/// The three attendee lists by role, each with its field and its element,
/// the element `None` for an empty list.
fn attendee_lists(attendees: &[AttendeeInput]) -> [(&'static str, Option<String>); 3] {
    let list = |name: &str, role: AttendeeRole| {
        let members: String = attendees
            .iter()
            .filter(|attendee| attendee.role == role)
            .map(attendee_element)
            .collect();
        (!members.is_empty()).then(|| format!("<t:{name}>{members}</t:{name}>"))
    };
    [
        (
            "RequiredAttendees",
            list("RequiredAttendees", AttendeeRole::Required),
        ),
        (
            "OptionalAttendees",
            list("OptionalAttendees", AttendeeRole::Optional),
        ),
        ("Resources", list("Resources", AttendeeRole::Resource)),
    ]
}

/// The reminder fields: on with its minutes, or off.
fn reminder_elements(reminder: Option<u32>) -> Vec<(&'static str, String)> {
    match reminder {
        Some(minutes) => vec![
            (
                "item:ReminderIsSet",
                "<t:ReminderIsSet>true</t:ReminderIsSet>".to_string(),
            ),
            (
                "item:ReminderMinutesBeforeStart",
                format!("<t:ReminderMinutesBeforeStart>{minutes}</t:ReminderMinutesBeforeStart>"),
            ),
        ],
        None => vec![(
            "item:ReminderIsSet",
            "<t:ReminderIsSet>false</t:ReminderIsSet>".to_string(),
        )],
    }
}

/// The time fields of `span`, zones first so the instants are read in them.
fn time_elements(span: &Span) -> Vec<(&'static str, String)> {
    vec![
        (
            "calendar:StartTimeZone",
            format!("<t:StartTimeZone Id=\"{}\"/>", span.windows),
        ),
        (
            "calendar:EndTimeZone",
            format!("<t:EndTimeZone Id=\"{}\"/>", span.windows),
        ),
        (
            "calendar:Start",
            format!("<t:Start>{}</t:Start>", utc_text(span.start)),
        ),
        (
            "calendar:End",
            format!("<t:End>{}</t:End>", utc_text(span.end)),
        ),
        (
            "calendar:IsAllDayEvent",
            format!("<t:IsAllDayEvent>{}</t:IsAllDayEvent>", span.all_day),
        ),
    ]
}

/// The refusal of an online meeting, which on-premises Exchange cannot make.
fn no_online_meeting(account: &str) -> CalendarBackendError {
    refusal(format!(
        "the EWS calendar of {account} cannot add an online meeting: on-premises Exchange makes \
         no Teams link"
    ))
}

/// The `CreateItem` of `input` into `calendar`, stamped with
/// `transaction_id` when there is one.
///
/// Built from the schema documentation, unverified live: the item's fields
/// go in the order of `ItemType` then `CalendarItemType` (the order GetItem
/// answers them in), which the schema requires.
pub(super) fn create_body(
    calendar: &str,
    input: &EventInput,
    transaction_id: &str,
) -> BackendResult<String> {
    let span = span_of(&input.time, input.zone.as_deref())?;
    let recurrence = input
        .recurrence
        .as_ref()
        .map(|rule| recurrence_element(rule, first_day_of(&input.time, input.zone.as_deref())))
        .transpose()?;
    let mut item = text_element("Subject", &input.summary);
    if let Some(sensitivity) = input.sensitivity {
        item += &text_element("Sensitivity", sensitivity_name(sensitivity));
    }
    if let Some(body) = input.body.as_deref() {
        item += &body_element(body);
    }
    if let Some(reminder) = input.reminder {
        for (_, element) in reminder_elements(reminder) {
            item += &element;
        }
    }
    if !transaction_id.trim().is_empty() {
        item += &format!(
            "<t:ExtendedProperty>{}{}</t:ExtendedProperty>",
            transaction_field(),
            text_element("Value", transaction_id)
        );
    }
    let times = time_elements(&span);
    let (zones, bounds) = times.split_at(2);
    for (_, element) in bounds {
        item += element;
    }
    if let Some(show_as) = input.show_as {
        item += &text_element("LegacyFreeBusyStatus", free_busy_name(show_as));
    }
    if let Some(location) = input.location.as_deref() {
        item += &text_element("Location", location);
    }
    for (_, list) in attendee_lists(&input.attendees) {
        item += &list.unwrap_or_default();
    }
    item += &recurrence.unwrap_or_default();
    for (_, element) in zones {
        item += element;
    }
    if let Some(allow) = input.allow_new_time_proposals {
        item += &format!("<t:AllowNewTimeProposal>{allow}</t:AllowNewTimeProposal>");
    }
    let invitations = if input.attendees.is_empty() {
        "SendToNone"
    } else {
        "SendToAllAndSaveCopy"
    };
    Ok(format!(
        "<m:CreateItem SendMeetingInvitations=\"{invitations}\">\
         <m:SavedItemFolderId>{}</m:SavedItemFolderId>\
         <m:Items><t:CalendarItem>{item}</t:CalendarItem></m:Items></m:CreateItem>",
        folder_id(calendar)
    ))
}

/// Whom an update of `patch` tells (`SendMeetingInvitationsOrCancellations`):
/// nobody when it changes only the user's own reminder or free/busy, which
/// no attendee holds; only the attendees added or removed when it changes the
/// list and otherwise only those own fields; every attendee otherwise.
///
/// The rule reads the patch alone. On an event without attendees Exchange
/// has nobody to tell whatever it is sent; on a meeting the user attends, the
/// daemon lets through only the reminder and the free/busy, which tell
/// nobody, so an attendee never sends the organizer's attendees an update.
pub(super) fn update_notice(patch: &EventPatch) -> &'static str {
    let own_only = EventPatch {
        reminder: None,
        show_as: None,
        attendees: None,
        ..patch.clone()
    }
    .is_empty();
    match (own_only, patch.attendees.is_some()) {
        (true, false) => "SendToNone",
        (true, true) => "SendToChangedAndSaveCopy",
        (false, _) => "SendToAllAndSaveCopy",
    }
}

/// The `SetItemField` of the field `uri` to `element`.
fn set_field(uri: &str, element: &str) -> String {
    format!(
        "<t:SetItemField><t:FieldURI FieldURI=\"{uri}\"/><t:CalendarItem>{element}\
         </t:CalendarItem></t:SetItemField>"
    )
}

/// The `DeleteItemField` of the field `uri`.
fn delete_field(uri: &str) -> String {
    format!("<t:DeleteItemField><t:FieldURI FieldURI=\"{uri}\"/></t:DeleteItemField>")
}

/// The `Updates` of `patch`, one per field it names, in the order of the
/// item's fields; empty for an empty patch.
///
/// Built from the schema documentation, unverified live. A location or an
/// attendee list emptied is deleted; a description emptied is set to an
/// empty body, which every item has.
pub(super) fn patch_updates(patch: &EventPatch) -> BackendResult<String> {
    let span = patch
        .time
        .as_ref()
        .map(|time| span_of(time, patch.zone.as_deref()))
        .transpose()?;
    let recurrence = match &patch.recurrence {
        Some(rule) => {
            let first = match (rule.start, &patch.time) {
                (Some(start), _) => start,
                (None, Some(time)) => first_day_of(time, patch.zone.as_deref()),
                (None, None) => {
                    return Err(refusal(
                        "a new rule for an Exchange series names the day it starts".to_string(),
                    ))
                }
            };
            Some(recurrence_element(rule, first)?)
        }
        None => None,
    };
    let mut updates = String::new();
    if let Some(summary) = &patch.summary {
        updates += &set_field("item:Subject", &text_element("Subject", summary));
    }
    if let Some(sensitivity) = patch.sensitivity {
        updates += &set_field(
            "item:Sensitivity",
            &text_element("Sensitivity", sensitivity_name(sensitivity)),
        );
    }
    if let Some(body) = &patch.body {
        updates += &set_field(
            "item:Body",
            &body_element(body.as_deref().unwrap_or_default()),
        );
    }
    if let Some(reminder) = patch.reminder {
        for (uri, element) in reminder_elements(reminder) {
            updates += &set_field(uri, &element);
        }
    }
    if let Some(span) = &span {
        for (uri, element) in time_elements(span) {
            updates += &set_field(uri, &element);
        }
    }
    if let Some(show_as) = patch.show_as {
        updates += &set_field(
            "calendar:LegacyFreeBusyStatus",
            &text_element("LegacyFreeBusyStatus", free_busy_name(show_as)),
        );
    }
    if let Some(location) = &patch.location {
        match location.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
            Some(location) => {
                updates += &set_field("calendar:Location", &text_element("Location", location))
            }
            None => updates += &delete_field("calendar:Location"),
        }
    }
    if let Some(attendees) = &patch.attendees {
        for (name, list) in attendee_lists(attendees) {
            let uri = format!("calendar:{name}");
            match list {
                Some(element) => updates += &set_field(&uri, &element),
                None => updates += &delete_field(&uri),
            }
        }
    }
    if let Some(recurrence) = &recurrence {
        updates += &set_field("calendar:Recurrence", recurrence);
    }
    if let Some(allow) = patch.allow_new_time_proposals {
        updates += &set_field(
            "calendar:AllowNewTimeProposal",
            &format!("<t:AllowNewTimeProposal>{allow}</t:AllowNewTimeProposal>"),
        );
    }
    Ok(updates)
}

impl<F: EwsFetch> EwsCalendarBackend<F> {
    /// Answer the invitation `event` ([`response_body`]): the server's one
    /// response message, or its refusal as a sentence.
    pub(super) async fn send_response(
        &self,
        event: &str,
        response: Response,
        comment: Option<&str>,
        send_response: bool,
        proposed: Option<&ProposedTime>,
    ) -> BackendResult<()> {
        let body = response_body(event, response, comment, send_response, proposed);
        let answer = self.call("CreateItem", &body).await?;
        let draft =
            parse::first_saved_id(&answer).map_err(|error| answer_error(&error, "CreateItem"))?;
        if let Some(draft) = draft.filter(|_| !send_response) {
            self.delete_draft(&draft).await;
        }
        Ok(())
    }

    /// Delete for good the unsent response `draft` an answer saved only left
    /// in Drafts. Built from the schema documentation, unverified live. A
    /// draft that cannot be deleted is logged, never the answer's failure:
    /// the calendar holds the answer already, and the draft is clutter the
    /// user can delete.
    async fn delete_draft(&self, draft: &str) {
        let request = format!(
            "<m:DeleteItem DeleteType=\"HardDelete\"><m:ItemIds><t:ItemId Id=\"{}\"/>\
             </m:ItemIds></m:DeleteItem>",
            xml::escape(draft)
        );
        let outcome = match self.call("DeleteItem", &request).await {
            Ok(answer) => parse::only_message(&answer, "DeleteItem")
                .map(drop)
                .map_err(|error| error.sentence("DeleteItem")),
            Err(error) => Err(error.to_string()),
        };
        if let Err(why) = outcome {
            warn!(
                "[calendar] {}: the unsent answer {draft} stays in Drafts: {why}",
                self.account
            );
        }
    }

    /// The calendar item `id` of `calendar` as the server holds it after a
    /// write: a series master as its series, an occurrence or an exception
    /// hung from its master.
    async fn written(&self, calendar: &str, id: &str) -> BackendResult<Written> {
        let answer = self
            .get_items(&[ItemRef::Item(id)])
            .await?
            .pop()
            .expect("get_items answers one message per item asked");
        let item = answer.map_err(|error| {
            anyhow!("the Exchange server refused GetItem after a write: {error}")
        })?;
        let unreadable = || anyhow!("the Exchange server answered a write with an unreadable item");
        if parse::item_type(&item) == Some(ItemType::RecurringMaster) {
            let series = self.series_of(calendar, &item).ok_or_else(unreadable)?;
            return Ok(Written::Series(Box::new(series)));
        }
        let mut events = vec![self.event_of(calendar, &item).ok_or_else(unreadable)?];
        self.resolve_series(calendar, &mut events).await?;
        Ok(Written::Event(Box::new(events.remove(0))))
    }

    /// The item of `calendar` a create with `transaction_id` made already,
    /// found by its transaction property. Built from the schema
    /// documentation, unverified live: a FindItem without a view lists single
    /// events and series masters, the items a create makes.
    async fn earlier_try(
        &self,
        calendar: &str,
        transaction_id: &str,
    ) -> BackendResult<Option<String>> {
        let body = format!(
            "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape>\
             </m:ItemShape><m:Restriction><t:IsEqualTo>{}<t:FieldURIOrConstant>\
             <t:Constant Value=\"{}\"/></t:FieldURIOrConstant></t:IsEqualTo></m:Restriction>\
             <m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>",
            transaction_field(),
            xml::escape(transaction_id),
            folder_id(calendar)
        );
        let answer = self.call("FindItem", &body).await?;
        let found = parse::found_ids(&answer).map_err(|error| answer_error(&error, "FindItem"))?;
        Ok(found.into_iter().next())
    }

    /// Create `input` in `calendar`, or update the event a first try with
    /// the same `transaction_id` made.
    pub(super) async fn create_item(
        &self,
        calendar: &Calendar,
        input: &EventInput,
        transaction_id: &str,
    ) -> BackendResult<Written> {
        if input.online_meeting {
            return Err(no_online_meeting(&self.account));
        }
        let body = create_body(&calendar.id, input, transaction_id)?;
        if !transaction_id.trim().is_empty() {
            if let Some(existing) = self.earlier_try(&calendar.id, transaction_id).await? {
                let held = self.written(&calendar.id, &existing).await?;
                let same = match &held {
                    Written::Event(event) => input.matches(event),
                    Written::Series(series) => {
                        series
                            .master
                            .as_deref()
                            .is_some_and(|master| input.matches(master))
                            && input.matches_rule(series.recurrence.as_ref())
                    }
                };
                if same {
                    debug!(
                        "[calendar] {}: transaction {transaction_id} made item {existing} as asked \
                         already",
                        self.account
                    );
                    return Ok(held);
                }
                debug!(
                    "[calendar] {}: transaction {transaction_id} made item {existing} already; \
                     updating it with this create's fields",
                    self.account
                );
                return self
                    .update_item(&calendar.id, &existing, &input.as_patch())
                    .await;
            }
        }
        let answer = self.call("CreateItem", &body).await?;
        let id = parse::saved_id(&answer, "CreateItem")
            .map_err(|error| answer_error(&error, "CreateItem"))?;
        self.written(&calendar.id, &id).await
    }

    /// Change the fields `patch` names on `event` of `calendar`. Built from
    /// the schema documentation, unverified live.
    pub(super) async fn update_item(
        &self,
        calendar: &str,
        event: &str,
        patch: &EventPatch,
    ) -> BackendResult<Written> {
        if patch.online_meeting == Some(true) {
            return Err(no_online_meeting(&self.account));
        }
        let updates = patch_updates(patch)?;
        if updates.is_empty() {
            return self.written(calendar, event).await;
        }
        let body = format!(
            "<m:UpdateItem MessageDisposition=\"SaveOnly\" ConflictResolution=\"AlwaysOverwrite\" \
             SendMeetingInvitationsOrCancellations=\"{}\"><m:ItemChanges>\
             <t:ItemChange><t:ItemId Id=\"{}\"/><t:Updates>{updates}</t:Updates></t:ItemChange>\
             </m:ItemChanges></m:UpdateItem>",
            update_notice(patch),
            xml::escape(event)
        );
        let answer = self.call("UpdateItem", &body).await?;
        let id = parse::saved_id(&answer, "UpdateItem")
            .map_err(|error| answer_error(&error, "UpdateItem"))?;
        self.written(calendar, &id).await
    }

    /// Settle `operation`'s answer, an item already gone counting as done.
    fn done_or_gone(&self, operation: &str, event: &str, answer: &str) -> BackendResult<()> {
        match parse::only_message(answer, operation) {
            Ok(_) => Ok(()),
            Err(parse::AnswerError::Refused(error)) if error.code == ITEM_NOT_FOUND => {
                debug!(
                    "[calendar] {}: item {event} is gone already; the {operation} counts as done",
                    self.account
                );
                Ok(())
            }
            Err(error) => Err(answer_error(&error, operation)),
        }
    }

    /// Cancel the meeting `event` the user organizes, telling every
    /// attendee. Built from the schema documentation, unverified live.
    pub(super) async fn cancel_item(
        &self,
        event: &str,
        comment: Option<&str>,
    ) -> BackendResult<()> {
        let body = comment
            .filter(|comment| !comment.trim().is_empty())
            .map(body_element)
            .unwrap_or_default();
        let request = format!(
            "<m:CreateItem MessageDisposition=\"SendAndSaveCopy\"><m:Items><t:CancelCalendarItem>\
             {body}<t:ReferenceItemId Id=\"{}\"/></t:CancelCalendarItem></m:Items></m:CreateItem>",
            xml::escape(event)
        );
        let answer = self.call("CreateItem", &request).await?;
        self.done_or_gone("CreateItem", event, &answer)
    }

    /// Dismiss the reminder of `event` (`Dismiss`), or snooze it until
    /// `until` (`Snooze`). Built from the schema documentation, unverified
    /// live; the item is named by its id alone, without the change key
    /// mp does not keep.
    pub(super) async fn reminder_action(
        &self,
        event: &str,
        until: Option<DateTime<Utc>>,
    ) -> BackendResult<()> {
        const OPERATION: &str = "PerformReminderAction";
        let (action, time) = match until {
            Some(until) => (
                "Snooze",
                format!("<t:NewReminderTime>{}</t:NewReminderTime>", utc_text(until)),
            ),
            None => ("Dismiss", String::new()),
        };
        let request = format!(
            "<m:{OPERATION}><m:ReminderItemActions><t:ReminderItemAction>\
             <t:ActionType>{action}</t:ActionType><t:ItemId Id=\"{}\"/>{time}\
             </t:ReminderItemAction></m:ReminderItemActions></m:{OPERATION}>",
            xml::escape(event)
        );
        let answer = self.call(OPERATION, &request).await?;
        match parse::bare_response(&answer, OPERATION) {
            Ok(_) => Ok(()),
            Err(parse::AnswerError::Refused(error)) if error.code == ITEM_NOT_FOUND => {
                debug!(
                    "[calendar] {}: item {event} is gone already; its reminder is no more",
                    self.account
                );
                Ok(())
            }
            Err(error) => Err(answer_error(&error, OPERATION)),
        }
    }

    /// Delete `event` into Deleted Items, telling nobody. Built from the
    /// schema documentation, unverified live.
    pub(super) async fn delete_item(&self, event: &str) -> BackendResult<()> {
        let request = format!(
            "<m:DeleteItem DeleteType=\"MoveToDeletedItems\" SendMeetingCancellations=\"SendToNone\">\
             <m:ItemIds><t:ItemId Id=\"{}\"/></m:ItemIds></m:DeleteItem>",
            xml::escape(event)
        );
        let answer = self.call("DeleteItem", &request).await?;
        self.done_or_gone("DeleteItem", event, &answer)
    }
}
