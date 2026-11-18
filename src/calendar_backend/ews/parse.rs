//! The EWS answers the calendar backend reads, and their conversion into the
//! neutral model (PERSO-256, `.agents/research/2026-11-13-ews-spike.md`,
//! "Operations").
//!
//! Every answer is a SOAP envelope whose body holds either a fault (HTTP 500,
//! a request the schema refuses) or one `<Operation>Response` with a
//! `ResponseMessages` list, each message carrying its `ResponseClass` and
//! `ResponseCode`; an operation error (an invalid sync state) is HTTP 200
//! with `ResponseClass="Error"`. [`messages`] reads that frame, and the
//! readers below read the payloads: folders, a `CalendarView`, a
//! `SyncFolderItems` page and the `GetItem` calendar items.
//!
//! An answer without the frame or the payload its operation must carry is
//! [`AnswerError::Malformed`], never an empty list: an empty calendar and a
//! truncated answer must not read alike.
//!
//! Times come in UTC (`Start`, `End`) with the event's Windows zone in
//! `StartTimeZoneId` (GetItem) or the `Id` of `StartTimeZone` (FindItem),
//! mapped to IANA through [`crate::windows_zones`]; a zone the table lacks
//! (`Customized Time Zone`) is labelled UTC with the instants exact. An
//! all-day event's `Start` and `End` are its local midnights in UTC, read
//! back into its zone, or from `StartWallClock` when the zone is unknown.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use chrono_tz::Tz;

use super::xml::{self, Element};
use crate::calendar_model::{
    Attendee, AttendeeRole, Calendar, Event, EventKind, EventTime, Frequency, OwnResponse, Person,
    Recurrence, ResponseStatus, Sensitivity, ShowAs, WeekIndex, Weekday,
};

/// A response message the server marked as failed, or a SOAP fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseError {
    /// `ErrorInvalidSyncStateData`, `ErrorItemNotFound`, `ErrorInvalidRequest`.
    pub code: String,
    /// The server's `MessageText` (or the fault's `faultstring`).
    pub message: String,
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message.trim() {
            "" => write!(f, "{}", self.code),
            message => write!(f, "{}: {message}", self.code),
        }
    }
}

/// Why an answer gave nothing to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerError {
    /// A SOAP fault: the server refused the request as a whole.
    Fault(ResponseError),
    /// The operation's (only) response message is an error.
    Refused(ResponseError),
    /// Not the answer the operation must give: not XML, cut short, or
    /// missing the elements every such answer carries.
    Malformed(String),
}

impl AnswerError {
    /// The sentence a client shows, naming the operation.
    pub fn sentence(&self, operation: &str) -> String {
        match self {
            AnswerError::Fault(error) | AnswerError::Refused(error) => {
                format!("the Exchange server refused {operation}: {error}")
            }
            AnswerError::Malformed(why) => format!("unreadable EWS answer to {operation}: {why}"),
        }
    }
}

pub fn malformed(why: impl Into<String>) -> AnswerError {
    AnswerError::Malformed(why.into())
}

/// The fault a SOAP `body` carries, the server's refusal of the request as a
/// whole, with its `ResponseCode` (or `faultcode`) and `faultstring`.
pub fn fault_of(body: &Element) -> Option<AnswerError> {
    let fault = body.child("Fault")?;
    let code = fault
        .at(&["detail", "ResponseCode"])
        .map(|code| code.text.trim().to_string())
        .filter(|code| !code.is_empty())
        .or_else(|| {
            fault
                .text_of("faultcode")
                .map(|code| code.rsplit(':').next().unwrap_or(code).to_string())
        })
        .unwrap_or_else(|| "SOAP fault".to_string());
    let message = fault.text_of("faultstring").unwrap_or_default().to_string();
    Some(AnswerError::Fault(ResponseError { code, message }))
}

/// One response message: its payload element, or the error it carries.
pub type Message = Result<Element, ResponseError>;

/// The response messages of `operation`'s answer `body`, in order.
pub fn messages(body: &str, operation: &str) -> Result<Vec<Message>, AnswerError> {
    let root = xml::parse(body).map_err(|err| malformed(format!("{err:#}")))?;
    if root.name != "Envelope" {
        return Err(malformed(format!(
            "a <{}> where a SOAP envelope was due",
            root.name
        )));
    }
    let body = root
        .child("Body")
        .ok_or_else(|| malformed("no SOAP body"))?;
    if let Some(fault) = fault_of(body) {
        return Err(fault);
    }
    let response_name = format!("{operation}Response");
    let response = body
        .child(&response_name)
        .ok_or_else(|| malformed(format!("no {response_name}")))?;
    let list = response
        .child("ResponseMessages")
        .ok_or_else(|| malformed("no ResponseMessages"))?;
    let message_name = format!("{operation}ResponseMessage");
    let found: Vec<Message> = list
        .children_named(&message_name)
        .map(|message| match message.attr("ResponseClass") {
            Some("Success") => Ok(message.clone()),
            _ => Err(ResponseError {
                code: message
                    .text_of("ResponseCode")
                    .unwrap_or("an error without a ResponseCode")
                    .to_string(),
                message: message
                    .text_of("MessageText")
                    .unwrap_or_default()
                    .to_string(),
            }),
        })
        .collect();
    if found.is_empty() {
        return Err(malformed(format!("no {message_name}")));
    }
    Ok(found)
}

/// The answer of an operation whose response carries its class and code
/// itself, with no list of messages (GetReminders, PerformReminderAction):
/// the response element, or its error as [`AnswerError::Refused`].
pub fn bare_response(body: &str, operation: &str) -> Result<Element, AnswerError> {
    let root = xml::parse(body).map_err(|err| malformed(format!("{err:#}")))?;
    let body = root
        .child("Body")
        .ok_or_else(|| malformed("no SOAP body"))?;
    if let Some(fault) = fault_of(body) {
        return Err(fault);
    }
    let name = format!("{operation}Response");
    let response = body
        .child(&name)
        .ok_or_else(|| malformed(format!("no {name}")))?;
    match response.attr("ResponseClass") {
        Some("Success") => Ok(response.clone()),
        _ => Err(AnswerError::Refused(ResponseError {
            code: response
                .text_of("ResponseCode")
                .unwrap_or("an error without a ResponseCode")
                .to_string(),
            message: response
                .text_of("MessageText")
                .unwrap_or_default()
                .to_string(),
        })),
    }
}

/// The single response message of `operation`'s answer, its error as
/// [`AnswerError::Refused`].
pub fn only_message(body: &str, operation: &str) -> Result<Element, AnswerError> {
    let mut found = messages(body, operation)?;
    if found.len() != 1 {
        return Err(malformed(format!(
            "{} response messages where one was due",
            found.len()
        )));
    }
    found.remove(0).map_err(AnswerError::Refused)
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

/// A calendar folder as GetFolder and FindFolder name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    /// `EffectiveRights/CreateContents`, when the answer carries it.
    pub can_create: Option<bool>,
}

fn folder_of(folder: &Element) -> Result<Folder, AnswerError> {
    let id = folder
        .child("FolderId")
        .and_then(|id| id.attr("Id"))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| malformed("a calendar folder without a FolderId"))?;
    Ok(Folder {
        id: id.to_string(),
        name: folder
            .text_of("DisplayName")
            .unwrap_or_default()
            .to_string(),
        can_create: folder
            .child("EffectiveRights")
            .and_then(|rights| rights.flag("CreateContents")),
    })
}

/// The calendar folders of a GetFolder answer (`Folders`) or a FindFolder one
/// (`RootFolder/Folders`). A folder of another class is not a calendar and
/// is left out.
pub fn folders(body: &str, operation: &str) -> Result<Vec<Folder>, AnswerError> {
    let message = only_message(body, operation)?;
    let list = message
        .child("Folders")
        .or_else(|| message.at(&["RootFolder", "Folders"]))
        .ok_or_else(|| malformed("no Folders"))?;
    list.children_named("CalendarFolder")
        .map(folder_of)
        .collect()
}

/// The calendar folder of each GetFolder response message, in the order
/// the request named the folders, or the error the server answered for it.
pub fn folder_messages(body: &str) -> Result<Vec<Result<Folder, ResponseError>>, AnswerError> {
    messages(body, "GetFolder")?
        .into_iter()
        .map(|message| match message {
            Ok(message) => message
                .at(&["Folders", "CalendarFolder"])
                .ok_or_else(|| malformed("a GetFolder message without a CalendarFolder"))
                .and_then(folder_of)
                .map(Ok),
            Err(error) => Ok(Err(error)),
        })
        .collect()
}

/// A folder as the model's calendar: one of the user's own, which the user
/// may write to unless the server says otherwise.
pub fn calendar_of(account: &str, folder: &Folder, default_id: Option<&str>) -> Calendar {
    Calendar {
        account: account.to_string(),
        id: folder.id.clone(),
        name: folder.name.clone(),
        color: None,
        is_default: default_id == Some(folder.id.as_str()),
        can_edit: folder.can_create.unwrap_or(true),
        visible: true,
        online_meeting_providers: Vec::new(),
    }
}

/// The calendar folder of the mailbox `address`, which the user reads as a
/// delegate: named by the mailbox, never the default, and read-only unless
/// the server says the user may create items in it.
pub fn delegate_calendar_of(account: &str, address: &str, folder: &Folder) -> Calendar {
    Calendar {
        account: account.to_string(),
        id: folder.id.clone(),
        name: address.to_string(),
        color: None,
        is_default: false,
        can_edit: folder.can_create.unwrap_or(false),
        visible: true,
        online_meeting_providers: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// FindItem with a CalendarView
// ---------------------------------------------------------------------------

/// What an item is to its series (`CalendarItemType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemType {
    Single,
    Occurrence,
    Exception,
    RecurringMaster,
}

impl ItemType {
    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "Single" => ItemType::Single,
            "Occurrence" => ItemType::Occurrence,
            "Exception" => ItemType::Exception,
            "RecurringMaster" => ItemType::RecurringMaster,
            _ => return None,
        })
    }

    fn kind(self) -> EventKind {
        match self {
            ItemType::Single => EventKind::Single,
            ItemType::Occurrence => EventKind::Occurrence,
            ItemType::Exception => EventKind::Exception,
            ItemType::RecurringMaster => EventKind::SeriesMaster,
        }
    }
}

/// One item of a CalendarView: what the window holds, without attendees or
/// body, which only GetItem answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewItem {
    pub id: String,
    pub kind: ItemType,
    pub uid: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// The Windows id of `StartTimeZone`.
    pub zone: Option<String>,
}

/// A CalendarView answer: its items, and whether they are all the window
/// holds (`IncludesLastItemInRange`); the server stops at its view limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarView {
    pub items: Vec<ViewItem>,
    pub complete: bool,
}

fn item_id(item: &Element) -> Option<&str> {
    item.child("ItemId")
        .and_then(|id| id.attr("Id"))
        .filter(|id| !id.is_empty())
}

fn instant(raw: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|at| at.with_timezone(&Utc))
        .map_err(|err| format!("unreadable time {raw:?}: {err}"))
}

fn instant_of(item: &Element, name: &str) -> Result<DateTime<Utc>, String> {
    instant(item.text_of(name).ok_or_else(|| format!("no {name}"))?)
}

fn view_item(item: &Element) -> Result<ViewItem, String> {
    let id = item_id(item).ok_or("a calendar item without an ItemId")?;
    let raw_kind = item.text_of("CalendarItemType").unwrap_or("Single");
    let kind = ItemType::parse(raw_kind)
        .ok_or_else(|| format!("unknown CalendarItemType {raw_kind:?}"))?;
    Ok(ViewItem {
        id: id.to_string(),
        kind,
        uid: item.text_of("UID").map(str::to_string),
        start: instant_of(item, "Start")?,
        end: instant_of(item, "End")?,
        zone: item
            .child("StartTimeZone")
            .and_then(|zone| zone.attr("Id"))
            .map(str::to_string),
    })
}

/// The items of a FindItem answer with a CalendarView.
pub fn calendar_view(body: &str) -> Result<CalendarView, AnswerError> {
    let message = only_message(body, "FindItem")?;
    let root = message
        .child("RootFolder")
        .ok_or_else(|| malformed("no RootFolder"))?;
    let complete = match root.attr("IncludesLastItemInRange") {
        Some("true") => true,
        Some("false") => false,
        _ => return Err(malformed("a RootFolder without IncludesLastItemInRange")),
    };
    let items = root.child("Items").ok_or_else(|| malformed("no Items"))?;
    let items = items
        .children_named("CalendarItem")
        .map(|item| view_item(item).map_err(malformed))
        .collect::<Result<_, _>>()?;
    Ok(CalendarView { items, complete })
}

// ---------------------------------------------------------------------------
// SyncFolderItems
// ---------------------------------------------------------------------------

/// One change of a SyncFolderItems page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A new or changed item, with its `CalendarItemType` when the page
    /// names it (single events and series masters only).
    Upsert { id: String, kind: Option<ItemType> },
    /// An item deleted, which names only its id.
    Delete { id: String },
}

/// One page of SyncFolderItems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPage {
    /// The state the next call passes.
    pub sync_state: String,
    /// Whether this page ends the changes (`IncludesLastItemInRange`).
    pub last: bool,
    pub changes: Vec<Change>,
}

/// A SyncFolderItems answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAnswer {
    Page(SyncPage),
    /// `ErrorInvalidSyncStateData`: the state passed is unknown to the
    /// server, which tells mp to start from nothing.
    InvalidState,
}

pub const INVALID_SYNC_STATE: &str = "ErrorInvalidSyncStateData";

fn change_of(change: &Element) -> Result<Option<Change>, AnswerError> {
    let item = || {
        change
            .children
            .iter()
            .find(|child| child.child("ItemId").is_some())
    };
    match change.name.as_str() {
        "Create" | "Update" => {
            let item =
                item().ok_or_else(|| malformed(format!("a {} without an item", change.name)))?;
            let id = item_id(item).ok_or_else(|| malformed("a changed item without an ItemId"))?;
            Ok(Some(Change::Upsert {
                id: id.to_string(),
                kind: item.text_of("CalendarItemType").and_then(ItemType::parse),
            }))
        }
        "Delete" => {
            let id = item_id(change).ok_or_else(|| malformed("a Delete without an ItemId"))?;
            Ok(Some(Change::Delete { id: id.to_string() }))
        }
        // A read flag on a calendar item changes nothing the model holds.
        "ReadFlagChange" => Ok(None),
        other => Err(malformed(format!("an unknown change <{other}>"))),
    }
}

/// A SyncFolderItems answer: a page of changes, or the invalid state.
pub fn sync_page(body: &str) -> Result<SyncAnswer, AnswerError> {
    let message = match only_message(body, "SyncFolderItems") {
        Err(AnswerError::Refused(error)) if error.code == INVALID_SYNC_STATE => {
            return Ok(SyncAnswer::InvalidState)
        }
        other => other?,
    };
    let sync_state = message
        .text_of("SyncState")
        .ok_or_else(|| malformed("no SyncState"))?
        .to_string();
    let last = message
        .flag("IncludesLastItemInRange")
        .ok_or_else(|| malformed("no IncludesLastItemInRange"))?;
    let changes = message
        .child("Changes")
        .ok_or_else(|| malformed("no Changes"))?;
    let changes = changes
        .children
        .iter()
        .filter_map(|change| change_of(change).transpose())
        .collect::<Result<_, _>>()?;
    Ok(SyncAnswer::Page(SyncPage {
        sync_state,
        last,
        changes,
    }))
}

// ---------------------------------------------------------------------------
// GetItem
// ---------------------------------------------------------------------------

/// The extended property a create stamps with its transaction id: a string
/// named so in the `PublicStrings` set, which GetItem asks for and a retried
/// create searches by.
pub const TRANSACTION_PROPERTY: &str = "MailypoppinsTransactionId";

/// The transaction id the item carries, when a create of mp stamped it.
fn transaction_of(item: &Element) -> Option<String> {
    item.children_named("ExtendedProperty")
        .find(|property| {
            property
                .child("ExtendedFieldURI")
                .and_then(|uri| uri.attr("PropertyName"))
                == Some(TRANSACTION_PROPERTY)
        })
        .and_then(|property| property.text_of("Value"))
        .map(str::to_string)
}

/// The calendar item of each GetItem response message, in the order the
/// request named them, or the error the server answered for it.
pub fn items(body: &str) -> Result<Vec<Message>, AnswerError> {
    messages(body, "GetItem")?
        .into_iter()
        .map(|message| match message {
            Ok(message) => message
                .at(&["Items", "CalendarItem"])
                .cloned()
                .map(Ok)
                .ok_or_else(|| malformed("a GetItem message without a CalendarItem")),
            Err(error) => Ok(Err(error)),
        })
        .collect()
}

/// The IANA zone a Windows zone id (or an IANA name) means.
pub fn zone_of(name: Option<&str>) -> Option<Tz> {
    let iana = crate::windows_zones::iana_or_windows(name?.trim())?;
    Tz::from_str(&iana).ok()
}

/// The item's own zone: `StartTimeZoneId`, else the `Id` of its
/// `StartTimeZone` definition.
fn item_zone(item: &Element) -> Option<Tz> {
    zone_of(item.text_of("StartTimeZoneId"))
        .or_else(|| zone_of(item.child("StartTimeZone").and_then(|zone| zone.attr("Id"))))
}

/// An all-day item's local midnight pair: its UTC bounds read in its zone,
/// else its wall clocks, else its UTC bounds as they are.
fn all_day_time(item: &Element, zone: Option<Tz>) -> Result<EventTime, String> {
    let (start, end) = (instant_of(item, "Start")?, instant_of(item, "End")?);
    if let Some(zone) = zone {
        let local = |at: DateTime<Utc>| at.with_timezone(&zone).naive_local();
        if let Ok(time) = EventTime::all_day_from_midnights(local(start), local(end)) {
            return Ok(time);
        }
    }
    let wall = |name: &str| -> Option<NaiveDateTime> {
        DateTime::parse_from_rfc3339(item.text_of(name)?)
            .ok()
            .map(|at| at.naive_local())
    };
    if let (Some(s), Some(e)) = (wall("StartWallClock"), wall("EndWallClock")) {
        if let Ok(time) = EventTime::all_day_from_midnights(s, e) {
            return Ok(time);
        }
    }
    EventTime::all_day_from_midnights(start.naive_utc(), end.naive_utc())
        .map_err(|err| err.to_string())
}

fn mailbox_person(mailbox: &Element) -> Option<Person> {
    // An X500 address (`RoutingType` EX, as FindItem names an organizer) is
    // no mail address.
    if mailbox.text_of("RoutingType") == Some("EX") {
        return None;
    }
    let email = mailbox.text_of("EmailAddress")?;
    Some(Person {
        name: mailbox
            .text_of("Name")
            .filter(|name| *name != email)
            .map(str::to_string),
        email: email.to_string(),
    })
}

fn attendee_status(raw: Option<&str>) -> ResponseStatus {
    match raw {
        Some("Accept") | Some("Organizer") => ResponseStatus::Accepted,
        Some("Tentative") => ResponseStatus::Tentative,
        Some("Decline") => ResponseStatus::Declined,
        _ => ResponseStatus::NeedsAction,
    }
}

fn attendees(item: &Element) -> Vec<Attendee> {
    [
        ("RequiredAttendees", AttendeeRole::Required),
        ("OptionalAttendees", AttendeeRole::Optional),
        ("Resources", AttendeeRole::Resource),
    ]
    .into_iter()
    .flat_map(|(list, role)| {
        item.child(list)
            .into_iter()
            .flat_map(|list| list.children_named("Attendee"))
            .filter_map(move |attendee| {
                let person = mailbox_person(attendee.child("Mailbox")?)?;
                Some(Attendee {
                    name: person.name,
                    email: person.email,
                    role,
                    status: attendee_status(attendee.text_of("ResponseType")),
                    proposed_time: None,
                })
            })
    })
    .collect()
}

fn own_response(raw: Option<&str>) -> OwnResponse {
    match raw {
        Some("Organizer") => OwnResponse::Organizer,
        Some("Accept") => OwnResponse::Accepted,
        Some("Tentative") => OwnResponse::Tentative,
        Some("Decline") => OwnResponse::Declined,
        Some("NoResponseReceived") => OwnResponse::NeedsAction,
        _ => OwnResponse::None,
    }
}

fn sensitivity(raw: Option<&str>) -> Sensitivity {
    match raw {
        Some("Personal") => Sensitivity::Personal,
        Some("Private") => Sensitivity::Private,
        Some("Confidential") => Sensitivity::Confidential,
        _ => Sensitivity::Normal,
    }
}

fn show_as(raw: Option<&str>) -> ShowAs {
    match raw {
        Some("Free") => ShowAs::Free,
        Some("Tentative") => ShowAs::Tentative,
        Some("Busy") => ShowAs::Busy,
        Some("OOF") => ShowAs::Oof,
        Some("WorkingElsewhere") => ShowAs::WorkingElsewhere,
        _ => ShowAs::Unknown,
    }
}

/// The description as plain text: an HTML body goes through the mail
/// path's HTML-to-text.
fn plain_body(item: &Element) -> Option<String> {
    let body = item.child("Body")?;
    let text = if body
        .attr("BodyType")
        .is_some_and(|kind| kind.eq_ignore_ascii_case("HTML"))
    {
        crate::parse::html_to_plain(&body.text)
    } else {
        body.text.replace("\r\n", "\n")
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// An occurrence's `RECURRENCE-ID`: its original start in UTC, or as a date
/// in the series' zone for an all-day series, the form an iMIP mail about it
/// names it in.
fn recurrence_id(item: &Element, time: &EventTime, zone: Option<Tz>) -> Result<String, String> {
    let raw = item
        .text_of("RecurrenceId")
        .or_else(|| item.text_of("OriginalStart"));
    let original = match raw {
        Some(raw) => instant(raw)?,
        None => match time {
            EventTime::Timed { start, .. } => *start,
            EventTime::AllDay { start, .. } => return Ok(start.to_string()),
        },
    };
    Ok(match time {
        EventTime::Timed { .. } => original.to_rfc3339_opts(SecondsFormat::Secs, true),
        EventTime::AllDay { .. } => original
            .with_timezone(&zone.unwrap_or(chrono_tz::UTC))
            .date_naive()
            .to_string(),
    })
}

/// A GetItem calendar item as the neutral model has it, in `calendar` of
/// `account`. The series master id of an occurrence is the backend's to set,
/// since an occurrence does not carry it.
pub fn calendar_event(account: &str, calendar: &str, item: &Element) -> Result<Event, String> {
    let id = item_id(item).ok_or("a calendar item without an ItemId")?;
    let raw_kind = item.text_of("CalendarItemType").unwrap_or("Single");
    let kind = ItemType::parse(raw_kind)
        .ok_or_else(|| format!("unknown CalendarItemType {raw_kind:?}"))?
        .kind();
    let zone = item_zone(item);
    let time = if item.flag("IsAllDayEvent").unwrap_or(false) {
        all_day_time(item, zone)?
    } else {
        EventTime::timed(
            instant_of(item, "Start")?,
            instant_of(item, "End")?,
            zone.map_or("UTC", |tz| tz.name()),
        )
        .map_err(|err| err.to_string())?
    };
    let recurrence_id = if kind.is_instance() {
        Some(recurrence_id(item, &time, zone)?)
    } else {
        None
    };
    let rule = recurrence_of(item);
    Ok(Event {
        account: account.to_string(),
        calendar: calendar.to_string(),
        id: id.to_string(),
        uid: item.text_of("UID").unwrap_or(id).to_string(),
        recurrence_id,
        series_master_id: None,
        kind,
        summary: item.text_of("Subject").unwrap_or_default().to_string(),
        time,
        location: item.text_of("Location").map(str::to_string),
        body: plain_body(item),
        organizer: item.at(&["Organizer", "Mailbox"]).and_then(mailbox_person),
        attendees: attendees(item),
        own_response: own_response(item.text_of("MyResponseType")),
        recurrence: rule.as_ref().map(Recurrence::describe),
        reminder: item
            .text_of("ReminderMinutesBeforeStart")
            .filter(|_| item.flag("ReminderIsSet").unwrap_or(false))
            .and_then(|minutes| minutes.parse().ok()),
        online_meeting_url: item.text_of("JoinOnlineMeetingUrl").map(str::to_string),
        is_online_meeting: item.flag("IsOnlineMeeting").unwrap_or(false),
        online_meeting_provider: None,
        sensitivity: sensitivity(item.text_of("Sensitivity")),
        show_as: show_as(item.text_of("LegacyFreeBusyStatus")),
        cancelled: item.flag("IsCancelled").unwrap_or(false),
        is_organizer: item
            .flag("IsOrganizer")
            .unwrap_or_else(|| item.text_of("MyResponseType") == Some("Organizer")),
        last_modified: item
            .text_of("LastModifiedTime")
            .and_then(|raw| instant(raw).ok()),
        web_link: item
            .text_of("WebClientReadFormQueryString")
            .map(str::to_string),
        allow_new_time_proposals: item.flag("AllowNewTimeProposal").unwrap_or(true),
        response_requested: item.flag("IsResponseRequested").unwrap_or(true),
        transaction_id: transaction_of(item),
    })
}

/// The ItemIds a FindItem without a view answers, items of any kind, in the
/// order of the folders it searched: the server answers one response
/// message per parent folder (TUM's server on 2026-11-18,
/// `finditem_message_id_two_folders.xml`), and a folder it refuses fails
/// the whole answer.
pub fn found_ids(body: &str) -> Result<Vec<String>, AnswerError> {
    let mut ids = Vec::new();
    for message in messages(body, "FindItem")? {
        let message = message.map_err(AnswerError::Refused)?;
        let items = message
            .at(&["RootFolder", "Items"])
            .ok_or_else(|| malformed("no RootFolder with Items"))?;
        ids.extend(
            items
                .children
                .iter()
                .filter_map(item_id)
                .map(str::to_string),
        );
    }
    Ok(ids)
}

/// The calendar item a meeting mail (`MeetingRequest`,
/// `MeetingCancellation`, `MeetingResponse`) of a GetItem answer is about,
/// by its `AssociatedCalendarItemId`; `None` for a mail that is no meeting
/// mail, or one gone since it was found.
pub fn associated_event(body: &str) -> Result<Option<String>, AnswerError> {
    let message = match only_message(body, "GetItem") {
        Err(AnswerError::Refused(error)) if error.code == super::ITEM_NOT_FOUND => return Ok(None),
        other => other?,
    };
    let mail = message
        .child("Items")
        .and_then(|items| items.children.first())
        .ok_or_else(|| malformed("a GetItem message without an item"))?;
    Ok(mail
        .child("AssociatedCalendarItemId")
        .and_then(|id| id.attr("Id"))
        .filter(|id| !id.is_empty())
        .map(str::to_string))
}

/// The item kinds a response object saved only may be named as in a
/// CreateItem answer: `MeetingResponse`, the element the spike's fixtures
/// name an answer by (`getitem_meeting_response.xml`, from Sent Items), and
/// `Message`, its base type, both members of the schema's
/// `ArrayOfRealItemsType`. No CreateItem answer to a response was captured.
const RESPONSE_ITEMS: [&str; 2] = ["MeetingResponse", "Message"];

/// The ItemId of the response a `SaveOnly` answer left in Drafts, named as
/// a `MeetingResponse` or a `Message` in the CreateItem answer. `None` when
/// the answer names no such item, as a sent one does; an item of any other
/// kind (the calendar item the answer is about) is never one.
pub fn saved_response_id(body: &str) -> Result<Option<String>, AnswerError> {
    Ok(only_message(body, "CreateItem")?
        .child("Items")
        .and_then(|items| {
            items
                .children
                .iter()
                .filter(|item| RESPONSE_ITEMS.contains(&item.name.as_str()))
                .find_map(item_id)
        })
        .map(str::to_string))
}

/// The ItemId of the calendar item a CreateItem or an UpdateItem saved.
pub fn saved_id(body: &str, operation: &str) -> Result<String, AnswerError> {
    only_message(body, operation)?
        .at(&["Items", "CalendarItem"])
        .and_then(item_id)
        .map(str::to_string)
        .ok_or_else(|| malformed("no saved CalendarItem with an ItemId"))
}

/// The item's `CalendarItemType`, Single when it names none.
pub fn item_type(item: &Element) -> Option<ItemType> {
    ItemType::parse(item.text_of("CalendarItemType").unwrap_or("Single"))
}

/// The item's `ItemId`.
pub fn id_of(item: &Element) -> Option<&str> {
    item_id(item)
}

/// The IANA name of the item's own zone, when the table knows it.
pub fn zone_name(item: &Element) -> Option<String> {
    item_zone(item).map(|tz| tz.name().to_string())
}

// ---------------------------------------------------------------------------
// Recurrence
// ---------------------------------------------------------------------------

/// A date as a recurrence range writes it, with an offset
/// (`2026-10-14+02:00`) or without.
fn range_date(raw: Option<&str>) -> Option<NaiveDate> {
    let raw = raw?.trim();
    NaiveDate::parse_from_str(raw.get(..10)?, "%Y-%m-%d").ok()
}

/// `DaysOfWeek`, a space-separated list, with the group names `Day`,
/// `Weekday` and `WeekendDay` spelled out.
fn weekdays(raw: Option<&str>) -> Option<Vec<Weekday>> {
    let mut days = Vec::new();
    for name in raw?.split_whitespace() {
        let group: &[Weekday] = match name {
            "Day" => &Weekday::ALL,
            "Weekday" => &Weekday::ALL[..5],
            "WeekendDay" => &Weekday::ALL[5..],
            _ => &[Weekday::from_name(name)?],
        };
        for day in group {
            if !days.contains(day) {
                days.push(*day);
            }
        }
    }
    Some(days)
}

fn month_number(raw: Option<&str>) -> Option<u32> {
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
    let raw = raw?;
    MONTHS
        .iter()
        .position(|month| *month == raw)
        .map(|index| index as u32 + 1)
}

/// A series master's `Recurrence` as the model's rule, or `None` for an item
/// without one or a pattern the model cannot hold (a task's regenerating
/// pattern).
pub fn recurrence_of(item: &Element) -> Option<Recurrence> {
    let recurrence = item.child("Recurrence")?;
    let positive = |raw: Option<&str>| raw.and_then(|v| v.parse::<u32>().ok()).filter(|v| *v > 0);
    let (pattern, frequency, relative) = recurrence.children.iter().find_map(|child| {
        let (frequency, relative) = match child.name.as_str() {
            "DailyRecurrence" => (Frequency::Daily, false),
            "WeeklyRecurrence" => (Frequency::Weekly, false),
            "AbsoluteMonthlyRecurrence" => (Frequency::Monthly, false),
            "RelativeMonthlyRecurrence" => (Frequency::Monthly, true),
            "AbsoluteYearlyRecurrence" => (Frequency::Yearly, false),
            "RelativeYearlyRecurrence" => (Frequency::Yearly, true),
            _ => return None,
        };
        Some((child, frequency, relative))
    })?;
    let mut rule = Recurrence {
        frequency,
        interval: positive(pattern.text_of("Interval")).unwrap_or(1),
        weekdays: Vec::new(),
        day_of_month: None,
        week_index: None,
        month: None,
        first_day_of_week: None,
        start: None,
        until: None,
        count: None,
    };
    match frequency {
        Frequency::Daily => {}
        Frequency::Weekly => {
            rule.weekdays = weekdays(pattern.text_of("DaysOfWeek"))?;
            rule.first_day_of_week = pattern
                .text_of("FirstDayOfWeek")
                .and_then(Weekday::from_name);
        }
        Frequency::Monthly | Frequency::Yearly => {
            if relative {
                rule.week_index = Some(WeekIndex::from_name(pattern.text_of("DayOfWeekIndex")?)?);
                rule.weekdays = weekdays(pattern.text_of("DaysOfWeek"))?;
            } else {
                rule.day_of_month = positive(pattern.text_of("DayOfMonth")).filter(|d| *d <= 31);
            }
            if frequency == Frequency::Yearly {
                rule.month = month_number(pattern.text_of("Month"));
            }
        }
    }
    for range in &recurrence.children {
        match range.name.as_str() {
            "NoEndRecurrence" => {}
            "EndDateRecurrence" => rule.until = range_date(range.text_of("EndDate")),
            "NumberedRecurrence" => rule.count = positive(range.text_of("NumberOfOccurrences")),
            _ => continue,
        }
        rule.start = range_date(range.text_of("StartDate"));
    }
    Some(rule)
}
