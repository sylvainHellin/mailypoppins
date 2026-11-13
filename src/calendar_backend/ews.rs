//! The Exchange Web Services calendar backend for on-premises Exchange
//! (PERSO-256, `.agents/plans/calendar.md`, "Backend notes", EWS).
//!
//! TUM's Exchange 2019 accepts NTLM only, so the login is NTLMv2 written in
//! mp ([`ntlm`]). The backend reaches the server through the [`fetch`] seam,
//! one SOAP envelope per call, and reads the answers in [`parse`].
//!
//! The calendars are the user's `IPF.Appointment` folders (FindFolder under
//! `msgfolderroot`), the default one being GetFolder's `calendar`, then the
//! `calendar` folder of each mailbox the user reads as a delegate
//! (`delegate_calendars`, TUM's rooms), reached through a
//! `DistinguishedFolderId` with the mailbox, as the spike read a room's. A
//! delegate calendar is named by its mailbox, kept by its folder id like the
//! user's own, and read-only unless its `EffectiveRights` let the user create
//! items; it syncs and is written through that folder id as the others are.
//!
//! A calendar's window syncs in two parts, since `SyncFolderItems` reports
//! single events and series masters only, never an occurrence or an
//! exception (`.agents/research/2026-11-13-ews-spike.md`):
//!
//! - A baseline pages `SyncFolderItems` from nothing for a fresh
//!   `SyncState`, then reads the window through `FindItem` with a
//!   `CalendarView`, which expands every series into its occurrences and
//!   exceptions, and `GetItem` on what the view named, since the view carries
//!   no attendees and no body. Each series' master is read once through
//!   `RecurringMasterItemId`, for the rule and the master id its occurrences
//!   hang from. The state is read first, so a change made while the window
//!   is read shows at the next sync.
//! - An incremental sync pages `SyncFolderItems` on from the stored state: a
//!   deleted item is removed (a series master takes its occurrences along in
//!   the store), a single event is read again and kept while it overlaps the
//!   window, and a changed series master is read with its occurrences in the
//!   window, which the CalendarView names by the series' `UID`, the series
//!   being re-sent whole ([`SyncResult::resent_series`]). An item keeps its
//!   ItemId when it turns from a single event into a series or back, so a
//!   changed master is also removed as an event and a changed single event
//!   is also a series re-sent with no occurrence.
//!
//! A state the server no longer knows (`ErrorInvalidSyncStateData`) starts a
//! baseline. A view the server cuts at its item limit
//! (`IncludesLastItemInRange` false) is read again in two halves.
//!
//! A refused login stops the sync pass at once
//! ([`CalendarBackendError::LoginRefused`]): the session holds it, so no
//! later call of the pass asks the server again.
//!
//! An answer to an invitation, a proposed time included, a create, an update,
//! a cancel and a delete go through [`write`], which also stamps a create
//! with its transaction id, and the server reminders' dismissal and snooze.
//! Free/busy goes through [`freebusy`]. An online meeting is the one
//! feature [`FEATURES`] leaves out: on-premises Exchange makes no Teams
//! link.

pub mod fetch;
mod freebusy;
pub mod http;
pub mod ntlm;
pub mod parse;
mod write;
pub mod xml;

use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use super::{
    BackendResult, CalendarBackend, CalendarBackendError, Response, SeriesInfo, SyncResult, Written,
};
use crate::calendar_freebusy::{FreeBusyQuery, Schedule};
use crate::calendar_model::{
    Calendar, CalendarFeature, CalendarWindow, Event, EventId, Recurrence,
};
use crate::calendar_write::{EventInput, EventPatch, ProposedTime};
use crate::config::EwsPasswordSource;
use fetch::{EwsFetch, EwsFetchError, EwsSession};
use parse::{AnswerError, Change, ItemType, Message, SyncAnswer, ViewItem};

/// What this build serves on an EWS calendar beyond reading and answering.
pub const FEATURES: &[CalendarFeature] = &[
    CalendarFeature::Write,
    CalendarFeature::ProposeTime,
    CalendarFeature::FreeBusy,
    CalendarFeature::ServerReminders,
];

/// The most changes one SyncFolderItems page may carry, the server's limit.
const MAX_CHANGES: usize = 512;

/// The most SyncFolderItems pages one sync follows: 100 000 changes, far
/// beyond any calendar.
const MAX_PAGES: usize = 200;

/// How many items one GetItem asks for.
const GET_ITEM_BATCH: usize = 50;

/// The shortest span a cut CalendarView is split down to before the sync
/// gives up on the window.
const MIN_VIEW_SPAN: Duration = Duration::hours(1);

/// The path [`SyncResult::path`] records.
const PATH: &str = "sync-folder-items";

/// The code of an item gone between the listing and the read.
const ITEM_NOT_FOUND: &str = "ErrorItemNotFound";

/// The token this backend hands the engine, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EwsToken {
    sync_state: String,
}

impl EwsToken {
    fn encode(&self) -> String {
        serde_json::to_string(self).expect("an EwsToken always serialises")
    }

    /// A token this build cannot read is no token: the sync is a baseline.
    fn decode(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }
}

/// One item a GetItem names: an item by its id, or the series master of an
/// occurrence.
enum ItemRef<'a> {
    Item(&'a str),
    MasterOf(&'a str),
}

fn folder_id(id: &str) -> String {
    format!("<t:FolderId Id=\"{}\"/>", xml::escape(id))
}

fn utc_text(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The changes of a sync, the last word on each id winning, in the order the
/// ids first came.
fn last_word(changes: Vec<Change>) -> Vec<Change> {
    let mut order: Vec<String> = Vec::new();
    let mut last: HashMap<String, Change> = HashMap::new();
    for change in changes {
        let id = match &change {
            Change::Upsert { id, .. } | Change::Delete { id } => id.clone(),
        };
        if last.insert(id.clone(), change).is_none() {
            order.push(id);
        }
    }
    order
        .into_iter()
        .filter_map(|id| last.remove(&id))
        .collect()
}

/// One EWS account's calendar.
pub struct EwsCalendarBackend<F> {
    account: String,
    /// Which stored password the login uses, for the sentence of a refusal.
    password_from: EwsPasswordSource,
    /// The mailboxes whose calendar the user reads as a delegate
    /// (`delegate_calendars` of `[accounts.ews]`).
    delegates: Vec<String>,
    session: EwsSession<F>,
}

impl<F: EwsFetch> EwsCalendarBackend<F> {
    /// The backend of `account` over any [`EwsFetch`], the seam the tests
    /// use and the transport implements.
    pub fn with_fetch(account: &str, password_from: EwsPasswordSource, fetch: F) -> Self {
        EwsCalendarBackend {
            account: account.to_string(),
            password_from,
            delegates: Vec::new(),
            session: EwsSession::new(fetch),
        }
    }

    /// The backend also listing the calendar of each mailbox of
    /// `mailboxes`, which the user reads as a delegate.
    pub fn with_delegate_calendars(mut self, mailboxes: Vec<String>) -> Self {
        self.delegates = mailboxes;
        self
    }

    /// The calendar of each delegate mailbox, in one GetFolder through each
    /// mailbox's `calendar` folder; a mailbox the server refuses (gone, or
    /// no longer shared) is left out with a log line while the others stand.
    async fn delegate_calendars(&self, shape: &str) -> BackendResult<Vec<Calendar>> {
        if self.delegates.is_empty() {
            return Ok(Vec::new());
        }
        let ids: String = self
            .delegates
            .iter()
            .map(|address| {
                format!(
                    "<t:DistinguishedFolderId Id=\"calendar\"><t:Mailbox>\
                     <t:EmailAddress>{}</t:EmailAddress></t:Mailbox></t:DistinguishedFolderId>",
                    xml::escape(address)
                )
            })
            .collect();
        let body = format!("<m:GetFolder>{shape}<m:FolderIds>{ids}</m:FolderIds></m:GetFolder>");
        let answer = self.call("GetFolder", &body).await?;
        let folders =
            parse::folder_messages(&answer).map_err(|error| answer_error(&error, "GetFolder"))?;
        if folders.len() != self.delegates.len() {
            return Err(anyhow!(
                "unreadable EWS answer to GetFolder: {} folders for {} mailboxes asked",
                folders.len(),
                self.delegates.len()
            )
            .into());
        }
        let mut calendars = Vec::new();
        for (address, folder) in self.delegates.iter().zip(folders) {
            match folder {
                Ok(folder) => {
                    calendars.push(parse::delegate_calendar_of(&self.account, address, &folder))
                }
                Err(error) => warn!(
                    "[calendar] {}: leaving out the calendar of {address}: {error}",
                    self.account
                ),
            }
        }
        Ok(calendars)
    }

    /// The seam, for a test that reads what was sent.
    #[cfg(test)]
    fn fetch(&self) -> &F {
        self.session.fetch()
    }

    /// A seam failure of `operation` as the error a client shows.
    fn seam_error(&self, error: EwsFetchError, operation: &str) -> CalendarBackendError {
        match error {
            EwsFetchError::LoginRefused(_) => CalendarBackendError::LoginRefused(format!(
                "{error}; set the password again with `{}`",
                mp_core::mp_command::here(&[
                    "config",
                    "set-password",
                    self.password_from.as_str(),
                    "--account",
                    &self.account,
                ])
            )),
            EwsFetchError::Status { status, .. } => CalendarBackendError::Other(anyhow!(
                "the Exchange server answered {operation} with HTTP {status}"
            )),
            EwsFetchError::Transport(_) => {
                CalendarBackendError::Other(anyhow!("{operation}: {error}"))
            }
        }
    }

    /// Send `operation`'s `body` and answer the server's answer.
    async fn call(&self, operation: &str, body: &str) -> BackendResult<String> {
        self.session
            .call(body)
            .await
            .map_err(|error| self.seam_error(error, operation))
    }

    /// The calendar folders `operation` (`GetFolder`, `FindFolder`) answers
    /// for `body`.
    async fn folders(&self, operation: &str, body: &str) -> BackendResult<Vec<parse::Folder>> {
        let answer = self.call(operation, body).await?;
        parse::folders(&answer, operation).map_err(|error| answer_error(&error, operation))
    }

    /// Every change of `folder` after `state` (from nothing when `None`),
    /// page after page, with the state the last page ends on; `None` when
    /// the server no longer knows `state`.
    async fn changes(
        &self,
        folder: &str,
        mut state: Option<String>,
    ) -> BackendResult<Option<(String, Vec<Change>)>> {
        let mut changes = Vec::new();
        for _ in 0..MAX_PAGES {
            let sync_state = state
                .as_deref()
                .map(|state| format!("<m:SyncState>{}</m:SyncState>", xml::escape(state)))
                .unwrap_or_default();
            let body = format!(
                "<m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape>\
                 <t:AdditionalProperties><t:FieldURI FieldURI=\"calendar:CalendarItemType\"/>\
                 </t:AdditionalProperties></m:ItemShape>\
                 <m:SyncFolderId>{}</m:SyncFolderId>{sync_state}\
                 <m:MaxChangesReturned>{MAX_CHANGES}</m:MaxChangesReturned></m:SyncFolderItems>",
                folder_id(folder)
            );
            let answer = self.call("SyncFolderItems", &body).await?;
            match parse::sync_page(&answer)
                .map_err(|error| answer_error(&error, "SyncFolderItems"))?
            {
                SyncAnswer::InvalidState => return Ok(None),
                SyncAnswer::Page(page) => {
                    changes.extend(page.changes);
                    if page.last {
                        return Ok(Some((page.sync_state, last_word(changes))));
                    }
                    state = Some(page.sync_state);
                }
            }
        }
        Err(anyhow!("SyncFolderItems ran past {MAX_PAGES} pages").into())
    }

    /// The items of `folder` in `window`, series expanded, each once. A view
    /// the server cuts at its limit is read again in halves.
    async fn view(&self, folder: &str, window: CalendarWindow) -> BackendResult<Vec<ViewItem>> {
        let mut spans = vec![(window.start, window.end)];
        let mut items: Vec<ViewItem> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some((start, end)) = spans.pop() {
            let body = format!(
                "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape>\
                 <t:AdditionalProperties><t:FieldURI FieldURI=\"calendar:CalendarItemType\"/>\
                 <t:FieldURI FieldURI=\"calendar:UID\"/><t:FieldURI FieldURI=\"calendar:Start\"/>\
                 <t:FieldURI FieldURI=\"calendar:End\"/></t:AdditionalProperties></m:ItemShape>\
                 <m:CalendarView StartDate=\"{}\" EndDate=\"{}\"/>\
                 <m:ParentFolderIds>{}</m:ParentFolderIds></m:FindItem>",
                utc_text(start),
                utc_text(end),
                folder_id(folder)
            );
            let answer = self.call("FindItem", &body).await?;
            let view =
                parse::calendar_view(&answer).map_err(|error| answer_error(&error, "FindItem"))?;
            if !view.complete {
                if end - start < MIN_VIEW_SPAN * 2 {
                    return Err(anyhow!(
                        "the Exchange server cut its calendar view of {} to {} short",
                        utc_text(start),
                        utc_text(end)
                    )
                    .into());
                }
                let middle = start + (end - start) / 2;
                spans.push((middle, end));
                spans.push((start, middle));
                continue;
            }
            for item in view.items {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
            }
        }
        Ok(items)
    }

    /// The GetItem answer for each of `refs`, in order, batch after batch.
    async fn get_items(&self, refs: &[ItemRef<'_>]) -> BackendResult<Vec<Message>> {
        let mut answers = Vec::with_capacity(refs.len());
        for batch in refs.chunks(GET_ITEM_BATCH) {
            let ids: String = batch
                .iter()
                .map(|item| match item {
                    ItemRef::Item(id) => format!("<t:ItemId Id=\"{}\"/>", xml::escape(id)),
                    ItemRef::MasterOf(id) => format!(
                        "<t:RecurringMasterItemId OccurrenceId=\"{}\"/>",
                        xml::escape(id)
                    ),
                })
                .collect();
            let body = format!(
                "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape>\
                 <t:BodyType>Text</t:BodyType><t:AdditionalProperties>{}\
                 </t:AdditionalProperties></m:ItemShape><m:ItemIds>{ids}</m:ItemIds></m:GetItem>",
                write::transaction_field()
            );
            let answer = self.call("GetItem", &body).await?;
            let messages =
                parse::items(&answer).map_err(|error| answer_error(&error, "GetItem"))?;
            if messages.len() != batch.len() {
                return Err(anyhow!(
                    "unreadable EWS answer to GetItem: {} items for {} asked",
                    messages.len(),
                    batch.len()
                )
                .into());
            }
            answers.extend(messages);
        }
        Ok(answers)
    }

    /// `item` of the calendar `calendar` (its folder id) converted, or `None`
    /// with a log line when it cannot be: one unreadable item does not fail
    /// its calendar.
    fn event_of(&self, calendar: &str, item: &xml::Element) -> Option<Event> {
        match parse::calendar_event(&self.account, calendar, item) {
            Ok(event) => Some(event),
            Err(why) => {
                warn!(
                    "[calendar] {}: skipping EWS item {} of calendar {calendar}: {why}",
                    self.account,
                    parse::id_of(item).unwrap_or("without an id"),
                );
                None
            }
        }
    }

    /// The series a master item of `calendar` describes.
    fn series_of(&self, calendar: &str, master: &xml::Element) -> Option<SeriesInfo> {
        let event = self.event_of(calendar, master)?;
        Some(SeriesInfo {
            master_id: event.id.clone(),
            uid: event.uid.clone(),
            recurrence: parse::recurrence_of(master),
            zone: parse::zone_name(master),
            master: Some(Box::new(event)),
        })
    }

    /// The series masters of the occurrences and exceptions among `events`,
    /// one GetItem per series, which then hang each occurrence from its
    /// master and carry the rule in words. A master the server does not
    /// answer leaves its occurrences as they are.
    async fn resolve_series(
        &self,
        calendar: &str,
        events: &mut [Event],
    ) -> BackendResult<Vec<SeriesInfo>> {
        let mut asked: Vec<(String, String)> = Vec::new();
        for event in events.iter().filter(|event| event.kind.is_instance()) {
            if !asked.iter().any(|(uid, _)| *uid == event.uid) {
                asked.push((event.uid.clone(), event.id.clone()));
            }
        }
        let refs: Vec<ItemRef<'_>> = asked
            .iter()
            .map(|(_, occurrence)| ItemRef::MasterOf(occurrence))
            .collect();
        let answers = self.get_items(&refs).await?;
        let mut series = Vec::new();
        for ((uid, _), answer) in asked.iter().zip(answers) {
            match answer {
                Ok(master) => series.extend(self.series_of(calendar, &master)),
                Err(error) => warn!(
                    "[calendar] {}: no series master for {uid} in calendar {calendar}: {error}",
                    self.account
                ),
            }
        }
        attach_series(events, &series);
        Ok(series)
    }

    /// A fresh baseline of `calendar` over `window`.
    async fn baseline(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
    ) -> BackendResult<SyncResult> {
        let Some((state, _)) = self.changes(&calendar.id, None).await? else {
            return Err(anyhow!(
                "the Exchange server refused a SyncFolderItems from nothing as an invalid state"
            )
            .into());
        };
        let view = self.view(&calendar.id, window).await?;
        let refs: Vec<ItemRef<'_>> = view.iter().map(|item| ItemRef::Item(&item.id)).collect();
        let answers = self.get_items(&refs).await?;
        let mut events = Vec::new();
        for (item, answer) in view.iter().zip(answers) {
            match answer {
                Ok(found) => events.extend(self.event_of(&calendar.id, &found)),
                Err(error) if error.code == ITEM_NOT_FOUND => {
                    debug!(
                        "[calendar] {}: item {} left {} while it was read",
                        self.account, item.id, calendar.name
                    );
                }
                Err(error) => {
                    return Err(anyhow!("the Exchange server refused GetItem: {error}").into())
                }
            }
        }
        let series = self.resolve_series(&calendar.id, &mut events).await?;
        Ok(SyncResult {
            upserts: events,
            token: Some(EwsToken { sync_state: state }.encode()),
            full_resync: true,
            path: Some(PATH.to_string()),
            series,
            ..Default::default()
        })
    }

    /// What `changes` since the last sync mean for `calendar`'s `window`.
    async fn apply_changes(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        changes: Vec<Change>,
        state: String,
    ) -> BackendResult<SyncResult> {
        let mut result = SyncResult {
            token: Some(EwsToken { sync_state: state }.encode()),
            path: Some(PATH.to_string()),
            ..Default::default()
        };
        let mut changed: Vec<&str> = Vec::new();
        for change in &changes {
            match change {
                Change::Delete { id } => result.removed.push(id.clone()),
                Change::Upsert { id, .. } => changed.push(id),
            }
        }
        let refs: Vec<ItemRef<'_>> = changed.iter().map(|id| ItemRef::Item(id)).collect();
        let answers = self.get_items(&refs).await?;
        let mut masters = Vec::new();
        for (id, answer) in changed.iter().zip(answers) {
            let item = match answer {
                Ok(item) => item,
                Err(error) if error.code == ITEM_NOT_FOUND => {
                    result.removed.push(id.to_string());
                    continue;
                }
                Err(error) => {
                    return Err(anyhow!("the Exchange server refused GetItem: {error}").into())
                }
            };
            // An item keeps its ItemId when it turns from a single event into
            // a series or back, so each kind clears what the other left: a
            // master is removed as an event (the store deletes removals
            // before it writes the upserts and the series), and a single
            // event is a series re-sent with no occurrence.
            if parse::item_type(&item) == Some(ItemType::RecurringMaster) {
                if let Some(series) = self.series_of(&calendar.id, &item) {
                    result.removed.push(series.master_id.clone());
                    masters.push(series);
                }
                continue;
            }
            match self.event_of(&calendar.id, &item) {
                Some(event) if event.overlaps(&window, chrono_tz::UTC) => {
                    result.resent_series.push(event.id.clone());
                    result.upserts.push(event)
                }
                Some(event) => result.removed.push(event.id),
                None => {}
            }
        }
        if !masters.is_empty() {
            let uids: HashSet<&str> = masters.iter().map(|series| series.uid.as_str()).collect();
            let view = self.view(&calendar.id, window).await?;
            let occurrences: Vec<&ViewItem> = view
                .iter()
                .filter(|item| matches!(item.kind, ItemType::Occurrence | ItemType::Exception))
                .filter(|item| item.uid.as_deref().is_some_and(|uid| uids.contains(uid)))
                .collect();
            let refs: Vec<ItemRef<'_>> = occurrences
                .iter()
                .map(|item| ItemRef::Item(&item.id))
                .collect();
            let answers = self.get_items(&refs).await?;
            let mut events = Vec::new();
            for answer in answers {
                match answer {
                    Ok(item) => events.extend(self.event_of(&calendar.id, &item)),
                    Err(error) if error.code == ITEM_NOT_FOUND => {}
                    Err(error) => {
                        return Err(anyhow!("the Exchange server refused GetItem: {error}").into())
                    }
                }
            }
            attach_series(&mut events, &masters);
            result.upserts.extend(events);
            result
                .resent_series
                .extend(masters.iter().map(|series| series.master_id.clone()));
            result.series = masters;
        }
        Ok(result)
    }
}

/// Hang each occurrence and exception among `events` from its series in
/// `series`, by `UID`, and give it the rule in words.
fn attach_series(events: &mut [Event], series: &[SeriesInfo]) {
    for event in events.iter_mut().filter(|event| event.kind.is_instance()) {
        if let Some(info) = series.iter().find(|info| info.uid == event.uid) {
            event.series_master_id = Some(info.master_id.clone());
            if event.recurrence.is_none() {
                event.recurrence = info.recurrence.as_ref().map(Recurrence::describe);
            }
        }
    }
}

/// An answer that gave nothing to read, as the error a client shows: one
/// sentence, which the daemon passes on as it is.
fn answer_error(error: &AnswerError, operation: &str) -> CalendarBackendError {
    CalendarBackendError::Other(anyhow!(error.sentence(operation)))
}

impl<F: EwsFetch> CalendarBackend for EwsCalendarBackend<F> {
    /// The default calendar (GetFolder `calendar`) first, then every other
    /// `IPF.Appointment` folder FindFolder finds under the mailbox root, then
    /// the calendar of each delegate mailbox.
    async fn list_calendars(&self) -> BackendResult<Vec<Calendar>> {
        let shape = "<m:FolderShape><t:BaseShape>Default</t:BaseShape><t:AdditionalProperties>\
                     <t:FieldURI FieldURI=\"folder:EffectiveRights\"/></t:AdditionalProperties>\
                     </m:FolderShape>";
        let default = self
            .folders(
                "GetFolder",
                &format!(
                    "<m:GetFolder>{shape}<m:FolderIds>\
                     <t:DistinguishedFolderId Id=\"calendar\"/></m:FolderIds></m:GetFolder>"
                ),
            )
            .await?;
        let default_id = default.first().map(|folder| folder.id.clone());
        let found = self
            .folders(
                "FindFolder",
                &format!(
                    "<m:FindFolder Traversal=\"Deep\">{shape}<m:Restriction><t:IsEqualTo>\
                     <t:FieldURI FieldURI=\"folder:FolderClass\"/><t:FieldURIOrConstant>\
                     <t:Constant Value=\"IPF.Appointment\"/></t:FieldURIOrConstant></t:IsEqualTo>\
                     </m:Restriction><m:ParentFolderIds>\
                     <t:DistinguishedFolderId Id=\"msgfolderroot\"/></m:ParentFolderIds></m:FindFolder>"
                ),
            )
            .await?;
        let mut calendars: Vec<Calendar> = Vec::new();
        for folder in default.iter().chain(&found) {
            if !calendars.iter().any(|calendar| calendar.id == folder.id) {
                calendars.push(parse::calendar_of(
                    &self.account,
                    folder,
                    default_id.as_deref(),
                ));
            }
        }
        for delegate in self.delegate_calendars(shape).await? {
            if !calendars.iter().any(|calendar| calendar.id == delegate.id) {
                calendars.push(delegate);
            }
        }
        Ok(calendars)
    }

    async fn sync_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> BackendResult<SyncResult> {
        let Some(token) = token.as_deref().and_then(EwsToken::decode) else {
            return self.baseline(calendar, window).await;
        };
        match self.changes(&calendar.id, Some(token.sync_state)).await? {
            Some((state, changes)) => self.apply_changes(calendar, window, changes, state).await,
            None => {
                info!(
                    "[calendar] {}: the Exchange server no longer knows the sync state of {}; \
                     syncing the window afresh",
                    self.account, calendar.name
                );
                self.baseline(calendar, window).await
            }
        }
    }

    async fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
        proposed: Option<ProposedTime>,
    ) -> BackendResult<()> {
        self.send_response(
            event,
            response,
            comment.as_deref(),
            send_response,
            proposed.as_ref(),
        )
        .await
    }

    async fn create_event(
        &self,
        calendar: &Calendar,
        input: &EventInput,
        transaction_id: &str,
    ) -> BackendResult<Written> {
        self.create_item(calendar, input, transaction_id).await
    }

    async fn update_event(
        &self,
        calendar: &str,
        event: &str,
        patch: &EventPatch,
    ) -> BackendResult<Written> {
        self.update_item(calendar, event, patch).await
    }

    async fn cancel_event(&self, event: &str, comment: Option<String>) -> BackendResult<()> {
        self.cancel_item(event, comment.as_deref()).await
    }

    async fn delete_event(&self, event: &str) -> BackendResult<()> {
        self.delete_item(event).await
    }

    async fn dismiss_reminder(&self, event: &str) -> BackendResult<()> {
        self.reminder_action(event, None).await
    }

    async fn snooze_reminder(&self, event: &str, until: DateTime<Utc>) -> BackendResult<()> {
        self.reminder_action(event, Some(until)).await
    }

    async fn free_busy(&self, query: &FreeBusyQuery) -> BackendResult<Vec<Schedule>> {
        self.availability(query).await
    }

    /// The calendar item the meeting mail `message_id` is about. TUM's mail
    /// goes over IMAP, so the mail is named by its Message-ID (angle
    /// brackets optional) rather than by an EWS ItemId: FindItem finds it in
    /// the Inbox or in Deleted Items, where Outlook moves an answered
    /// invitation, and GetItem reads its `AssociatedCalendarItemId`. The
    /// restriction on `message:InternetMessageId` is built from the schema
    /// documentation and unverified live; the GetItem is the spike's.
    /// `calendar_sync::invite_event` asks under a row's Graph id only, so an
    /// EWS account reaches this once it passes the row's Message-ID.
    async fn event_for_message(&self, message_id: &str) -> BackendResult<Option<EventId>> {
        let bare = message_id
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if bare.is_empty() {
            return Ok(None);
        }
        let body = format!(
            "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape>\
             </m:ItemShape><m:Restriction><t:IsEqualTo>\
             <t:FieldURI FieldURI=\"message:InternetMessageId\"/><t:FieldURIOrConstant>\
             <t:Constant Value=\"{}\"/></t:FieldURIOrConstant></t:IsEqualTo></m:Restriction>\
             <m:ParentFolderIds><t:DistinguishedFolderId Id=\"inbox\"/>\
             <t:DistinguishedFolderId Id=\"deleteditems\"/></m:ParentFolderIds></m:FindItem>",
            xml::escape(&format!("<{bare}>"))
        );
        let answer = self.call("FindItem", &body).await?;
        let found = parse::found_ids(&answer).map_err(|error| answer_error(&error, "FindItem"))?;
        let Some(mail) = found.first() else {
            return Ok(None);
        };
        let body = format!(
            "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape></m:ItemShape>\
             <m:ItemIds><t:ItemId Id=\"{}\"/></m:ItemIds></m:GetItem>",
            xml::escape(mail)
        );
        let answer = self.call("GetItem", &body).await?;
        parse::associated_event(&answer).map_err(|error| answer_error(&error, "GetItem"))
    }
}

#[cfg(test)]
mod tests;
